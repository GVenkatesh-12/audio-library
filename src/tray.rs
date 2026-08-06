//! Ubuntu top-bar indicator (a freedesktop *StatusNotifierItem*).
//!
//! # Design
//!
//! The indicator is implemented with [`ksni`], a pure-Rust implementation of
//! the StatusNotifierItem specification. It shows in the top panel of
//! GNOME/Ubuntu via the stock AppIndicator/SNI host that ships with Ubuntu,
//! and it lets the user pick a music title from the system menu and play it
//! without touching the main window.
//!
//! Because the indicator's menu callbacks run on a D-Bus service thread (not
//! the GTK main loop), they must never touch GTK widgets. Instead they push
//! [`TrayCommand`]s over an unbounded channel that [`crate::window::Window`]
//! drains on the main loop. `ksni`'s *blocking* API lets the main loop push
//! fresh state (entries + playback state) back into the menu via the
//! [`Handle`].
//!
//! [`ksni`]: https://docs.rs/ksni
//! [`Handle`]: ksni::blocking::Handle

use futures_channel::mpsc::UnboundedSender;

/// Commands the indicator sends to the main loop.
///
/// These are turned into UI actions by the window's command-draining task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// Start/switch playback to the entry with this database id.
    Play(i64),
    /// Toggle between play and pause.
    TogglePlayPause,
    /// Stop playback and rewind.
    Stop,
    /// Show (or create) the main window.
    Open,
    /// Quit the application (stops playback, closes the database).
    Quit,
}

/// Playback state mirrored into the indicator menu.
///
/// Separate from the window's own state so that the D-Bus thread never has
/// to reach into the GTK side.
#[derive(Debug, Clone, Default)]
pub struct TrayStatus {
    /// Stored entries as `(id, title)`, in display order.
    pub entries: Vec<(i64, String)>,
    /// Database id of the entry that is loaded for playback, if any.
    pub playing_id: Option<i64>,
    /// Whether audio is currently playing.
    pub playing: bool,
    /// Whether playback is paused.
    pub paused: bool,
}

/// The status notifier item shown in the top bar.
pub struct LibraryTray {
    sender: UnboundedSender<TrayCommand>,
    /// Library + playback state, kept current from the main loop.
    pub(crate) status: TrayStatus,
}

impl LibraryTray {
    /// Create the tray with an empty library. `sender` is where menu-activation
    /// commands are delivered.
    pub fn new(sender: UnboundedSender<TrayCommand>) -> Self {
        Self {
            sender,
            status: TrayStatus::default(),
        }
    }
}

impl ksni::Tray for LibraryTray {
    fn id(&self) -> String {
        "audio-library".into()
    }

    fn title(&self) -> String {
        "Audio Library".into()
    }

    fn icon_name(&self) -> String {
        // Present in the Adwaita icon theme shipped with Ubuntu.
        "multimedia-player-symbolic".into()
    }

    /// A left click on the icon opens the main window.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.sender.unbounded_send(TrayCommand::Open);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;

        let mut items: Vec<MenuItem<Self>> = Vec::new();

        // --- Now-playing header --------------------------------------------
        let header = match self.status.playing_id {
            Some(id) => {
                let title = self
                    .status
                    .entries
                    .iter()
                    .find(|(eid, _)| *eid == id)
                    .map(|(_, title)| title.clone())
                    .unwrap_or_default();
                if self.status.playing {
                    format!("Playing: {title}")
                } else if self.status.paused {
                    format!("Paused: {title}")
                } else {
                    format!("Loaded: {title}")
                }
            }
            None => "Nothing playing".into(),
        };
        items.push(
            StandardItem {
                label: header,
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);


        // --- Library ---------------------------------------------------------
        if self.status.entries.is_empty() {
            items.push(
                StandardItem {
                    label: "No saved audio".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            for (id, title) in &self.status.entries {
                let sender = self.sender.clone();
                let id = *id;
                let now_playing = Some(id) == self.status.playing_id;
                items.push(
                    StandardItem {
                        label: if now_playing {
                            format!("▶ {title}")
                        } else {
                            title.clone()
                        },
                        icon_name: if now_playing {
                            "media-playback-start-symbolic".into()
                        } else {
                            String::new()
                        },
                        activate: Box::new(move |_tray| {
                            let _ = sender.unbounded_send(TrayCommand::Play(id));
                        }),
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }

        items.push(MenuItem::Separator);

        // --- Transport ---------------------------------------------------------
        {
            let sender = self.sender.clone();
            items.push(
                StandardItem {
                    label: if self.status.playing {
                        "Pause".into()
                    } else {
                        "Play".into()
                    },
                    icon_name: if self.status.playing {
                        "media-playback-pause-symbolic".into()
                    } else {
                        "media-playback-start-symbolic".into()
                    },
                    enabled: self.status.playing_id.is_some(),
                    activate: Box::new(move |_tray| {
                        let _ = sender.unbounded_send(TrayCommand::TogglePlayPause);
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        {
            let sender = self.sender.clone();
            items.push(
                StandardItem {
                    label: "Stop".into(),
                    icon_name: "media-playback-stop-symbolic".into(),
                    enabled: self.status.playing_id.is_some(),
                    activate: Box::new(move |_tray| {
                        let _ = sender.unbounded_send(TrayCommand::Stop);
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(MenuItem::Separator);

        // --- Window / quit -----------------------------------------------------
        {
            let sender = self.sender.clone();
            items.push(
                StandardItem {
                    label: "Open Audio Library".into(),
                    icon_name: "window-new-symbolic".into(),
                    activate: Box::new(move |_tray| {
                        let _ = sender.unbounded_send(TrayCommand::Open);
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        {
            let sender = self.sender.clone();
            items.push(
                StandardItem {
                    label: "Quit".into(),
                    icon_name: "application-exit-symbolic".into(),
                    activate: Box::new(move |_tray| {
                        let _ = sender.unbounded_send(TrayCommand::Quit);
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }

        items
    }
}

