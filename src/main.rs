//! Audio Library – a lightweight personal audio collection for GNOME.
//!
//! The app stores references to MP3 files together with user-defined
//! titles and can play them back. It is deliberately small: no tagging,
//! playlists or metadata scraping.

mod app;
mod database;
mod models;
mod player;
mod ui;
mod window;

fn main() -> glib::ExitCode {
    // GStreamer must be initialized before any element is created. This is
    // independent of GTK; the two coexist fine.
    gstreamer::init().expect("Failed to initialize GStreamer");

    glib::set_application_name("Audio Library");

    app::run()
}