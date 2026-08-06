//! Audio playback through GStreamer's `playbin`.
//!
//! # Design
//!
//! [`Player`] wraps a single `playbin` element and presents a small,
//! playback-oriented API (play / pause / stop / seek / position). GTK and
//! GStreamer never talk to each other directly:
//!
//! * The UI calls methods on [`Player`].
//! * `playbin` is driven purely by file **URIs**, so the player knows
//!   nothing about the database or the file system beyond the URI it is
//!   handed.
//! * Asynchronous conditions (end of stream, errors) are forwarded over a
//!   GLib channel carried by [`PlayerEvent`]. The window subscribes to it,
//!   so all player callbacks run on the GTK main loop.
//!
//! `playbin` selects an audio sink automatically, which keeps this module
//! free of hardware-specific configuration.
//!
//! If GStreamer's `playbin` factory cannot be created (a very rare
//! environment failure), [`Player::disabled`] provides a non-functional
//! player so the rest of the app still works.

use std::sync::{Arc, Mutex};

use futures_channel::mpsc::UnboundedSender;
use gstreamer::prelude::*;

/// The high-level playback state the UI reflects in its controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// Nothing is loaded, or playback was stopped.
    Stopped,
    /// Audio is currently being played back.
    Playing,
    /// Audio is paused at its current position.
    Paused,
}

/// Asynchronous events a GStreamer pipeline produces.
#[derive(Debug)]
pub enum PlayerEvent {
    /// Playback reached the end of the stream.
    EndOfStream,
    /// Playback failed; the string contains a human-readable reason.
    Error(String),
}

/// A lightweight `playbin` wrapper.
///
/// The inner element is optional: it is `None` only for the disabled
/// fallback created by [`Player::disabled`], in which case every method is
/// a no-op.
#[derive(Clone)]
pub struct Player {
    playbin: Option<gstreamer::Element>,
    state: Arc<Mutex<PlaybackState>>,
}

const ONE_SECOND_NS: u64 = 1_000_000_000;

impl Player {
    fn set_state(&self, new_state: PlaybackState) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *state = new_state;
    }

    fn get_state(&self) -> PlaybackState {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *state
    }

    /// Create a player and start listening on its bus.
    ///
    /// The returned `Player` is not playing anything yet.
    pub fn new(events: UnboundedSender<PlayerEvent>) -> Result<Self, PlayerError> {
        let playbin = gstreamer::ElementFactory::make("playbin").build()?;
        let state = Arc::new(Mutex::new(PlaybackState::Stopped));

        let bus = playbin
            .bus()
            .ok_or_else(|| PlayerError::Init("playbin has no bus".into()))?;
        let state_clone = Arc::clone(&state);
        bus.connect_message(None, move |_bus, message| match message.type_() {
            gstreamer::MessageType::Eos => {
                *state_clone.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    PlaybackState::Stopped;
                let _ = events.unbounded_send(PlayerEvent::EndOfStream);
            }
            gstreamer::MessageType::Error => {
                if let gstreamer::MessageView::Error(error) = message.view() {
                    *state_clone.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                        PlaybackState::Stopped;
                    let _ = events
                        .unbounded_send(PlayerEvent::Error(error.error().to_string()));
                }
            }
            _ => {}
        });

        Ok(Self {
            playbin: Some(playbin),
            state,
        })
    }

    /// A player that accepts calls but cannot play anything. Used when
    /// GStreamer failed to initialize so the app can still run and show a
    /// meaningful error.
    pub fn disabled() -> Self {
        Self {
            playbin: None,
            state: Arc::new(Mutex::new(PlaybackState::Stopped)),
        }
    }

    /// The current playback state.
    pub fn state(&self) -> PlaybackState {
        self.get_state()
    }

    /// Load a URI and start playing it immediately.
    pub fn play_uri(&self, uri: &str) {
        let Some(playbin) = &self.playbin else {
            return;
        };
        playbin.set_property("uri", uri.to_string());
        if playbin.set_state(gstreamer::State::Playing).is_ok() {
            self.set_state(PlaybackState::Playing);
        }
    }

    /// Resume playback if paused, pause it if playing, otherwise start over
    /// from the beginning.
    pub fn toggle_play_pause(&self) {
        let Some(playbin) = &self.playbin else {
            return;
        };
        let current = self.get_state();
        let target = match current {
            PlaybackState::Playing => gstreamer::State::Paused,
            // After a stop or an end-of-stream the stream is positioned at
            // its end, so rewind before replaying.
            PlaybackState::Paused | PlaybackState::Stopped => gstreamer::State::Playing,
        };
        if target == gstreamer::State::Playing && current == PlaybackState::Stopped {
            let _ = playbin.set_state(gstreamer::State::Ready);
            let _ = playbin.seek_simple(
                gstreamer::SeekFlags::FLUSH,
                gstreamer::ClockTime::from_nseconds(0),
            );
        }
        if playbin.set_state(target).is_ok() {
            self.set_state(match target {
                gstreamer::State::Playing => PlaybackState::Playing,
                gstreamer::State::Paused => PlaybackState::Paused,
                _ => PlaybackState::Stopped,
            });
        }
    }

    /// Stop playback and rewind to the start of the stream.
    pub fn stop(&self) {
        let Some(playbin) = &self.playbin else {
            return;
        };
        if playbin.set_state(gstreamer::State::Ready).is_ok() {
            self.set_state(PlaybackState::Stopped);
        }
    }

    /// Jump to a position given in seconds.
    pub fn seek(&self, position_seconds: u64) {
        let Some(playbin) = &self.playbin else {
            return;
        };
        let target = gstreamer::ClockTime::from_nseconds(
            position_seconds.saturating_mul(ONE_SECOND_NS),
        );
        let _ = playbin.seek_simple(gstreamer::SeekFlags::FLUSH, target);
    }

    /// Current position in seconds (0 when unknown).
    pub fn position(&self) -> u64 {
        let Some(playbin) = &self.playbin else {
            return 0;
        };
        playbin
            .query_position::<gstreamer::ClockTime>()
            .map(|value| value.nseconds() / ONE_SECOND_NS)
            .unwrap_or(0)
    }

    /// Total duration in seconds (0 when unknown, e.g. for streams).
    pub fn duration(&self) -> u64 {
        let Some(playbin) = &self.playbin else {
            return 0;
        };
        playbin
            .query_duration::<gstreamer::ClockTime>()
            .map(|value| value.nseconds() / ONE_SECOND_NS)
            .unwrap_or(0)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Release the audio sink cleanly when the window closes.
        if let Some(playbin) = &self.playbin {
            let _ = playbin.set_state(gstreamer::State::Null);
        }
    }
}

/// Errors that can happen when creating a player.
#[derive(Debug)]
pub enum PlayerError {
    /// GStreamer could not instantiate `playbin`.
    Init(String),
    /// The `playbin` element could not be created.
    Factory(glib::BoolError),
}

impl std::fmt::Display for PlayerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlayerError::Init(reason) => write!(formatter, "{reason}"),
            PlayerError::Factory(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for PlayerError {}

impl From<glib::BoolError> for PlayerError {
    fn from(error: glib::BoolError) -> Self {
        PlayerError::Factory(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_gstreamer() {
        gstreamer::init().expect("GStreamer should initialize");
    }

    fn new_test_player() -> Player {
        init_gstreamer();
        let (sender, _receiver) = futures_channel::mpsc::unbounded();
        Player::new(sender).expect("player should initialize")
    }

    #[test]
    fn player_starts_in_stopped_state() {
        assert_eq!(new_test_player().state(), PlaybackState::Stopped);
    }

    #[test]
    fn player_can_be_stopped_without_playing() {
        let player = new_test_player();
        player.stop();
        assert_eq!(player.state(), PlaybackState::Stopped);
    }

    #[test]
    fn unknown_duration_reports_zero() {
        let player = new_test_player();
        assert_eq!(player.duration(), 0);
        assert_eq!(player.position(), 0);
    }

    #[test]
    fn disabled_player_never_leaves_stopped() {
        let player = Player::disabled();
        player.play_uri("file:///nonexistent.mp3");
        player.toggle_play_pause();
        player.seek(30);
        assert_eq!(player.state(), PlaybackState::Stopped);
        assert_eq!(player.position(), 0);
        assert_eq!(player.duration(), 0);
    }
}