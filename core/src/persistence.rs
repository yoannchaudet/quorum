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
        self.record_transition_with_events(from, to, reason, &[])
    }

    /// As [`record_transition`], but also appends `extra` `(kind, data)` events
    /// in the **same** transaction. Used to log an HI decision atomically with
    /// the transition it authorizes, so the audit log can never claim a decision
    /// that did not actually advance the state.
    pub fn record_transition_with_events(
        &mut self,
        from: Option<State>,
        to: State,
        reason: &str,
        extra: &[(&str, &str)],
    ) -> Result<(), StoreError> {
        let ts = now_millis();
        let from_s = from.map(|s| s.as_str());
        let to_s = to.as_str();
        let tx = self.conn.transaction()?;
        // Record the authorizing events before the transition so they read in
        // causal order; all commit together or not at all.
        for (kind, data) in extra {
            tx.execute(
                "INSERT INTO events (ts, kind, data) VALUES (?1, ?2, ?3)",
                params![ts, kind, data],
            )?;
        }
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

    /// Total number of logged events.
    pub fn count_events(&self) -> Result<i64, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?)
    }

    /// Number of logged events of a given kind.
    pub fn count_events_of_kind(&self, kind: &str) -> Result<i64, StoreError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE kind = ?1",
            params![kind],
            |r| r.get(0),
        )?)
    }

    /// Store the normalized WI markdown (single row). Idempotent (upsert).
    pub fn set_work_item(&mut self, text: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO work_item (id, text) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET text = excluded.text",
            params![text],
        )?;
        Ok(())
    }

    /// The stored WI markdown, if any.
    pub fn work_item(&self) -> Result<Option<String>, StoreError> {
        match self
            .conn
            .query_row("SELECT text FROM work_item WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            }) {
            Ok(t) => Ok(Some(t)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Save a planner's candidate plan for a given iteration. Idempotent (upsert).
    pub fn save_candidate(
        &mut self,
        planner: &str,
        iteration: u32,
        text: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO candidates (planner, iteration, text) VALUES (?1, ?2, ?3)
             ON CONFLICT(planner, iteration) DO UPDATE SET text = excluded.text",
            params![planner, iteration, text],
        )?;
        Ok(())
    }

    /// All candidate plans for a given iteration, ordered by planner slot.
    pub fn candidates(&self, iteration: u32) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT planner, text FROM candidates WHERE iteration = ?1 ORDER BY planner ASC",
        )?;
        let rows = stmt.query_map([iteration], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The highest candidate iteration recorded so far, if any.
    pub fn max_candidate_iteration(&self) -> Result<Option<u32>, StoreError> {
        let v: Option<i64> =
            self.conn
                .query_row("SELECT MAX(iteration) FROM candidates", [], |r| r.get(0))?;
        Ok(v.map(|n| n as u32))
    }

    /// Store the converged Plan (single row) with optional metrics. Idempotent.
    pub fn set_plan(&mut self, text: &str, metrics: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO plan (id, text, metrics) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET text = excluded.text, metrics = excluded.metrics",
            params![text, metrics],
        )?;
        Ok(())
    }

    /// The stored Plan text, if any.
    pub fn plan(&self) -> Result<Option<String>, StoreError> {
        match self
            .conn
            .query_row("SELECT text FROM plan WHERE id = 1", [], |r| {
                r.get::<_, String>(0)
            }) {
            Ok(t) => Ok(Some(t)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
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

    #[test]
    fn stores_work_item_and_candidates() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.work_item().unwrap(), None);
        store.set_work_item("# WI\nbody").unwrap();
        assert_eq!(store.work_item().unwrap().as_deref(), Some("# WI\nbody"));

        store.save_candidate("planner-b", 0, "plan B").unwrap();
        store.save_candidate("planner-a", 0, "plan A").unwrap();
        store.save_candidate("planner-a", 1, "plan A v2").unwrap();

        let iter0 = store.candidates(0).unwrap();
        assert_eq!(
            iter0,
            vec![
                ("planner-a".to_string(), "plan A".to_string()),
                ("planner-b".to_string(), "plan B".to_string()),
            ]
        );
        assert_eq!(store.candidates(1).unwrap().len(), 1);
    }
}
