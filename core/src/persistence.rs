//! Per-WI SQLite persistence. Mirrors `docs/persistence.md`.
//!
//! Each work item has its own `quorum.db` (WAL mode). The `state` row is the
//! single source of truth and only advances inside the same transaction that
//! persisted a step's outputs. Most methods are stubs at this skeleton stage;
//! the schema and open/migrate path are real so `verify` is meaningful.

use crate::state::State;
use rusqlite::{params, Connection};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// The current schema version. Bump when `migrate` changes.
pub const SCHEMA_VERSION: i64 = 1;

/// Errors from the persistence layer.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored state {0:?} is not a known state")]
    UnknownState(String),
}

/// A recorded state transition, newest-relevant order as inserted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: Option<State>,
    pub to: State,
    pub reason: String,
    pub ts: String,
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
            Ok(s) => State::from_db_str(&s)
                .map(Some)
                .ok_or(StoreError::UnknownState(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically advance the persisted state and record the transition.
    ///
    /// Updates the single `state` row, appends to `transitions`, and appends an
    /// `events` row — all in one SQLite transaction, so a crash leaves either
    /// the whole transition or none of it (see `docs/persistence.md`).
    pub fn record_transition(
        &mut self,
        from: Option<State>,
        to: State,
        reason: &str,
    ) -> Result<(), StoreError> {
        let ts = now_millis();
        let from_s = from.map(|s| s.as_str());
        let to_s = to.as_str();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO state (id, state, updated_at) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET state = excluded.state, updated_at = excluded.updated_at",
            params![to_s, ts],
        )?;
        tx.execute(
            "INSERT INTO transitions (from_state, to_state, reason, ts) VALUES (?1, ?2, ?3, ?4)",
            params![from_s, to_s, reason, ts],
        )?;
        let data = format!("{}->{}", from_s.unwrap_or("-"), to_s);
        tx.execute(
            "INSERT INTO events (ts, kind, data) VALUES (?1, 'transition', ?2)",
            params![ts, data],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The full transition history, in the order it was recorded.
    pub fn history(&self) -> Result<Vec<Transition>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT from_state, to_state, reason, ts FROM transitions ORDER BY id ASC")?;
        let rows = stmt.query_map([], |row| {
            let from: Option<String> = row.get(0)?;
            let to: String = row.get(1)?;
            let reason: String = row.get(2)?;
            let ts: String = row.get(3)?;
            Ok((from, to, reason, ts))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (from, to, reason, ts) = row?;
            let from = match from {
                Some(s) => Some(State::from_db_str(&s).ok_or(StoreError::UnknownState(s))?),
                None => None,
            };
            let to = State::from_db_str(&to).ok_or(StoreError::UnknownState(to))?;
            out.push(Transition {
                from,
                to,
                reason,
                ts,
            });
        }
        Ok(out)
    }
}

/// Milliseconds since the Unix epoch, as a string (sortable, dependency-free).
fn now_millis() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        .to_string()
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
    fn records_and_reads_back_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quorum.db");
        {
            let mut store = Store::open(&path).unwrap();
            store
                .record_transition(Some(State::Intake), State::Planning, "auto")
                .unwrap();
            store
                .record_transition(Some(State::Planning), State::Converging, "auto")
                .unwrap();
        }
        // Reopen to prove durability across a close (crash-resume shape).
        let store = Store::open(&path).unwrap();
        assert_eq!(store.current_state().unwrap(), Some(State::Converging));
        let history = store.history().unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].from, Some(State::Intake));
        assert_eq!(history[0].to, State::Planning);
        assert_eq!(history[1].to, State::Converging);
    }
}
