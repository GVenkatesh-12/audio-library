//! Main application window and all UI wiring.
//!
//! # Architecture
//!
//! The window owns four layers and keeps them strictly separated:
//!
//! * [`AppState`] – plain Rust data (entries, selection, pending file).
//! * [`Database`] – a handle that talks to the SQLite worker thread.
//! * [`Player`] – a GStreamer `playbin` wrapper driven by URIs.
//! * [`Ui`] – the widget tree.
//!
//! Nothing in here talks to SQLite or GStreamer directly; the window only
//! forwards user gestures to [`Database`]/[`Player`] and reacts to their
//! events. Handlers use `Rc<Window>` weak references so no reference
//! cycles keep the window alive.
//!
//! The window is the single place where the pieces are composed, which is
//! what allows the layers to stay independent and testable.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures_channel::mpsc::UnboundedReceiver;
use futures_util::StreamExt;
use glib::prelude::*;
use gtk4::prelude::*;
use libadwaita::prelude::*;

use crate::database::{Database, Event};
use crate::models::AudioEntry;
use crate::player::{PlaybackState, Player, PlayerEvent};
use crate::ui::dialogs;
use crate::ui::EntryListPopover;

/// How often the UI refreshes playback position and state.
const POSITION_REFRESH_MS: u64 = 500;
/// While the user is dragging the position slider, the UI must not move
/// it from under them.
const DRAG_GRACE_MS: u64 = 300;

/// Plain, GTK-free application state.
#[derive(Default)]
struct AppState {
    /// Entries as returned by the database, newest first.
    entries: Vec<AudioEntry>,
    /// Absolute path of the file chosen in the file picker, not yet saved.
    pending_path: Option<PathBuf>,
    /// Entry currently highlighted as "playing".
    playing_id: Option<i64>,
    /// Whether a file has been loaded into the player at all.
    has_current_uri: bool,
    /// Last known stream duration in seconds (0 = unknown).
    last_duration: u64,
    /// When the user last moved the position slider.
    last_interaction: Option<Instant>,
}

/// All widgets the window needs to address.
#[derive(Clone)]
struct Ui {
    title_entry: libadwaita::EntryRow,
    file_row: libadwaita::ActionRow,
    save_button: gtk4::Button,
    status_label: gtk4::Label,
    time_label: gtk4::Label,
    position_scale: gtk4::Scale,
    play_button: gtk4::ToggleButton,
    stop_button: gtk4::Button,
    toast_overlay: libadwaita::ToastOverlay,
    popover: EntryListPopover,
}

/// The application window. Widgets, database handle, player and state.
pub struct Window {
    pub window: libadwaita::ApplicationWindow,
    database: Database,
    player: Player,
    state: Rc<RefCell<AppState>>,
    ui: Ui,
}

impl Window {
    /// Build the full window: widget tree, signals, background services.
    pub fn new(app: &libadwaita::Application) -> Rc<Self> {
        let window = libadwaita::ApplicationWindow::builder()
            .application(app)
            .title("Audio Library")
            .default_width(520)
            .default_height(420)
            .build();

        let ui = build_ui(&window);

        let (player_events_tx, player_events_rx) = futures_channel::mpsc::unbounded();
        let player = match Player::new(player_events_tx) {
            Ok(player) => player,
            Err(error) => {
                // Without GStreamer the window is useless; show a clear error
                // and keep a non-functional player so the app still runs.
                dialogs::show_error(
                    &window,
                    "Playback unavailable",
                    &format!("GStreamer could not be initialized:\n{error}"),
                );
                Player::disabled()
            }
        };

        let (database, db_events) = Database::new();

        let window = Rc::new(Self {
            window,
            database,
            player,
            state: Rc::new(RefCell::new(AppState::default())),
            ui,
        });

        window.setup(player_events_rx, db_events);
        window
    }

    /// Connect all signals and start background updates.
    fn setup(self: &Rc<Self>, player_events: UnboundedReceiver<PlayerEvent>, db_events: UnboundedReceiver<Event>) {
        // --- Add-audio form ------------------------------------------------
        {
            let weak = Rc::downgrade(self);
            self.ui
                .title_entry
                .connect_notify_local(Some("text"), move |_, _| {
                    let Some(this) = weak.upgrade() else {
                        return;
                    };
                    this.update_save_enabled();
                });
        }

        // --- File chooser ---------------------------------------------------
        let choose = gtk4::Button::builder()
            .label("Choose MP3")
            .icon_name("document-open-symbolic")
            .build();
        {
            let weak = Rc::downgrade(self);
            let choose_outer = choose.clone();
            let choose_inner = choose.clone();
            choose_outer.connect_clicked(move |_| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                this.on_choose_file(&choose_inner);
            });
        }
        // The whole "Audio File" row acts as a button.
        self.ui.file_row.set_activatable_widget(Some(&choose));

        {
            let weak = Rc::downgrade(self);
            self.ui
                .save_button
                .connect_clicked(move |_| {
                    let Some(this) = weak.upgrade() else {
                        return;
                    };
                    this.on_save();
                });
        }

        // --- Library navigation ---------------------------------------------
        {
            let weak = Rc::downgrade(self);
            self.ui.popover.connect_activated(move |id| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                this.on_entry_activated(id);
            });
        }

        // --- Transport controls ----------------------------------------------
        {
            let weak = Rc::downgrade(self);
            self.ui
                .play_button
                .connect_toggled(move |button| {
                    let Some(this) = weak.upgrade() else {
                        return;
                    };
                    this.on_play_toggled(button);
                });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui
                .stop_button
                .connect_clicked(move |_| {
                    let Some(this) = weak.upgrade() else {
                        return;
                    };
                    this.on_stop();
                });
        }

        {
            let weak = Rc::downgrade(self);
            self.ui
                .position_scale
                .connect_change_value(move |_scale, _scroll, value| {
                    let Some(this) = weak.upgrade() else {
                        return glib::Propagation::Proceed;
                    };
                    this.on_seek(value);
                    glib::Propagation::Proceed
                });
        }

        // --- Database events --------------------------------------------------
        // Both event streams are drained on the main loop by local tasks so
        // handlers always run on the GTK thread.
        {
            let weak = Rc::downgrade(self);
            glib::MainContext::default().spawn_local(async move {
                let mut db_events = db_events;
                while let Some(event) = db_events.next().await {
                    let Some(this) = weak.upgrade() else {
                        break;
                    };
                    this.on_database_event(event);
                }
            });
        }

        // --- Player events -----------------------------------------------------
        {
            let weak = Rc::downgrade(self);
            glib::MainContext::default().spawn_local(async move {
                let mut player_events = player_events;
                while let Some(event) = player_events.next().await {
                    let Some(this) = weak.upgrade() else {
                        break;
                    };
                    this.on_player_event(event);
                }
            });
        }

        // --- Periodic position/state refresh -----------------------------------
        {
            let weak = Rc::downgrade(self);
            glib::timeout_add_local(
                Duration::from_millis(POSITION_REFRESH_MS),
                move || {
                    let Some(this) = weak.upgrade() else {
                        return glib::ControlFlow::Continue;
                    };
                    this.refresh();
                    glib::ControlFlow::Continue
                },
            );
        }

        // --- Keyboard shortcuts (Ctrl+O choose, Ctrl+S save) ---------------------
        let shortcuts = gtk4::ShortcutController::new();
        {
            let weak = Rc::downgrade(self);
            let choose = choose.clone();
            shortcuts.add_shortcut(gtk4::Shortcut::new(
                Some(gtk4::KeyvalTrigger::new(
                    gtk4::gdk::Key::o,
                    gtk4::gdk::ModifierType::CONTROL_MASK,
                )),
                Some(gtk4::CallbackAction::new(move |_widget, _target| {
                    let Some(this) = weak.upgrade() else {
                        return glib::Propagation::Stop;
                    };
                    this.on_choose_file(&choose);
                    glib::Propagation::Stop
                })),
            ));
        }
        {
            let weak = Rc::downgrade(self);
            shortcuts.add_shortcut(gtk4::Shortcut::new(
                Some(gtk4::KeyvalTrigger::new(
                    gtk4::gdk::Key::s,
                    gtk4::gdk::ModifierType::CONTROL_MASK,
                )),
                Some(gtk4::CallbackAction::new(move |_widget, _target| {
                    let Some(this) = weak.upgrade() else {
                        return glib::Propagation::Stop;
                    };
                    this.on_save();
                    glib::Propagation::Stop
                })),
            ));
        }
        self.window.add_controller(shortcuts);

        // --- Cleanup when the window goes away -----------------------------------
        let player = self.player.clone();
        self.window.connect_close_request(move |_| {
            player.stop();
            glib::Propagation::Proceed
        });

        // Load the library; the empty-state UI is shown until the reply.
        self.database.load();
        self.update_save_enabled();
        self.refresh_status();
    }

    // ------------------------------------------------------------------
    // Handlers
    // ------------------------------------------------------------------

    /// Open the native GTK file dialog for choosing an MP3.
    fn on_choose_file(self: &Rc<Self>, _button: &gtk4::Button) {
        let filter = gtk4::FileFilter::new();
        filter.set_name(Some("MP3 audio"));
        filter.add_mime_type("audio/mpeg");
        filter.add_mime_type("audio/mp3");
        filter.add_pattern("*.mp3");

        let filters = gtk4::gio::ListStore::new::<gtk4::FileFilter>();
        filters.append(&filter);

        let dialog = gtk4::FileDialog::builder()
            .title("Choose an MP3 file")
            .accept_label("Choose")
            .build();
        dialog.set_filters(Some(&filters));

        let state = Rc::clone(&self.state);
        let file_row = self.ui.file_row.clone();
        let window = self.window.clone();
        let weak = Rc::downgrade(self);

        dialog.open(Some(&window.clone()), gtk4::gio::Cancellable::NONE, move |result| {
            match result {
                Ok(file) => {
                    let Some(path) = file.path() else {
                        dialogs::show_error(
                            &window,
                            "Invalid file",
                            "The selected file could not be resolved to a local path.",
                        );
                        return;
                    };
                    state.borrow_mut().pending_path = Some(path.clone());
                    let path_text = path.display().to_string();
                    file_row.set_subtitle(&path_text);
                    file_row.set_tooltip_text(Some(&path_text));
                    if let Some(this) = weak.upgrade() {
                        this.update_save_enabled();
                    }
                }
                Err(error) if error.matches(gtk4::gio::IOErrorEnum::Cancelled) => {}
                Err(error) => {
                    dialogs::show_error(&window, "Could not open file", &error.to_string());
                }
            }
        });
    }

    /// Store the entered title + chosen file into the database.
    fn on_save(&self) {
        let title = self.ui.title_entry.text().trim().to_string();
        let path = self.state.borrow().pending_path.clone();
        let (Some(title), Some(path)) = (non_empty(&title), path) else {
            return;
        };
        let Some(path_str) = path.to_str() else {
            dialogs::show_error(
                &self.window,
                "Invalid file",
                "The selected file has no valid UTF-8 path.",
            );
            return;
        };
        self.database.insert(title, path_str.to_string());
    }

    /// The user picked an entry from the library popover.
    fn on_entry_activated(&self, id: i64) {
        let Some(entry) = self.state.borrow().entries.iter().find(|e| e.id == id).cloned() else {
            return;
        };
        self.ui.popover.popdown();

        // The library only stores references; the file may have moved.
        if !std::path::Path::new(&entry.file_path).exists() {
            let database = self.database.clone();
            let state = Rc::clone(&self.state);
            let player = self.player.clone();
            let file_path = entry.file_path.clone();
            dialogs::show_missing_file(&self.window, &file_path, move || {
                database.delete(id);
                if state.borrow().playing_id == Some(id) {
                    player.stop();
                    state.borrow_mut().playing_id = None;
                }
            });
            return;
        }

        let uri = match glib::filename_to_uri(&entry.file_path, None) {
            Ok(uri) => uri,
            Err(error) => {
                dialogs::show_error(
                    &self.window,
                    "Could not play file",
                    &format!("{entry:?}: {error}"),
                );
                return;
            }
        };

        self.player.play_uri(&uri);
        {
            let mut state = self.state.borrow_mut();
            state.playing_id = Some(id);
            state.has_current_uri = true;
            state.last_interaction = None;
        }
        self.database.set_last_played(Some(id));
        self.ui.popover.select(Some(id));
        self.refresh_status();
    }

    fn on_play_toggled(&self, button: &gtk4::ToggleButton) {
        // Ignore programmatic changes made by refresh().
        if button.is_active() == (self.player.state() == PlaybackState::Playing) {
            return;
        }
        if button.is_active() && !self.state.borrow().has_current_uri {
            button.set_active(false);
            return;
        }
        self.player.toggle_play_pause();
        self.refresh_status();
    }

    fn on_stop(&self) {
        self.player.stop();
        {
            let mut state = self.state.borrow_mut();
            state.last_interaction = None;
            state.last_duration = 0;
        }
        self.ui.position_scale.set_value(0.0);
        self.refresh_time_label();
        self.refresh_status();
    }

    /// The position slider was moved by the user.
    fn on_seek(&self, value: f64) {
        self.state.borrow_mut().last_interaction = Some(Instant::now());
        if self.player.state() != PlaybackState::Stopped {
            self.player.seek(value.max(0.0).round() as u64);
        }
    }

    /// A database worker reply arrived on the main loop.
    fn on_database_event(&self, event: Event) {
        match event {
            Event::Loaded {
                entries,
                last_played,
            } => {
                self.state.borrow_mut().entries = entries.clone();
                self.ui.popover.set_entries(&entries, last_played);
                self.ui.popover.select(self.state.borrow().playing_id);
                self.refresh_status();
            }
            Event::Inserted(entry) => {
                {
                    let mut state = self.state.borrow_mut();
                    state.entries.insert(0, entry.clone());
                }
                self.ui
                    .popover
                    .set_entries(&self.state.borrow().entries, self.state.borrow().playing_id);
                self.ui
                    .toast_overlay
                    .add_toast(libadwaita::Toast::new("Audio saved."));
                self.ui.title_entry.set_text("");
                self.state.borrow_mut().pending_path = None;
                self.ui.file_row.set_subtitle("No file selected");
                self.ui.file_row.set_tooltip_text(None);
                self.update_save_enabled();
            }
            Event::Deleted(id) => {
                {
                    let mut state = self.state.borrow_mut();
                    state.entries.retain(|entry| entry.id != id);
                    if state.playing_id == Some(id) {
                        state.playing_id = None;
                        state.has_current_uri = false;
                        self.player.stop();
                    }
                }
                self.ui
                    .popover
                    .set_entries(&self.state.borrow().entries, self.state.borrow().playing_id);
                self.refresh_status();
            }
            Event::Error(message) => {
                dialogs::show_error(&self.window, "Database error", &message);
            }
        }
    }

    /// An asynchronous condition from the player.
    fn on_player_event(&self, event: PlayerEvent) {
        match event {
            PlayerEvent::EndOfStream => {
                self.state.borrow_mut().last_interaction = None;
                self.ui.position_scale.set_value(0.0);
                self.refresh_time_label();
                self.refresh_status();
            }
            PlayerEvent::Error(message) => {
                self.state.borrow_mut().last_duration = 0;
                self.refresh_status();
                dialogs::show_error(&self.window, "Playback error", &message);
            }
        }
    }

    /// Periodic refresh of the transport controls, position and status.
    fn refresh(&self) {
        let player_state = self.player.state();
        let (position, duration) = if player_state == PlaybackState::Stopped {
            (0, 0)
        } else {
            (self.player.position(), self.player.duration())
        };

        {
            let mut state = self.state.borrow_mut();
            if duration != state.last_duration {
                state.last_duration = duration;
                self.ui.position_scale.set_range(0.0, duration as f64);
                self.ui.position_scale.set_increments(1.0, 10.0);
            }
            let dragging = state
                .last_interaction
                .map_or(false, |t| t.elapsed() < Duration::from_millis(DRAG_GRACE_MS));
            if !dragging && position > 0 {
                self.ui.position_scale.set_value(position as f64);
            }
        }

        self.ui
            .play_button
            .set_active(player_state == PlaybackState::Playing);
        self.ui.play_button.set_icon_name(if player_state == PlaybackState::Playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
        self.ui
            .play_button
            .set_tooltip_text(Some(if player_state == PlaybackState::Playing {
                "Pause"
            } else {
                "Play"
            }));
        self.ui
            .stop_button
            .set_sensitive(player_state != PlaybackState::Stopped);

        self.ui
            .time_label
            .set_text(&format!("{} / {}", format_time(position), format_time(duration)));
        self.refresh_status();
    }

    // ------------------------------------------------------------------
    // Small UI helpers
    // ------------------------------------------------------------------

    /// The Save button is only usable once both fields are filled in.
    fn update_save_enabled(&self) {
        let ready = !self.ui.title_entry.text().trim().is_empty()
            && self.state.borrow().pending_path.is_some();
        self.ui.save_button.set_sensitive(ready);
    }

    /// "Ready", "Playing: <title>" or "Paused: <title>".
    fn refresh_status(&self) {
        let state = self.state.borrow();
        let title = state
            .playing_id
            .and_then(|id| state.entries.iter().find(|entry| entry.id == id))
            .map(|entry| entry.title.clone());

        let text = match (self.player.state(), title) {
            (PlaybackState::Playing, Some(title)) => format!("Playing: {title}"),
            (PlaybackState::Playing, None) => "Playing".into(),
            (PlaybackState::Paused, Some(title)) => format!("Paused: {title}"),
            (PlaybackState::Paused, None) => "Paused".into(),
            (PlaybackState::Stopped, _) => "Ready".into(),
        };
        self.ui.status_label.set_text(&text);
    }

    fn refresh_time_label(&self) {
        let duration = self.state.borrow().last_duration;
        self.ui
            .time_label
            .set_text(&format!("0:00 / {}", format_time(duration)));
    }
}

// ------------------------------------------------------------------------
// Widget construction
// ------------------------------------------------------------------------

/// Build the complete widget tree and return the widgets the window logic
/// needs to address. The tree itself is attached to `window`.
fn build_ui(window: &libadwaita::ApplicationWindow) -> Ui {
    // --- Add-audio form --------------------------------------------------
    let title_entry = libadwaita::EntryRow::builder()
        .title("Title")
        .build();

    let file_row = libadwaita::ActionRow::builder()
        .title("Audio File")
        .subtitle("No file selected")
        .build();

    let save_button = gtk4::Button::with_label("Save");
    save_button.add_css_class("suggested-action");
    save_button.set_halign(gtk4::Align::End);
    save_button.set_sensitive(false);

    let form = libadwaita::PreferencesGroup::new();
    form.set_title("Add Audio");
    form.add(&title_entry);
    form.add(&file_row);

    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.append(&form);
    content.append(&save_button);

    let clamp = libadwaita::Clamp::new();
    clamp.set_maximum_size(640);
    clamp.set_tightening_threshold(480);
    clamp.set_child(Some(&content));

    // --- Toast layer ------------------------------------------------------
    let toast_overlay = libadwaita::ToastOverlay::new();
    toast_overlay.set_child(Some(&clamp));

    // --- Bottom transport bar ------------------------------------------------
    let status_label = gtk4::Label::new(Some("Ready"));
    status_label.set_halign(gtk4::Align::Start);
    status_label.set_xalign(0.0);
    status_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    status_label.set_hexpand(true);

    let position_scale = gtk4::Scale::builder()
        .draw_value(false)
        .hexpand(true)
        .width_request(180)
        .build();
    position_scale.set_range(0.0, 0.0);
    position_scale.set_value(0.0);

    let time_label = gtk4::Label::new(Some("0:00 / 0:00"));
    time_label.add_css_class("dim-label");

    let play_button = gtk4::ToggleButton::builder()
        .icon_name("media-playback-start-symbolic")
        .tooltip_text("Play")
        .build();
    let stop_button = gtk4::Button::builder()
        .icon_name("media-playback-stop-symbolic")
        .tooltip_text("Stop")
        .sensitive(false)
        .build();

    let bottom = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    bottom.set_margin_top(6);
    bottom.set_margin_bottom(6);
    bottom.set_margin_start(12);
    bottom.set_margin_end(12);
    bottom.append(&status_label);
    bottom.append(&position_scale);
    bottom.append(&time_label);
    bottom.append(&play_button);
    bottom.append(&stop_button);

    // --- Assemble -----------------------------------------------------------
    let popover = EntryListPopover::new();

    let header = libadwaita::HeaderBar::new();
    header.pack_end(popover.widget());

    let toolbar = libadwaita::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_bottom_bar(&bottom);
    toolbar.set_content(Some(&toast_overlay));

    window.set_content(Some(&toolbar));

    Ui {
        title_entry,
        file_row,
        save_button,
        status_label,
        time_label,
        position_scale,
        play_button,
        stop_button,
        toast_overlay,
        popover,
    }
}

/// `Some(s)` for a non-blank string, `None` otherwise.
fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Format seconds as `m:ss`.
fn format_time(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}