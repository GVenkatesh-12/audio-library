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
    /// Keeps the bus watch installed; dropping it removes the watch and
    /// would silently break end-of-stream / error delivery.
    _bus_watch: Option<Arc<gstreamer::bus::BusWatchGuard>>,
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
        let playbin_clone = playbin.clone();
        // A bus *watch* must be installed for messages to be dispatched at
        // all: `connect_message` only wires up the `message` signal, which
        // GStreamer emits solely when a watch source pops messages from the
        // bus. Without `add_watch` the pipeline posts messages that never
        // reach this callback (end-of-stream, errors).
        let watch = bus
            .add_watch(move |_bus, message| {
                // Park the pipeline in `READY` when the stream ends or
                // fails. GStreamer leaves `playbin` in `PLAYING` after an
                // end-of-stream, so a later `play_uri`/toggle would be a
                // skipped PLAYING→PLAYING transition and resume at the end
                // of the file instead of the beginning.
                let park = || playbin_clone.set_state(gstreamer::State::Ready);
                match message.type_() {
                    gstreamer::MessageType::Eos => {
                        *state_clone.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                            PlaybackState::Stopped;
                        let _ = park();
                        let _ = events.unbounded_send(PlayerEvent::EndOfStream);
                    }
                    gstreamer::MessageType::Error => {
                        if let gstreamer::MessageView::Error(error) = message.view() {
                            *state_clone.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                PlaybackState::Stopped;
                            let _ = park();
                            let _ = events
                                .unbounded_send(PlayerEvent::Error(error.error().to_string()));
                        }
                    }
                    _ => {}
                }
                glib::ControlFlow::Continue
            })
            .map_err(|error| PlayerError::Init(format!("could not watch the bus: {error}")))?;

        Ok(Self {
            playbin: Some(playbin),
            state,
            _bus_watch: Some(Arc::new(watch)),
        })
    }

    /// A player that accepts calls but cannot play anything. Used when
    /// GStreamer failed to initialize so the app can still run and show a
    /// meaningful error.
    pub fn disabled() -> Self {
        Self {
            playbin: None,
            state: Arc::new(Mutex::new(PlaybackState::Stopped)),
            _bus_watch: None,
        }
    }

    /// The current playback state.
    pub fn state(&self) -> PlaybackState {
        self.get_state()
    }

    /// Load a URI and start playing it immediately.
    ///
    /// The pipeline is parked in `READY` first: when another track is still
    /// playing, `playbin` would otherwise skip the `PLAYING → PLAYING`
    /// transition and keep streaming the old file while only the `uri`
    /// property changes.
    pub fn play_uri(&self, uri: &str) {
        let Some(playbin) = &self.playbin else {
            return;
        };
        let _ = playbin.set_state(gstreamer::State::Ready);
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

    /// Write a minimal mono 16-bit PCM WAV file (a `seconds`-long tone) into
    /// `temp_dir`.
    fn write_test_wav(name: &str, seconds: u32) -> std::path::PathBuf {
        let wav = std::env::temp_dir().join(name);
        let samples_per_second = 8000u32;
        let samples = (samples_per_second * seconds) as usize;
        let mut data = Vec::with_capacity(samples * 2);
        for i in 0..samples {
            let value = (8000f32
                * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / samples_per_second as f32).sin())
                as i16;
            data.extend_from_slice(&value.to_le_bytes());
        }
        // Minimal mono 16-bit PCM WAV file.
        let mut header = Vec::new();
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        header.extend_from_slice(b"WAVE");
        header.extend_from_slice(b"fmt ");
        header.extend_from_slice(&16u32.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes()); // PCM
        header.extend_from_slice(&1u16.to_le_bytes()); // mono
        header.extend_from_slice(&samples_per_second.to_le_bytes());
        header.extend_from_slice(&(samples_per_second * 2).to_le_bytes()); // byte rate
        header.extend_from_slice(&2u16.to_le_bytes()); // block align
        header.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        header.extend_from_slice(b"data");
        header.extend_from_slice(&(data.len() as u32).to_le_bytes());
        std::fs::write(&wav, header.into_iter().chain(data).collect::<Vec<_>>())
            .expect("write test wav");
        wav
    }

    /// Cycle the GLib main context until `condition` holds or `timeout`
    /// elapses, draining player events into `events`.
    fn pump_until(
        context: &glib::MainContext,
        receiver: &mut futures_channel::mpsc::UnboundedReceiver<PlayerEvent>,
        events: &mut Vec<PlayerEvent>,
        timeout: std::time::Duration,
        condition: impl FnMut(&[PlayerEvent]) -> bool,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut condition = condition;
        while std::time::Instant::now() < deadline {
            while let Ok(event) = receiver.try_recv() {
                events.push(event);
            }
            if condition(events) {
                return true;
            }
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        condition(events)
    }

    #[test]
    fn end_of_stream_returns_to_stopped_and_emits_event() {
        init_gstreamer();
        let wav = write_test_wav("audio-library-test-eos.wav", 3);
        let uri = format!("file://{}", wav.display());

        let (sender, mut receiver) = futures_channel::mpsc::unbounded();
        let player = Player::new(sender).expect("player should initialize");
        player.play_uri(&uri);
        assert_eq!(player.state(), PlaybackState::Playing);

        let context = glib::MainContext::default();
        let mut events = Vec::new();
        let done = pump_until(
            &context,
            &mut receiver,
            &mut events,
            std::time::Duration::from_secs(15),
            |events| {
                matches!(events.last(), Some(PlayerEvent::EndOfStream))
                    && player.state() == PlaybackState::Stopped
            },
        );
        assert!(done, "EndOfStream event was never emitted");
        assert_eq!(
            player.state(),
            PlaybackState::Stopped,
            "player must clear its state when the stream ends"
        );
    }

    #[test]
    fn replay_after_end_of_stream_starts_from_the_beginning() {
        init_gstreamer();
        let wav = write_test_wav("test-eos-replay.wav", 3);
        let other_wav = write_test_wav("test-eos-replay-other.wav", 3);
        let uri = format!("file://{}", wav.display());
        let other_uri = format!("file://{}", other_wav.display());

        let (sender, mut receiver) = futures_channel::mpsc::unbounded();
        let player = Player::new(sender).expect("player should initialize");
        let context = glib::MainContext::default();

        // First play runs to completion.
        player.play_uri(&uri);
        assert_eq!(player.state(), PlaybackState::Playing);
        let mut events = Vec::new();
        let finished = pump_until(
            &context,
            &mut receiver,
            &mut events,
            std::time::Duration::from_secs(15),
            |events| {
                matches!(events.last(), Some(PlayerEvent::EndOfStream))
                    && player.state() == PlaybackState::Stopped
            },
        );
        assert!(finished, "first play-through should reach end-of-stream");

        // Switching to a *different* track after an end-of-stream must play
        // it from the beginning. A buggy player resumes the new stream at
        // the old stream's end position and posts an (almost) immediate
        // end-of-stream, so we require the EOS to arrive only after the vast
        // majority of the 3s track has elapsed.
        player.play_uri(&other_uri);
        assert_eq!(player.state(), PlaybackState::Playing);
        let replay_started = std::time::Instant::now();
        let mut reached_eos = false;
        while std::time::Instant::now() < replay_started + std::time::Duration::from_secs(15) {
            let mut fresh = Vec::new();
            while let Ok(event) = receiver.try_recv() {
                fresh.push(event);
            }
            if fresh
                .iter()
                .any(|event| matches!(event, PlayerEvent::EndOfStream))
            {
                reached_eos = true;
                break;
            }
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(reached_eos, "replayed track should reach end-of-stream");
        let elapsed = replay_started.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_millis(1500),
            "replay raised end-of-stream after {elapsed:?}; the stream did not play again"
        );
    }

    #[test]
    fn switching_tracks_while_playing_starts_the_new_stream() {
        init_gstreamer();
        // Two tracks of different lengths: a short one plays first, then a
        // longer one is selected *while the short one is still playing*. A
        // buggy player ignores the mid-playback switch (PLAYING → PLAYING
        // is skipped) and finishes the old track instead, which ends after
        // roughly 2s — the new track must instead run its full 3s.
        let short = write_test_wav("test-switch-short.wav", 2);
        let long = write_test_wav("test-switch-long.wav", 3);
        let short_uri = format!("file://{}", short.display());
        let long_uri = format!("file://{}", long.display());

        let (sender, mut receiver) = futures_channel::mpsc::unbounded();
        let player = Player::new(sender).expect("player should initialize");
        let context = glib::MainContext::default();

        player.play_uri(&short_uri);
        assert_eq!(player.state(), PlaybackState::Playing);

        // Let the short track play partway through before switching.
        let preroll = std::time::Instant::now();
        while std::time::Instant::now() < preroll + std::time::Duration::from_millis(700) {
            while receiver.try_recv().is_ok() {}
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        // Switch to the longer track mid-playback.
        player.play_uri(&long_uri);
        assert_eq!(player.state(), PlaybackState::Playing);
        let switch_time = std::time::Instant::now();
        let mut reached_eos = false;
        while std::time::Instant::now() < switch_time + std::time::Duration::from_secs(15) {
            while let Ok(event) = receiver.try_recv() {
                if matches!(event, PlayerEvent::EndOfStream) {
                    reached_eos = true;
                }
            }
            if reached_eos {
                break;
            }
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(reached_eos, "switched track should reach end-of-stream");
        let elapsed = switch_time.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_millis(2500),
            "the new track ended after only {elapsed:?} — the previous stream kept playing"
        );
    }
}
