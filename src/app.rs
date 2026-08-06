//! Application setup: the `Adw::Application`, resource loading and the
//! top-level wiring that brings the whole app up.

use gtk4::prelude::*;
use ksni::blocking::TrayMethods as _;
use std::cell::RefCell;
use std::rc::Rc;

use crate::tray::{LibraryTray, TrayCommand};
use crate::window::Window;

/// Reverse-DNS application id used on the D-Bus session bus and for the
/// compiled resource path. Change this before shipping.
pub const APPLICATION_ID: &str = "org.example.AudioLibrary";

/// Prefix of the bundled resources inside the binary.
pub const RESOURCE_PREFIX: &str = "/org/example/AudioLibrary";

/// Build the application and hand control to the GTK main loop.
pub fn run() -> glib::ExitCode {
    // When the app is autostarted we only want the tray icon, not a window
    // popping up on every login. The autostart entry sets this environment
    // variable (an env var is used because GTK's own option parser would
    // reject an unknown `--background` flag).
    let start_hidden = std::env::var("AUDIO_LIBRARY_BACKGROUND").is_ok();

    let app = libadwaita::Application::builder()
        .application_id(APPLICATION_ID)
        .build();

    app.connect_startup(|_| load_style());

    // Tray command bridge. The indicator (run by ksni on a D-Bus thread) only
    // ever sends commands down this channel; the window drains them on the
    // GTK main loop. The `Rc<RefCell<..>>` lets the first window claim the
    // receiver while later activations just present the existing window.
    let (command_tx, command_rx) = futures_channel::mpsc::unbounded::<TrayCommand>();
    let command_rx = Rc::new(RefCell::new(Some(command_rx)));

    // Spawn the status-notifier indicator. If the desktop has no SNI host
    // (e.g. a non-desktop session) the tray simply stays absent and `.ok()`
    // turns that into `None`; the app still runs normally.
    //
    // Inside a Flatpak sandbox the session bus forbids owning arbitrary
    // well-known names, which ksni needs for its default `StatusNotifierItem-*`
    // registration. Registering via the connection's unique name instead
    // keeps the tray icon working there (flatpak sets `FLATPAK_ID`).
    let tray = LibraryTray::new(command_tx);
    let tray_handle = if std::env::var("FLATPAK_ID").is_ok() {
        tray.disable_dbus_name(true).spawn().ok()
    } else {
        tray.spawn().ok()
    };

    app.connect_activate(move |app| {
        // SAFETY: we only ever store a valid `Rc<Window>` under this key.
        if let Some(rc) = unsafe { app.data::<Rc<Window>>("window") } {
            unsafe { rc.as_ref() }.window.present();
            return;
        }

        let command_rx = command_rx
            .borrow_mut()
            .take()
            .expect("tray command receiver already claimed");

        let window = Window::new(app, command_rx, tray_handle.clone());

        if !start_hidden {
            window.window.present();
        }
        // Keep the `Rc<Window>` alive for the whole application lifetime.
        // GTK keeps the widgets alive through its window list, but the
        // signal handlers need the `Rc` to stay valid.
        // SAFETY: The key "window" is only ever set here and the `Rc<Window>`
        // is valid for the application's lifetime.
        unsafe { app.set_data("window", window) };
    });

    app.run()
}

/// Register the compiled resources and apply the bundled stylesheet.
///
/// The stylesheet is intentionally minimal: libadwaita's default theme
/// (including automatic light/dark switching) does the real work.
fn load_style() {
    // `include_resource!` embeds the file built by `build.rs` into the
    // binary, so no external data directory is needed.
    gtk4::gio::resources_register_include!("audio-library.gresource")
        .expect("bundled resources must be valid");

    let provider = gtk4::CssProvider::new();
    provider.load_from_resource(&format!("{RESOURCE_PREFIX}/style.css"));
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}