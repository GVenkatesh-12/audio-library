//! Native Adwaita dialogs.
//!
//! All dialogs are presented modelessly via [`libadwaita::AlertDialog`];
//! callers provide callbacks instead of blocking the main loop. This
//! matches modern GTK4/libadwaita conventions and keeps the interface
//! responsive. AlertDialog dismisses itself after any response, so no
//! explicit closing is needed.

use gtk4::prelude::*;
use libadwaita::prelude::*;

/// Show a plain error dialog with a single "Close" button.
///
/// `parent` may be any window; the dialog floats above it.
pub fn show_error(parent: &impl IsA<gtk4::Widget>, title: &str, body: &str) {
    let dialog = libadwaita::AlertDialog::new(Some(title), Some(body));
    dialog.add_response("close", "Close");
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");
    dialog.present(Some(parent));
}

/// The dialog shown when a stored audio file no longer exists on disk.
///
/// Presents two choices:
/// * **Remove Entry** – calls `on_remove` so the caller can delete the
///   database row (and stop the player).
/// * **Cancel** – leaves the entry untouched.
pub fn show_missing_file(
    parent: &impl IsA<gtk4::Widget>,
    file_path: &str,
    on_remove: impl Fn() + 'static,
) {
    let body = if file_path.trim().is_empty() {
        "The audio file could not be found.".to_string()
    } else {
        format!("The audio file could not be found.\n\n{file_path}")
    };

    let dialog = libadwaita::AlertDialog::new(Some("Audio file could not be found."), Some(&body));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove Entry");
    dialog.set_response_appearance("cancel", libadwaita::ResponseAppearance::Suggested);
    dialog.set_response_appearance("remove", libadwaita::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    dialog.connect_response(None, move |_current, response| {
        if response == "remove" {
            on_remove();
        }
    });
    dialog.present(Some(parent));
}