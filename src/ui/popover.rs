//! The popover listing stored audio entries.
//!
//! This is the app's navigation surface: the "☰" menu button in the header
//! bar opens a popover whose rows are the stored audio entries. Clicking a
//! row starts playback immediately.
//!
//! # Design
//!
//! The popover uses a plain [`gtk4::ListBox`] instead of a `Gio::MenuModel`
//! on purpose: a list box allows the currently playing row to be visually
//! highlighted (native `:selected` styling) and gives room to grow (tags,
//! favorites, right-click menus) without changing the widget.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;

use crate::models::AudioEntry;

/// A header-bar menu button that opens a popover listing the library.
#[derive(Clone)]
pub struct EntryListPopover {
    button: gtk4::MenuButton,
    list: gtk4::ListBox,
    stack: gtk4::Stack,
    /// Entries shown, in display order, each paired with its row. The list
    /// is rebuilt wholesale whenever entries change, so the two vectors
    /// always stay aligned.
    entries: Rc<RefCell<Vec<AudioEntry>>>,
    rows: Rc<RefCell<Vec<gtk4::ListBoxRow>>>,
}

impl EntryListPopover {
    /// Create the button with an empty list. Call [`set_entries`] to
    /// populate it.
    pub fn new() -> Self {
        let empty_label = gtk4::Label::new(Some("No entries yet"));
        empty_label.add_css_class("dim-label");
        empty_label.set_margin_top(24);
        empty_label.set_margin_bottom(24);

        let list = gtk4::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk4::SelectionMode::Single);

        let scrolled = gtk4::ScrolledWindow::new();
        scrolled.set_min_content_width(260);
        scrolled.set_max_content_height(320);
        scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        scrolled.set_child(Some(&list));

        let stack = gtk4::Stack::new();
        stack.add_titled(&empty_label, Some("empty"), "Empty");
        stack.add_titled(&scrolled, Some("list"), "Entries");
        stack.set_visible_child_name("empty");

        let popover = gtk4::Popover::new();
        popover.set_child(Some(&stack));
        popover.set_position(gtk4::PositionType::Bottom);

        let button = gtk4::MenuButton::new();
        button.set_icon_name("open-menu-symbolic");
        button.set_tooltip_text(Some("Audio library"));
        button.set_popover(Some(&popover));

        Self {
            button,
            list,
            stack,
            entries: Rc::new(RefCell::new(Vec::new())),
            rows: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// The widget to pack into the header bar.
    pub fn widget(&self) -> &gtk4::MenuButton {
        &self.button
    }

    /// Replace the displayed entries, highlighting `selected_id` if given.
    ///
    /// Rows are rebuilt from scratch; the list is small so this is cheap.
    pub fn set_entries(&self, entries: &[AudioEntry], selected_id: Option<i64>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }

        {
            // Scoped so the `rows` mutable borrow is released before `select`
            // below tries to re-borrow it (a `RefCell` panic otherwise).
            let mut rows = self.rows.borrow_mut();
            rows.clear();
            *self.entries.borrow_mut() = entries.to_vec();

            for (index, entry) in entries.iter().enumerate() {
                let row = entry_row(entry, index);
                self.list.append(&row);
                rows.push(row);
            }
        }

        self.stack.set_visible_child_name(if entries.is_empty() {
            "empty"
        } else {
            "list"
        });

        self.select(selected_id);
    }

    /// Highlight the row for `id`, clearing the highlight if `None`.
    pub fn select(&self, id: Option<i64>) {
        let entries = self.entries.borrow();
        let rows = self.rows.borrow();
        let Some((_entry, row)) = entries
            .iter()
            .zip(rows.iter())
            .find(|(entry, _row)| Some(entry.id) == id)
        else {
            self.list.unselect_all();
            return;
        };
        self.list.select_row(Some(row));
    }

    /// Close the popover.
    pub fn popdown(&self) {
        if let Some(popover) = self.button.popover() {
            popover.popdown();
        }
    }

    /// Invoked when the user activates a row. The id of the corresponding
    /// entry is passed to `callback`.
    ///
    /// Rows are appended in entry order, so the row's index in the list
    /// doubles as the index into the stored entries.
    pub fn connect_activated(&self, callback: impl Fn(i64) + 'static) {
        let entries = Rc::clone(&self.entries);
        self.list.connect_row_activated(move |_list, row| {
            let index = row.index();
            if index >= 0 {
                if let Some(entry) = entries.borrow().get(index as usize) {
                    callback(entry.id);
                }
            }
        });
    }
}

/// Build one popover row showing the entry title.
fn entry_row(entry: &AudioEntry, _index: usize) -> gtk4::ListBoxRow {
    let label = gtk4::Label::new(Some(&entry.title));
    label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    label.set_xalign(0.0);
    label.set_margin_top(6);
    label.set_margin_bottom(6);
    label.set_margin_start(10);
    label.set_margin_end(10);

    let row = gtk4::ListBoxRow::new();
    row.set_child(Some(&label));
    row
}

impl Default for EntryListPopover {
    fn default() -> Self {
        Self::new()
    }
}