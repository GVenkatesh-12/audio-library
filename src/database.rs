//! SQLite persistence layer.
//!
//! # Design
//!
//! The database layer is completely decoupled from the UI:
//!
//! * [`Database`] is a handle the UI keeps. Commands are sent over a
//!   standard `std::sync::mpsc` channel and executed on a dedicated
//!   **worker thread** that owns the `rusqlite::Connection`. This keeps
//!   disk I/O off the main UI thread (blocking operations never stall the
//!   interface) and means the connection never has to cross thread
//!   boundaries.
//! * Results come back on a futures [`UnboundedSender`] channel. The window
//!   spawns a local task that awaits them on the GTK main loop, so event
//!   handlers always run on the main thread.
//!
//! This split is deliberately future-proof: import-folder, search or export
//! features can add new [`Command`] variants without touching the UI. GLib
//! keeps no built-in channel type since 0.22, so the data plane uses
//! `futures_channel` (already a glib dependency) and the control plane uses
//! `std::sync::mpsc`.

use std::path::Path;
use std::path::PathBuf;
use std::thread;

use futures_channel::mpsc::UnboundedReceiver;
use futures_channel::mpsc::UnboundedSender;

use crate::models::AudioEntry;
use rusqlite::OptionalExtension;

/// Commands the UI can send to the database worker thread.
#[derive(Debug)]
pub enum Command {
    /// Load every stored entry plus the id of the previously selected one.
    Load,
    /// Store a new entry.
    Insert { title: String, file_path: String },
    /// Remove an entry by id.
    Delete { id: i64 },
    /// Remember which entry was selected last, so it can be restored on the
    /// next launch. `None` clears the memory.
    SetLastPlayed { id: Option<i64> },
}

/// Events the database worker sends back to the UI.
#[derive(Debug)]
pub enum Event {
    /// Reply to [`Command::Load`]: the full entry list and the id of the
    /// entry that was selected when the app last ran (if any).
    Loaded {
        entries: Vec<AudioEntry>,
        last_played: Option<i64>,
    },
    /// Reply to [`Command::Insert`]: the entry as it was actually stored.
    Inserted(AudioEntry),
    /// Reply to [`Command::Delete`].
    Deleted(i64),
    /// Any failure. The string is human-readable and meant to be shown in a
    /// dialog.
    Error(String),
}

/// Handle to the database worker.
#[derive(Clone)]
pub struct Database {
    command_sender: std::sync::mpsc::Sender<Command>,
}

impl Database {
    /// Start the database worker and return the handle plus the channel the
    /// UI should drain (normally via a `MainContext::spawn_local` task).
    ///
    /// The database file lives in `$XDG_DATA_HOME/audio-library/` (usually
    /// `~/.local/share/audio-library/`), which is created on demand.
    pub fn new() -> (Self, UnboundedReceiver<Event>) {
        let (command_sender, command_receiver) = std::sync::mpsc::channel();
        let (event_sender, event_receiver) = futures_channel::mpsc::unbounded();

        let db_path = default_db_path();
        thread::Builder::new()
            .name("audio-library-db".into())
            .spawn(move || worker(command_receiver, event_sender, db_path))
            .expect("failed to spawn database worker thread");

        let database = Self { command_sender };
        (database, event_receiver)
    }

    /// Ask the worker to send the full entry list.
    pub fn load(&self) {
        let _ = self.command_sender.send(Command::Load);
    }

    /// Store a new entry.
    pub fn insert(&self, title: String, file_path: String) {
        let _ = self
            .command_sender
            .send(Command::Insert { title, file_path });
    }

    /// Remove an entry.
    pub fn delete(&self, id: i64) {
        let _ = self.command_sender.send(Command::Delete { id });
    }

    /// Remember the id of the currently selected entry.
    pub fn set_last_played(&self, id: Option<i64>) {
        let _ = self.command_sender.send(Command::SetLastPlayed { id });
    }
}

/// Where the SQLite file is stored, following the XDG base directory spec.
fn default_db_path() -> PathBuf {
    let directory = glib::user_data_dir().join("audio-library");
    std::fs::create_dir_all(&directory)
        .unwrap_or_else(|error| panic!("cannot create data directory {directory:?}: {error}"));
    directory.join("audio-library.db")
}

/// Run the worker loop until the command channel is closed (which happens
/// when every `Database` handle is dropped, i.e. when the app exits).
fn worker(
    receiver: std::sync::mpsc::Receiver<Command>,
    sender: UnboundedSender<Event>,
    db_path: PathBuf,
) {
    let connection = match open_database(&db_path) {
        Ok(connection) => connection,
        Err(error) => {
            let _ = sender.unbounded_send(Event::Error(format!(
                "Could not open database: {error}"
            )));
            return;
        }
    };

    while let Ok(command) = receiver.recv() {
        // `Ok(None)` means the command completed but has nothing to report
        // to the UI (e.g. remembering the selection).
        let outcome: rusqlite::Result<Option<Event>> = match command {
            Command::Load => load_all(&connection).map(|(entries, last_played)| {
                Some(Event::Loaded {
                    entries,
                    last_played,
                })
            }),
            Command::Insert { title, file_path } => insert(&connection, &title, &file_path)
                .map(|entry| Some(Event::Inserted(entry))),
            Command::Delete { id } => delete(&connection, id).map(|_| Some(Event::Deleted(id))),
            Command::SetLastPlayed { id } => set_last_played(&connection, id).map(|_| None),
        };

        match outcome {
            Ok(Some(event)) => {
                if sender.unbounded_send(event).is_err() {
                    // The UI is gone; there is nothing left to keep open.
                    break;
                }
            }
            Ok(None) => {}
            Err(error) => {
                if sender
                    .unbounded_send(Event::Error(format!("Database error: {error}")))
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

/// Open the database, configure pragmas and make sure the schema exists.
fn open_database(db_path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    let connection = rusqlite::Connection::open(db_path)?;
    // WAL improves responsiveness when reads happen meanwhile and is cheap
    // for small personal libraries.
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;

    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS audio_files (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            title      TEXT NOT NULL,
            file_path  TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d', 'now'))
        );
        CREATE TABLE IF NOT EXISTS app_settings (
            key   TEXT PRIMARY KEY,
            value TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_audio_files_created ON audio_files (created_at);",
    )?;

    Ok(connection)
}

fn load_all(connection: &rusqlite::Connection) -> rusqlite::Result<(Vec<AudioEntry>, Option<i64>)> {
    let mut statement = connection.prepare(
        "SELECT id, title, file_path, created_at FROM audio_files ORDER BY created_at DESC, id DESC",
    )?;
    let rows = statement
        .query_map([], row_to_entry)?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let last_played = connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'last_played_id'",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .and_then(|value| value.parse::<i64>().ok());

    Ok((rows, last_played))
}

fn insert(
    connection: &rusqlite::Connection,
    title: &str,
    file_path: &str,
) -> rusqlite::Result<AudioEntry> {
    // `RETURNING` gives the UI the exact stored row without a second query.
    let mut statement = connection.prepare(
        "INSERT INTO audio_files (title, file_path, created_at)
         VALUES (?1, ?2, strftime('%Y-%m-%d', 'now'))
         RETURNING id, title, file_path, created_at",
    )?;
    statement.query_row((title, file_path), row_to_entry)
}

fn delete(connection: &rusqlite::Connection, id: i64) -> rusqlite::Result<()> {
    connection.execute("DELETE FROM audio_files WHERE id = ?1", [id])?;
    // If the deleted entry was the remembered selection, forget it too.
    if connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'last_played_id'",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .is_some_and(|value| value == id.to_string())
    {
        connection.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('last_played_id', NULL)",
            [],
        )?;
    }
    Ok(())
}

fn set_last_played(connection: &rusqlite::Connection, id: Option<i64>) -> rusqlite::Result<()> {
    let value = id.map(|id| id.to_string());
    connection.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('last_played_id', ?1)",
        [value],
    )?;
    Ok(())
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<AudioEntry> {
    Ok(AudioEntry {
        id: row.get(0)?,
        title: row.get(1)?,
        file_path: row.get(2)?,
        created_at: row.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database handle backed by an in-memory SQLite file, driven
    /// synchronously for tests.
    fn test_db() -> (Database, UnboundedReceiver<Event>) {
        let (command_sender, command_receiver) = std::sync::mpsc::channel();
        let (event_sender, event_receiver) = futures_channel::mpsc::unbounded();
        thread::spawn(move || worker(command_receiver, event_sender, PathBuf::from(":memory:")));
        (Database { command_sender }, event_receiver)
    }

    /// Block until the next event arrives (tests run without a main loop).
    fn recv(events: &mut UnboundedReceiver<Event>) -> Event {
        loop {
            match events.try_recv() {
                Ok(event) => return event,
                Err(futures_channel::mpsc::TryRecvError::Empty) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(futures_channel::mpsc::TryRecvError::Closed) => {
                    panic!("channel closed unexpectedly");
                }
            }
        }
    }

    #[test]
    fn insert_then_load_round_trip() {
        let (db, mut events) = test_db();
        db.insert("Meditation".into(), "/tmp/meditation.mp3".into());
        let inserted = recv(&mut events);
        let Event::Inserted(entry) = inserted else {
            panic!("expected Inserted event, got {inserted:?}");
        };
        assert_eq!(entry.title, "Meditation");
        assert_eq!(entry.file_path, "/tmp/meditation.mp3");
        assert!(!entry.created_at.is_empty());

        db.load();
        match recv(&mut events) {
            Event::Loaded { entries, .. } => {
                assert_eq!(entries, vec![entry]);
            }
            other => panic!("expected Loaded event, got {other:?}"),
        }
    }

    #[test]
    fn last_played_is_remembered_across_loads() {
        let (db, mut events) = test_db();
        db.set_last_played(Some(7));
        // SetLastPlayed has no reply event, so the next event is from Load.
        db.load();
        match recv(&mut events) {
            Event::Loaded { last_played, .. } => assert_eq!(last_played, Some(7)),
            other => panic!("expected Loaded event, got {other:?}"),
        }
    }

    #[test]
    fn deleting_removes_entry_and_clears_selection() {
        let (db, mut events) = test_db();
        db.insert("A".into(), "/tmp/a.mp3".into());
        let entry = match recv(&mut events) {
            Event::Inserted(entry) => entry,
            other => panic!("expected Inserted event, got {other:?}"),
        };
        db.set_last_played(Some(entry.id));

        db.delete(entry.id);
        assert!(matches!(recv(&mut events), Event::Deleted(id) if id == entry.id));

        db.load();
        match recv(&mut events) {
            Event::Loaded {
                entries,
                last_played,
            } => {
                assert!(entries.is_empty());
                assert_eq!(last_played, None);
            }
            other => panic!("expected Loaded event, got {other:?}"),
        }
    }
}