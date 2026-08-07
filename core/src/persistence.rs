//! Per-WI SQLite persistence. Mirrors `docs/persistence.md`.
//!
//! Each work item has its own `quorum.db` (WAL mode). The `state` row is the
//! single source of truth and only advances inside the same transaction that
//! persisted a step's outputs. Most methods are stubs at this skeleton stage;
//! the schema and open/migrate path are real so `verify` is meaningful.

use crate::state::State;
use rusqlite::Connection;
use std::path::Path;

/// The current schema version. Bump when `migrate` changes.
pub const SCHEMA_VERSION: i64 = 1;

/// Errors from the persistence layer.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("no state row found")]
    MissingState,
}

/// A handle to a work item's SQLite database.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) the DB at `path`, apply WAL mode, and migrate.
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Open an in-memory DB. Intended for tests.
    pub fn open_in_memory() -> Result<Store, StoreError> {
        let conn = Connection::open_in_memory()?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Create tables per `docs/persistence.md` if they do not yet exist.
    fn migrate(&self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS work_item (
                id       INTEGER PRIMARY KEY CHECK (id = 1),
                text     TEXT NOT NULL,
                source   TEXT,
                repo     TEXT,
                issue    INTEGER
            );

            CREATE TABLE IF NOT EXISTS state (
                id         INTEGER PRIMARY KEY CHECK (id = 1),
                state      TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS transitions (
                id     INTEGER PRIMARY KEY AUTOINCREMENT,
                from_state TEXT,
                to_state   TEXT NOT NULL,
                reason     TEXT,
                ts         TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS candidates (
                planner   TEXT NOT NULL,
                iteration INTEGER NOT NULL,
                text      TEXT NOT NULL,
                PRIMARY KEY (planner, iteration)
            );

            CREATE TABLE IF NOT EXISTS plan (
                id      INTEGER PRIMARY KEY CHECK (id = 1),
                text    TEXT NOT NULL,
                metrics TEXT
            );

            CREATE TABLE IF NOT EXISTS reviews (
                iteration INTEGER PRIMARY KEY,
                text      TEXT NOT NULL,
                accepted  INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
                state        TEXT PRIMARY KEY,
                session_name TEXT NOT NULL,
                ts           TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS events (
                id   INTEGER PRIMARY KEY AUTOINCREMENT,
                ts   TEXT NOT NULL,
                kind TEXT NOT NULL,
                data TEXT
            );
            "#,
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('schema_version', ?1)",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// The current schema version recorded in the DB.
    pub fn schema_version(&self) -> Result<i64, StoreError> {
        let v: String = self.conn.query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        Ok(v.parse().unwrap_or(0))
    }

    /// Read the current state, if one has been recorded.
    pub fn current_state(&self) -> Result<Option<State>, StoreError> {
        let row = self
            .conn
            .query_row("SELECT state FROM state WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            });
        match row {
            Ok(s) => Ok(serde_yaml::from_str(&s).ok()),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_and_migrates() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn open_creates_file_and_is_reopenable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quorum.db");
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.current_state().unwrap(), None);
        }
        // Reopening the same file must migrate idempotently.
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }
}
