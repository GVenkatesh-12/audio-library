//! Domain model for audio library entries.
//!
//! This module is intentionally free of GTK/GStreamer dependencies so the
//! same types can be reused by the database layer, the player and (in the
//! future) by serialization for export/import features.

use serde::{Deserialize, Serialize};

/// A single stored audio entry: a user-defined title plus the absolute
/// path of the MP3 file it refers to.
///
/// The library deliberately stores only a *reference* to the file: the
/// file itself stays wherever the user placed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioEntry {
    /// Stable database identifier.
    pub id: i64,
    /// User-defined display title.
    pub title: String,
    /// Absolute path of the audio file on disk.
    pub file_path: String,
    /// Day the entry was created, in `YYYY-MM-DD` form (UTC).
    pub created_at: String,
}

impl AudioEntry {
    /// Convenience constructor for tests and code that builds entries
    /// before persisting them.
    #[cfg(test)]
    pub fn new(id: i64, title: impl Into<String>, file_path: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            file_path: file_path.into(),
            created_at: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_compare_by_value() {
        let a = AudioEntry::new(1, "Meditation", "/tmp/a.mp3");
        let b = AudioEntry::new(1, "Meditation", "/tmp/a.mp3");
        assert_eq!(a, b);
    }

    #[test]
    fn entries_round_trip_through_serde_json() {
        let entry = AudioEntry::new(1, "Lecture 1", "/tmp/l1.mp3");
        let json = serde_json::to_string(&entry).unwrap();
        let back: AudioEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }
}
