//! Application setup: the `Adw::Application`, resource loading and the
//! top-level wiring that brings the whole app up.

use gtk4::prelude::*;

use crate::window::Window;

/// Reverse-DNS application id used on the D-Bus session bus and for the
/// compiled resource path. Change this before shipping.
pub const APPLICATION_ID: &str = "org.example.AudioLibrary";

/// Prefix of the bundled resources inside the binary.
pub const RESOURCE_PREFIX: &str = "/org/example/AudioLibrary";

/// Build the application and hand control to the GTK main loop.
pub fn run() -> glib::ExitCode {
    let app = libadwaita::Application::builder()
        .application_id(APPLICATION_ID)
        .build();

    app.connect_startup(|_| load_style());

    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        let window = Window::new(app);
        window.window.present();
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