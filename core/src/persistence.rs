//! Global SQLite persistence. Mirrors `docs/persistence.md`.
//!
//! Quorum stores all structured state in one database. [`Database`] owns
//! catalog-level operations, while [`Store`] scopes every coordinator query to
//! one work item so WI data cannot leak across runs.

use crate::observability::{
    ActivityEvent, ActivityKind, ImplementationSnapshot, PlanningSnapshot, ReviewSnapshot,
    StateSnapshot, StatusSnapshot, WorkItemIdentitySnapshot, WorkspaceSnapshot,
};
use crate::repository::RepositoryRoot;
use crate::state::State;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// The global database schema version.
pub const SCHEMA_VERSION: i64 = 5;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Stable internal identity for a work item.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkItemId(String);

impl WorkItemId {
    fn new() -> WorkItemId {
        WorkItemId(Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn for_test(value: impl Into<String>) -> WorkItemId {
        WorkItemId(value.into())
    }
}

impl fmt::Display for WorkItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable internal identity for a registered repository.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepositoryId(String);

impl RepositoryId {
    fn new() -> RepositoryId {
        RepositoryId(Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RepositoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One repository in Quorum's allow-list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredRepository {
    pub id: RepositoryId,
    pub root: std::path::PathBuf,
}

/// Persisted intent and lifecycle state for a WI's linked Git worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRecord {
    pub work_item_id: WorkItemId,
    pub base_commit: String,
    pub branch: String,
    pub path: std::path::PathBuf,
    pub ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImplementationRoundStatus {
    Running,
    AgentComplete,
    Committed,
}

impl ImplementationRoundStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ImplementationRoundStatus::Running => "running",
            ImplementationRoundStatus::AgentComplete => "agent_complete",
            ImplementationRoundStatus::Committed => "committed",
        }
    }

    fn from_str(value: &str) -> Option<ImplementationRoundStatus> {
        match value {
            "running" => Some(ImplementationRoundStatus::Running),
            "agent_complete" => Some(ImplementationRoundStatus::AgentComplete),
            "committed" => Some(ImplementationRoundStatus::Committed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplementationRound {
    pub iteration: u32,
    pub start_commit: String,
    pub status: ImplementationRoundStatus,
    pub result_commit: Option<String>,
    pub tree_sha: Option<String>,
}

/// Errors from the persistence layer.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored state {0:?} is not a known state")]
    UnknownState(String),
    #[error("work item {0} does not exist")]
    WorkItemNotFound(WorkItemId),
    #[error("database schema version {found} is not supported (expected {expected})")]
    UnsupportedSchema { found: i64, expected: i64 },
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(std::path::PathBuf),
    #[error("stored implementation round status {0:?} is invalid")]
    InvalidRoundStatus(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// A recorded state transition, in insertion order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    pub from: Option<State>,
    pub to: State,
    pub reason: String,
    pub ts: String,
}

/// Catalog-level access to the global Quorum database.
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open the global database, creating and initializing it when needed.
    pub fn open(path: &Path) -> Result<Database, StoreError> {
        let conn = Connection::open(path)?;
        initialize_connection(&conn)?;
        let db = Database { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Open an isolated in-memory global database. Intended for tests.
    pub fn open_in_memory() -> Result<Database, StoreError> {
        let conn = Connection::open_in_memory()?;
        initialize_connection(&conn)?;
        let db = Database { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;

        let stored_version = match self.conn.query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        ) {
            Ok(value) => value.parse().unwrap_or(0),
            Err(rusqlite::Error::QueryReturnedNoRows) => 0,
            Err(error) => return Err(error.into()),
        };
        if stored_version > SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema {
                found: stored_version,
                expected: SCHEMA_VERSION,
            });
        }

        if stored_version == 1 {
            self.migrate_v1_to_v2()?;
        }

        self.create_schema_v2()?;
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    fn create_schema_v2(&self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS repositories (
                id            TEXT PRIMARY KEY,
                root          TEXT NOT NULL UNIQUE,
                registered    INTEGER NOT NULL,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS work_items (
                id           TEXT PRIMARY KEY,
                repository_id TEXT REFERENCES repositories(id),
                slug         TEXT NOT NULL,
                text         TEXT,
                source       TEXT,
                origin_repo  TEXT,
                origin_issue INTEGER,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                UNIQUE (repository_id, slug)
            );

            CREATE TABLE IF NOT EXISTS states (
                work_item_id TEXT PRIMARY KEY REFERENCES work_items(id) ON DELETE CASCADE,
                state        TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS transitions (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
                from_state   TEXT,
                to_state     TEXT NOT NULL,
                reason       TEXT,
                ts           TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS transitions_work_item
                ON transitions(work_item_id, id);

            CREATE TABLE IF NOT EXISTS candidates (
                work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
                planner      TEXT NOT NULL,
                iteration    INTEGER NOT NULL,
                text         TEXT NOT NULL,
                PRIMARY KEY (work_item_id, planner, iteration)
            );

            CREATE TABLE IF NOT EXISTS plans (
                work_item_id TEXT PRIMARY KEY REFERENCES work_items(id) ON DELETE CASCADE,
                text         TEXT NOT NULL,
                metrics      TEXT
            );

            CREATE TABLE IF NOT EXISTS implementations (
                work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
                iteration    INTEGER NOT NULL,
                summary      TEXT NOT NULL,
                ts           TEXT NOT NULL,
                PRIMARY KEY (work_item_id, iteration)
            );

            CREATE TABLE IF NOT EXISTS intake (
                work_item_id TEXT PRIMARY KEY REFERENCES work_items(id) ON DELETE CASCADE,
                questions    TEXT NOT NULL,
                ts           TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS reviews (
                work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
                iteration    INTEGER NOT NULL,
                text         TEXT NOT NULL,
                accepted     INTEGER NOT NULL,
                PRIMARY KEY (work_item_id, iteration)
            );

            CREATE TABLE IF NOT EXISTS sessions (
                work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
                state        TEXT NOT NULL,
                session_name TEXT NOT NULL,
                ts           TEXT NOT NULL,
                PRIMARY KEY (work_item_id, state)
            );

            CREATE TABLE IF NOT EXISTS events (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
                ts           TEXT NOT NULL,
                kind         TEXT NOT NULL,
                data         TEXT
            );

            CREATE INDEX IF NOT EXISTS events_work_item
                ON events(work_item_id, id);

            CREATE TABLE IF NOT EXISTS activities (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
                ts           INTEGER NOT NULL,
                kind         TEXT NOT NULL,
                data         TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS activities_work_item
                ON activities(work_item_id, id);

            CREATE TABLE IF NOT EXISTS worktrees (
                work_item_id TEXT PRIMARY KEY REFERENCES work_items(id) ON DELETE CASCADE,
                base_commit  TEXT NOT NULL,
                branch       TEXT NOT NULL,
                path         TEXT NOT NULL UNIQUE,
                status       TEXT NOT NULL CHECK (status IN ('creating', 'ready')),
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS implementation_rounds (
                work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
                iteration    INTEGER NOT NULL,
                start_commit TEXT NOT NULL,
                status       TEXT NOT NULL CHECK (
                    status IN ('running', 'agent_complete', 'committed')
                ),
                result_commit TEXT,
                tree_sha      TEXT,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                PRIMARY KEY (work_item_id, iteration)
            );
            "#,
        )?;
        Ok(())
    }

    fn migrate_v1_to_v2(&self) -> Result<(), StoreError> {
        self.conn.pragma_update(None, "foreign_keys", false)?;
        let result = (|| -> Result<(), rusqlite::Error> {
            self.conn.execute_batch("BEGIN IMMEDIATE;")?;
            let version: String = self.conn.query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )?;
            if version != "1" {
                self.conn.execute_batch("COMMIT;")?;
                return Ok(());
            }

            self.conn.execute_batch(
                r#"
                CREATE TABLE repositories (
                id            TEXT PRIMARY KEY,
                root          TEXT NOT NULL UNIQUE,
                registered    INTEGER NOT NULL,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
            );

            CREATE TABLE work_items_v2 (
                id            TEXT PRIMARY KEY,
                repository_id TEXT REFERENCES repositories(id),
                slug          TEXT NOT NULL,
                text          TEXT,
                source        TEXT,
                origin_repo   TEXT,
                origin_issue  INTEGER,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                UNIQUE (repository_id, slug)
            );

            INSERT INTO work_items_v2
                (id, repository_id, slug, text, source, origin_repo, origin_issue, created_at, updated_at)
            SELECT id, NULL, slug, text, source, origin_repo, origin_issue, created_at, updated_at
            FROM work_items;

            DROP TABLE work_items;
            ALTER TABLE work_items_v2 RENAME TO work_items;
            UPDATE meta SET value = '2' WHERE key = 'schema_version';

            COMMIT;
            "#,
            )
        })();
        if result.is_err() {
            let _ = self.conn.execute_batch("ROLLBACK;");
        }
        let foreign_keys = self.conn.pragma_update(None, "foreign_keys", true);
        result?;
        foreign_keys?;
        Ok(())
    }

    /// The current global schema version.
    pub fn schema_version(&self) -> Result<i64, StoreError> {
        schema_version(&self.conn)
    }

    /// Add or reactivate a repository in Quorum's allow-list.
    pub fn register_repository(
        &mut self,
        root: &RepositoryRoot,
    ) -> Result<RegisteredRepository, StoreError> {
        let root_text = root
            .as_path()
            .to_str()
            .ok_or_else(|| StoreError::NonUtf8Path(root.as_path().to_path_buf()))?;
        let id = RepositoryId::new();
        let ts = now_millis();
        let stored_id = self.conn.query_row(
            "INSERT INTO repositories (id, root, registered, created_at, updated_at)
             VALUES (?1, ?2, 1, ?3, ?3)
             ON CONFLICT(root) DO UPDATE
             SET registered = 1, updated_at = excluded.updated_at
             RETURNING id",
            params![id.as_str(), root_text, ts],
            |row| row.get::<_, String>(0),
        )?;
        Ok(RegisteredRepository {
            id: RepositoryId(stored_id),
            root: root.as_path().to_path_buf(),
        })
    }

    /// Deactivate a repository without deleting its identity or WI history.
    pub fn unregister_repository(
        &mut self,
        root: &RepositoryRoot,
    ) -> Result<Option<RegisteredRepository>, StoreError> {
        let repository = self.repository_by_root(root, false)?;
        if let Some(repository) = &repository {
            self.conn.execute(
                "UPDATE repositories SET registered = 0, updated_at = ?1 WHERE id = ?2",
                params![now_millis(), repository.id.as_str()],
            )?;
        }
        Ok(repository)
    }

    /// Find an active registered repository by canonical root.
    pub fn registered_repository(
        &self,
        root: &RepositoryRoot,
    ) -> Result<Option<RegisteredRepository>, StoreError> {
        self.repository_by_root(root, true)
    }

    fn repository_by_root(
        &self,
        root: &RepositoryRoot,
        active_only: bool,
    ) -> Result<Option<RegisteredRepository>, StoreError> {
        let root_text = root
            .as_path()
            .to_str()
            .ok_or_else(|| StoreError::NonUtf8Path(root.as_path().to_path_buf()))?;
        let sql = if active_only {
            "SELECT id, root FROM repositories WHERE root = ?1 AND registered = 1"
        } else {
            "SELECT id, root FROM repositories WHERE root = ?1"
        };
        match self.conn.query_row(sql, params![root_text], |row| {
            Ok(RegisteredRepository {
                id: RepositoryId(row.get(0)?),
                root: std::path::PathBuf::from(row.get::<_, String>(1)?),
            })
        }) {
            Ok(repository) => Ok(Some(repository)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// List active registered repositories in root-path order.
    pub fn repositories(&self) -> Result<Vec<RegisteredRepository>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, root FROM repositories WHERE registered = 1 ORDER BY root ASC")?;
        let rows = stmt.query_map([], |row| {
            Ok(RegisteredRepository {
                id: RepositoryId(row.get(0)?),
                root: std::path::PathBuf::from(row.get::<_, String>(1)?),
            })
        })?;
        let mut repositories = Vec::new();
        for row in rows {
            repositories.push(row?);
        }
        Ok(repositories)
    }

    /// Find a work item by repository and user-facing slug.
    pub fn work_item_id(
        &self,
        repository_id: &RepositoryId,
        slug: &str,
    ) -> Result<Option<WorkItemId>, StoreError> {
        match self.conn.query_row(
            "SELECT id FROM work_items WHERE repository_id = ?1 AND slug = ?2",
            params![repository_id.as_str(), slug],
            |row| row.get::<_, String>(0),
        ) {
            Ok(id) => Ok(Some(WorkItemId(id))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Return the existing WI for `(repository, slug)`, or create an empty one.
    pub fn get_or_create_work_item(
        &mut self,
        repository_id: &RepositoryId,
        slug: &str,
    ) -> Result<WorkItemId, StoreError> {
        let id = WorkItemId::new();
        let ts = now_millis();
        self.conn
            .query_row(
                "INSERT INTO work_items (id, repository_id, slug, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(repository_id, slug) DO UPDATE SET slug = excluded.slug
             RETURNING id",
                params![id.as_str(), repository_id.as_str(), slug, ts],
                |row| row.get::<_, String>(0),
            )
            .map(WorkItemId)
            .map_err(Into::into)
    }

    /// The persisted worktree intent for a WI, if setup has begun.
    pub fn worktree(
        &self,
        work_item_id: &WorkItemId,
    ) -> Result<Option<WorktreeRecord>, StoreError> {
        match self.conn.query_row(
            "SELECT base_commit, branch, path, status
             FROM worktrees WHERE work_item_id = ?1",
            params![work_item_id.as_str()],
            |row| {
                Ok(WorktreeRecord {
                    work_item_id: work_item_id.clone(),
                    base_commit: row.get(0)?,
                    branch: row.get(1)?,
                    path: std::path::PathBuf::from(row.get::<_, String>(2)?),
                    ready: row.get::<_, String>(3)? == "ready",
                })
            },
        ) {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Persist worktree creation intent before touching Git.
    pub fn reserve_worktree(
        &mut self,
        work_item_id: &WorkItemId,
        base_commit: &str,
        branch: &str,
        path: &Path,
    ) -> Result<WorktreeRecord, StoreError> {
        let path_text = path
            .to_str()
            .ok_or_else(|| StoreError::NonUtf8Path(path.to_path_buf()))?;
        let ts = now_millis();
        self.conn.execute(
            "INSERT INTO worktrees
             (work_item_id, base_commit, branch, path, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'creating', ?5, ?5)",
            params![work_item_id.as_str(), base_commit, branch, path_text, ts],
        )?;
        Ok(WorktreeRecord {
            work_item_id: work_item_id.clone(),
            base_commit: base_commit.to_string(),
            branch: branch.to_string(),
            path: path.to_path_buf(),
            ready: false,
        })
    }

    /// Mark a reconciled worktree ready for agent use.
    pub fn mark_worktree_ready(&mut self, work_item_id: &WorkItemId) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE worktrees SET status = 'ready', updated_at = ?1 WHERE work_item_id = ?2",
            params![now_millis(), work_item_id.as_str()],
        )?;
        Ok(())
    }

    /// Scope this connection to one work item for Coordinator use.
    pub fn into_store(self, work_item_id: WorkItemId) -> Result<Store, StoreError> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_items WHERE id = ?1)",
            params![work_item_id.as_str()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StoreError::WorkItemNotFound(work_item_id));
        }
        Ok(Store {
            conn: self.conn,
            work_item_id,
        })
    }
}

/// A global database connection scoped to one work item.
pub struct Store {
    conn: Connection,
    work_item_id: WorkItemId,
}

impl Store {
    /// Open a fresh in-memory database with one empty WI. Intended for tests.
    pub fn open_in_memory() -> Result<Store, StoreError> {
        let mut db = Database::open_in_memory()?;
        let root = RepositoryRoot::from_canonical("/test/repository");
        let repository = db.register_repository(&root)?;
        let id = db.get_or_create_work_item(&repository.id, "test-wi")?;
        db.into_store(id)
    }

    /// The stable internal identity of this store's WI.
    pub fn work_item_id(&self) -> &WorkItemId {
        &self.work_item_id
    }

    /// The global schema version recorded by this connection.
    pub fn schema_version(&self) -> Result<i64, StoreError> {
        schema_version(&self.conn)
    }

    /// Read the current state for this WI.
    pub fn current_state(&self) -> Result<Option<State>, StoreError> {
        let row = self.conn.query_row(
            "SELECT state FROM states WHERE work_item_id = ?1",
            params![self.work_item_id.as_str()],
            |row| row.get::<_, String>(0),
        );
        match row {
            Ok(s) => State::from_db_str(&s)
                .map(Some)
                .ok_or(StoreError::UnknownState(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn record_transition(
        &mut self,
        from: Option<State>,
        to: State,
        reason: &str,
    ) -> Result<(), StoreError> {
        self.record_transition_with_events(from, to, reason, &[])
    }

    pub fn record_transition_with_events(
        &mut self,
        from: Option<State>,
        to: State,
        reason: &str,
        extra: &[(&str, &str)],
    ) -> Result<(), StoreError> {
        let ts = now_millis();
        let wi = self.work_item_id.as_str();
        let from_s = from.map(|s| s.as_str());
        let to_s = to.as_str();
        let tx = self.conn.transaction()?;
        for (kind, data) in extra {
            tx.execute(
                "INSERT INTO events (work_item_id, ts, kind, data) VALUES (?1, ?2, ?3, ?4)",
                params![wi, ts, kind, data],
            )?;
        }
        tx.execute(
            "INSERT INTO states (work_item_id, state, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(work_item_id) DO UPDATE
             SET state = excluded.state, updated_at = excluded.updated_at",
            params![wi, to_s, ts],
        )?;
        tx.execute(
            "INSERT INTO transitions
             (work_item_id, from_state, to_state, reason, ts)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![wi, from_s, to_s, reason, ts],
        )?;
        let data = format!("{}->{}", from_s.unwrap_or("-"), to_s);
        tx.execute(
            "INSERT INTO events (work_item_id, ts, kind, data)
             VALUES (?1, ?2, 'transition', ?3)",
            params![wi, ts, data],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn history(&self) -> Result<Vec<Transition>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT from_state, to_state, reason, ts
             FROM transitions WHERE work_item_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![self.work_item_id.as_str()], |row| {
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

    pub fn record_activity(&mut self, event: &ActivityEvent) -> Result<ActivityEvent, StoreError> {
        let data = serde_json::to_string(event)?;
        self.conn.execute(
            "INSERT INTO activities (work_item_id, ts, kind, data)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                self.work_item_id.as_str(),
                event.timestamp_ms as i64,
                activity_kind_name(event.kind),
                data
            ],
        )?;
        let mut recorded = event.clone();
        recorded.id = Some(self.conn.last_insert_rowid());
        Ok(recorded)
    }

    pub fn activities(&self) -> Result<Vec<ActivityEvent>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, data FROM activities
             WHERE work_item_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![self.work_item_id.as_str()], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut activities = Vec::new();
        for row in rows {
            let (id, data) = row?;
            let mut event: ActivityEvent = serde_json::from_str(&data)?;
            event.id = Some(id);
            activities.push(event);
        }
        Ok(activities)
    }

    pub fn count_events(&self) -> Result<i64, StoreError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE work_item_id = ?1",
            params![self.work_item_id.as_str()],
            |row| row.get(0),
        )?)
    }

    pub fn count_events_of_kind(&self, kind: &str) -> Result<i64, StoreError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE work_item_id = ?1 AND kind = ?2",
            params![self.work_item_id.as_str(), kind],
            |row| row.get(0),
        )?)
    }

    pub fn status_snapshot(&self) -> Result<StatusSnapshot, StoreError> {
        let (id, slug, repository_root) = self.conn.query_row(
            "SELECT w.id, w.slug, r.root
             FROM work_items w
             JOIN repositories r ON r.id = w.repository_id
             WHERE w.id = ?1",
            params![self.work_item_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        let state = self.current_state()?.unwrap_or(State::Intake);

        let (candidate_count, iterations): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(MAX(iteration) + 1, 0)
             FROM candidates WHERE work_item_id = ?1",
            params![self.work_item_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut planner_stmt = self.conn.prepare(
            "SELECT DISTINCT planner FROM candidates
             WHERE work_item_id = ?1 ORDER BY planner",
        )?;
        let planners = planner_stmt
            .query_map(params![self.work_item_id.as_str()], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let plan = self
            .conn
            .query_row(
                "SELECT text, metrics FROM plans WHERE work_item_id = ?1",
                params![self.work_item_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?;

        let mut implementation_stmt = self.conn.prepare(
            "SELECT r.iteration, r.status, r.start_commit, r.result_commit, r.tree_sha,
                    i.summary
             FROM implementation_rounds r
             LEFT JOIN implementations i
               ON i.work_item_id = r.work_item_id AND i.iteration = r.iteration
             WHERE r.work_item_id = ?1 ORDER BY r.iteration",
        )?;
        let implementations = implementation_stmt
            .query_map(params![self.work_item_id.as_str()], |row| {
                let start_commit = row.get::<_, String>(2)?;
                let result_commit = row.get::<_, Option<String>>(3)?;
                Ok(ImplementationSnapshot {
                    iteration: row.get::<_, i64>(0)? as u32,
                    status: row.get(1)?,
                    changed: result_commit.as_ref().map(|result| result != &start_commit),
                    start_commit,
                    result_commit,
                    tree_sha: row.get(4)?,
                    summary: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut review_stmt = self.conn.prepare(
            "SELECT iteration, accepted, text FROM reviews
             WHERE work_item_id = ?1 ORDER BY iteration",
        )?;
        let reviews = review_stmt
            .query_map(params![self.work_item_id.as_str()], |row| {
                Ok(ReviewSnapshot {
                    iteration: row.get::<_, i64>(0)? as u32,
                    accepted: row.get::<_, i64>(1)? != 0,
                    findings: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let worktree = self
            .conn
            .query_row(
                "SELECT path, branch, base_commit, status
                 FROM worktrees WHERE work_item_id = ?1",
                params![self.work_item_id.as_str()],
                |row| {
                    Ok(WorkspaceSnapshot {
                        path: row.get(0)?,
                        branch: Some(row.get(1)?),
                        base_commit: Some(row.get(2)?),
                        ready: row.get::<_, String>(3)? == "ready",
                        head: None,
                        clean: None,
                    })
                },
            )
            .optional()?
            .unwrap_or(WorkspaceSnapshot {
                path: String::new(),
                branch: None,
                base_commit: None,
                ready: false,
                head: None,
                clean: None,
            });

        let activities = self.activities()?;
        let mut errors = activities
            .iter()
            .filter(|event| matches!(event.kind, ActivityKind::AgentFailed | ActivityKind::Failed))
            .cloned()
            .collect::<Vec<_>>();
        if errors.is_empty() {
            let mut legacy_stmt = self.conn.prepare(
                "SELECT ts, data FROM events
                 WHERE work_item_id = ?1 AND kind = 'error' ORDER BY id",
            )?;
            for row in legacy_stmt.query_map(params![self.work_item_id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                ))
            })? {
                let (timestamp, message) = row?;
                errors.push(ActivityEvent {
                    id: None,
                    timestamp_ms: timestamp.parse().unwrap_or(0),
                    kind: ActivityKind::Failed,
                    message,
                    phase: Some(state),
                    role: None,
                    model: None,
                    iteration: None,
                    attempt: None,
                    elapsed_ms: None,
                });
            }
        }

        Ok(StatusSnapshot {
            version: 1,
            identity: WorkItemIdentitySnapshot {
                id,
                slug: slug.clone(),
                repository_root,
            },
            state: StateSnapshot {
                current: state,
                kind: state.kind(),
            },
            questions: self.questions()?,
            session_name: state.is_blocked().then(|| format!("quorum/{slug}/{state}")),
            transitions: self.history()?,
            planning: PlanningSnapshot {
                iterations: iterations as u32,
                candidate_count: candidate_count as u32,
                planners,
                plan: plan.as_ref().map(|value| value.0.clone()),
                metrics: plan.and_then(|value| value.1),
            },
            implementations,
            reviews,
            errors,
            activities,
            workspace: worktree,
        })
    }

    pub fn set_work_item(&mut self, text: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE work_items SET text = ?1, updated_at = ?2 WHERE id = ?3",
            params![text, now_millis(), self.work_item_id.as_str()],
        )?;
        Ok(())
    }

    pub fn work_item(&self) -> Result<Option<String>, StoreError> {
        self.conn
            .query_row(
                "SELECT text FROM work_items WHERE id = ?1",
                params![self.work_item_id.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(Into::into)
    }

    pub fn save_candidate(
        &mut self,
        planner: &str,
        iteration: u32,
        text: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO candidates (work_item_id, planner, iteration, text)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(work_item_id, planner, iteration)
             DO UPDATE SET text = excluded.text",
            params![self.work_item_id.as_str(), planner, iteration, text],
        )?;
        Ok(())
    }

    pub fn candidates(&self, iteration: u32) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT planner, text FROM candidates
             WHERE work_item_id = ?1 AND iteration = ?2 ORDER BY planner ASC",
        )?;
        let rows = stmt.query_map(params![self.work_item_id.as_str(), iteration], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn max_candidate_iteration(&self) -> Result<Option<u32>, StoreError> {
        let value: Option<i64> = self.conn.query_row(
            "SELECT MAX(iteration) FROM candidates WHERE work_item_id = ?1",
            params![self.work_item_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(value.map(|n| n as u32))
    }

    pub fn set_plan(&mut self, text: &str, metrics: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO plans (work_item_id, text, metrics) VALUES (?1, ?2, ?3)
             ON CONFLICT(work_item_id)
             DO UPDATE SET text = excluded.text, metrics = excluded.metrics",
            params![self.work_item_id.as_str(), text, metrics],
        )?;
        Ok(())
    }

    pub fn plan(&self) -> Result<Option<String>, StoreError> {
        optional_string(
            &self.conn,
            "SELECT text FROM plans WHERE work_item_id = ?1",
            self.work_item_id.as_str(),
        )
    }

    pub fn save_implementation(&mut self, iteration: u32, summary: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO implementations (work_item_id, iteration, summary, ts)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(work_item_id, iteration)
             DO UPDATE SET summary = excluded.summary, ts = excluded.ts",
            params![self.work_item_id.as_str(), iteration, summary, now_millis()],
        )?;
        Ok(())
    }

    pub fn implementation_round(
        &self,
        iteration: u32,
    ) -> Result<Option<ImplementationRound>, StoreError> {
        match self.conn.query_row(
            "SELECT start_commit, status, result_commit, tree_sha
             FROM implementation_rounds
             WHERE work_item_id = ?1 AND iteration = ?2",
            params![self.work_item_id.as_str(), iteration],
            |row| {
                let status: String = row.get(1)?;
                Ok((
                    row.get::<_, String>(0)?,
                    status,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        ) {
            Ok((start_commit, status, result_commit, tree_sha)) => {
                let status = ImplementationRoundStatus::from_str(&status)
                    .ok_or(StoreError::InvalidRoundStatus(status))?;
                Ok(Some(ImplementationRound {
                    iteration,
                    start_commit,
                    status,
                    result_commit,
                    tree_sha,
                }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn reserve_implementation_round(
        &mut self,
        iteration: u32,
        start_commit: &str,
    ) -> Result<ImplementationRound, StoreError> {
        let ts = now_millis();
        self.conn.execute(
            "INSERT INTO implementation_rounds
             (work_item_id, iteration, start_commit, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'running', ?4, ?4)",
            params![self.work_item_id.as_str(), iteration, start_commit, ts],
        )?;
        Ok(ImplementationRound {
            iteration,
            start_commit: start_commit.to_string(),
            status: ImplementationRoundStatus::Running,
            result_commit: None,
            tree_sha: None,
        })
    }

    pub fn mark_implementation_agent_complete(
        &mut self,
        iteration: u32,
        summary: &str,
    ) -> Result<(), StoreError> {
        let ts = now_millis();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO implementations (work_item_id, iteration, summary, ts)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(work_item_id, iteration)
             DO UPDATE SET summary = excluded.summary, ts = excluded.ts",
            params![self.work_item_id.as_str(), iteration, summary, ts],
        )?;
        tx.execute(
            "UPDATE implementation_rounds
             SET status = 'agent_complete', updated_at = ?1
             WHERE work_item_id = ?2 AND iteration = ?3",
            params![ts, self.work_item_id.as_str(), iteration],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn complete_implementation_round(
        &mut self,
        iteration: u32,
        result_commit: &str,
        tree_sha: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE implementation_rounds
             SET status = 'committed', result_commit = ?1, tree_sha = ?2, updated_at = ?3
             WHERE work_item_id = ?4 AND iteration = ?5",
            params![
                result_commit,
                tree_sha,
                now_millis(),
                self.work_item_id.as_str(),
                iteration
            ],
        )?;
        Ok(())
    }

    pub fn implementation_tree(&self, iteration: u32) -> Result<Option<String>, StoreError> {
        match self.conn.query_row(
            "SELECT tree_sha FROM implementation_rounds
             WHERE work_item_id = ?1 AND iteration = ?2 AND status = 'committed'",
            params![self.work_item_id.as_str(), iteration],
            |row| row.get::<_, Option<String>>(0),
        ) {
            Ok(tree) => Ok(tree),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn latest_implementation(&self) -> Result<Option<(u32, String)>, StoreError> {
        match self.conn.query_row(
            "SELECT iteration, summary FROM implementations
             WHERE work_item_id = ?1 ORDER BY iteration DESC LIMIT 1",
            params![self.work_item_id.as_str()],
            |row| Ok((row.get::<_, i64>(0)? as u32, row.get::<_, String>(1)?)),
        ) {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn latest_review(&self) -> Result<Option<(String, bool)>, StoreError> {
        match self.conn.query_row(
            "SELECT text, accepted FROM reviews
             WHERE work_item_id = ?1 ORDER BY iteration DESC LIMIT 1",
            params![self.work_item_id.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0)),
        ) {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn review_count(&self) -> Result<u32, StoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM reviews WHERE work_item_id = ?1",
            params![self.work_item_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(count as u32)
    }

    pub fn answers(&self) -> Result<String, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT data FROM events
             WHERE work_item_id = ?1 AND kind = 'hi_answer' ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![self.work_item_id.as_str()], |row| {
            row.get::<_, Option<String>>(0)
        })?;
        let mut parts = Vec::new();
        for row in rows {
            if let Some(text) = row? {
                parts.push(text);
            }
        }
        Ok(parts.join("\n\n"))
    }

    pub fn set_questions(&mut self, questions: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO intake (work_item_id, questions, ts) VALUES (?1, ?2, ?3)
             ON CONFLICT(work_item_id)
             DO UPDATE SET questions = excluded.questions, ts = excluded.ts",
            params![self.work_item_id.as_str(), questions, now_millis()],
        )?;
        Ok(())
    }

    pub fn questions(&self) -> Result<Option<String>, StoreError> {
        optional_string(
            &self.conn,
            "SELECT questions FROM intake WHERE work_item_id = ?1",
            self.work_item_id.as_str(),
        )
    }

    pub fn record_session(&mut self, state: State, name: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO sessions (work_item_id, state, session_name, ts)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(work_item_id, state)
             DO UPDATE SET session_name = excluded.session_name, ts = excluded.ts",
            params![
                self.work_item_id.as_str(),
                state.as_str(),
                name,
                now_millis()
            ],
        )?;
        Ok(())
    }

    pub fn session(&self, state: State) -> Result<Option<String>, StoreError> {
        match self.conn.query_row(
            "SELECT session_name FROM sessions
             WHERE work_item_id = ?1 AND state = ?2",
            params![self.work_item_id.as_str(), state.as_str()],
            |row| row.get::<_, String>(0),
        ) {
            Ok(name) => Ok(Some(name)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save_review(
        &mut self,
        iteration: u32,
        text: &str,
        accepted: bool,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO reviews (work_item_id, iteration, text, accepted)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(work_item_id, iteration)
             DO UPDATE SET text = excluded.text, accepted = excluded.accepted",
            params![self.work_item_id.as_str(), iteration, text, accepted as i64],
        )?;
        Ok(())
    }
}

fn initialize_connection(conn: &Connection) -> Result<(), StoreError> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

fn schema_version(conn: &Connection) -> Result<i64, StoreError> {
    let value: String = conn.query_row(
        "SELECT value FROM meta WHERE key = 'schema_version'",
        [],
        |row| row.get(0),
    )?;
    Ok(value.parse().unwrap_or(0))
}

fn optional_string(
    conn: &Connection,
    sql: &str,
    work_item_id: &str,
) -> Result<Option<String>, StoreError> {
    match conn.query_row(sql, params![work_item_id], |row| row.get::<_, String>(0)) {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn now_millis() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
        .to_string()
}

fn activity_kind_name(kind: ActivityKind) -> &'static str {
    match kind {
        ActivityKind::PhaseStarted => "phase_started",
        ActivityKind::AgentStarted => "agent_started",
        ActivityKind::AgentRetrying => "agent_retrying",
        ActivityKind::AgentCompleted => "agent_completed",
        ActivityKind::AgentFailed => "agent_failed",
        ActivityKind::Convergence => "convergence",
        ActivityKind::ImplementationRound => "implementation_round",
        ActivityKind::Review => "review",
        ActivityKind::Transition => "transition",
        ActivityKind::HumanIntervention => "human_intervention",
        ActivityKind::Completed => "completed",
        ActivityKind::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn register(db: &mut Database, root: &str) -> RegisteredRepository {
        db.register_repository(&RepositoryRoot::from_canonical(root))
            .unwrap()
    }

    #[test]
    fn opens_and_migrates() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let foreign_keys: i64 = db
            .conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        let busy_timeout: i64 = db
            .conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);
        assert_eq!(busy_timeout, BUSY_TIMEOUT.as_millis() as i64);
    }

    #[test]
    fn file_database_uses_wal() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("quorum.db")).unwrap();
        let journal_mode: String = db
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quorum.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta (key, value) VALUES ('schema_version', '999');",
        )
        .unwrap();
        drop(conn);

        let error = Database::open(&path).err().unwrap();
        assert!(matches!(
            error,
            StoreError::UnsupportedSchema {
                found: 999,
                expected: SCHEMA_VERSION
            }
        ));
    }

    #[test]
    fn migrates_global_v1_without_losing_work_item_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quorum.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta (key, value) VALUES ('schema_version', '1');
             CREATE TABLE work_items (
                 id TEXT PRIMARY KEY,
                 slug TEXT NOT NULL UNIQUE,
                 text TEXT,
                 source TEXT,
                 origin_repo TEXT,
                 origin_issue INTEGER,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE TABLE states (
                 work_item_id TEXT PRIMARY KEY REFERENCES work_items(id) ON DELETE CASCADE,
                 state TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             INSERT INTO work_items
                 (id, slug, text, created_at, updated_at)
             VALUES ('legacy-wi', 'legacy', '# Legacy', '1', '1');
             INSERT INTO states (work_item_id, state, updated_at)
             VALUES ('legacy-wi', 'Planning', '1');",
        )
        .unwrap();
        drop(conn);

        let db = Database::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let store = db.into_store(WorkItemId("legacy-wi".to_string())).unwrap();
        assert_eq!(store.work_item().unwrap().as_deref(), Some("# Legacy"));
        assert_eq!(store.current_state().unwrap(), Some(State::Planning));
    }

    #[test]
    fn migration_recheck_tolerates_an_already_upgraded_database() {
        let db = Database::open_in_memory().unwrap();
        db.migrate_v1_to_v2().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn migrates_v3_by_adding_implementation_rounds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quorum.db");
        let db = Database::open(&path).unwrap();
        db.conn
            .execute_batch(
                "DROP TABLE implementation_rounds;
                 UPDATE meta SET value = '3' WHERE key = 'schema_version';",
            )
            .unwrap();
        drop(db);

        let db = Database::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let table: String = db
            .conn
            .query_row(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name = 'implementation_rounds'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table, "implementation_rounds");
    }

    #[test]
    fn migrates_v4_by_adding_activities() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quorum.db");
        let db = Database::open(&path).unwrap();
        db.conn
            .execute_batch(
                "DROP TABLE activities;
                 UPDATE meta SET value = '4' WHERE key = 'schema_version';",
            )
            .unwrap();
        drop(db);

        let db = Database::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let table: String = db
            .conn
            .query_row(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name = 'activities'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table, "activities");
    }

    #[test]
    fn get_or_create_returns_stable_id() {
        let mut db = Database::open_in_memory().unwrap();
        let repository = register(&mut db, "/repo");
        let first = db
            .get_or_create_work_item(&repository.id, "example")
            .unwrap();
        let second = db
            .get_or_create_work_item(&repository.id, "example")
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(
            db.work_item_id(&repository.id, "example").unwrap(),
            Some(first)
        );
    }

    #[test]
    fn persists_worktree_intent_and_ready_state() {
        let mut db = Database::open_in_memory().unwrap();
        let repository = register(&mut db, "/repo");
        let work_item = db
            .get_or_create_work_item(&repository.id, "example")
            .unwrap();
        let path = Path::new("/state/example/implementation");

        let creating = db
            .reserve_worktree(&work_item, "abc123", "quorum/example-12345678", path)
            .unwrap();
        assert!(!creating.ready);
        assert_eq!(db.worktree(&work_item).unwrap(), Some(creating.clone()));

        db.mark_worktree_ready(&work_item).unwrap();
        let ready = db.worktree(&work_item).unwrap().unwrap();
        assert!(ready.ready);
        assert_eq!(ready.base_commit, "abc123");
        assert_eq!(ready.path, path);
    }

    #[test]
    fn registration_is_idempotent_and_reactivation_keeps_identity() {
        let mut db = Database::open_in_memory().unwrap();
        let first = register(&mut db, "/repo");
        let second = register(&mut db, "/repo");
        assert_eq!(first, second);
        assert_eq!(db.repositories().unwrap(), vec![first.clone()]);

        assert_eq!(
            db.unregister_repository(&RepositoryRoot::from_canonical("/repo"))
                .unwrap(),
            Some(first.clone())
        );
        assert!(db.repositories().unwrap().is_empty());

        let reactivated = register(&mut db, "/repo");
        assert_eq!(reactivated, first);
    }

    #[test]
    fn records_and_reads_back_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quorum.db");
        let id = {
            let mut db = Database::open(&path).unwrap();
            let repository = register(&mut db, "/repo");
            let id = db
                .get_or_create_work_item(&repository.id, "example")
                .unwrap();
            let mut store = db.into_store(id.clone()).unwrap();
            store
                .record_transition(Some(State::Intake), State::Planning, "auto")
                .unwrap();
            store
                .record_transition(Some(State::Planning), State::Converging, "auto")
                .unwrap();
            id
        };

        let db = Database::open(&path).unwrap();
        let store = db.into_store(id).unwrap();
        assert_eq!(store.current_state().unwrap(), Some(State::Converging));
        let history = store.history().unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].from, Some(State::Intake));
        assert_eq!(history[1].to, State::Converging);
    }

    #[test]
    fn work_items_are_isolated_in_one_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quorum.db");

        let (one, two) = {
            let mut db = Database::open(&path).unwrap();
            let repository_one = register(&mut db, "/repo/one");
            let repository_two = register(&mut db, "/repo/two");
            let one = db
                .get_or_create_work_item(&repository_one.id, "same-slug")
                .unwrap();
            let two = db
                .get_or_create_work_item(&repository_two.id, "same-slug")
                .unwrap();
            (one, two)
        };

        {
            let db = Database::open(&path).unwrap();
            let mut store = db.into_store(one.clone()).unwrap();
            store.set_work_item("# One").unwrap();
            store.save_candidate("planner", 0, "one plan").unwrap();
            store
                .record_transition(Some(State::Intake), State::Planning, "one")
                .unwrap();
        }
        {
            let db = Database::open(&path).unwrap();
            let mut store = db.into_store(two.clone()).unwrap();
            store.set_work_item("# Two").unwrap();
            store.save_candidate("planner", 0, "two plan").unwrap();
        }

        let one_store = Database::open(&path).unwrap().into_store(one).unwrap();
        let two_store = Database::open(&path).unwrap().into_store(two).unwrap();
        assert_eq!(one_store.work_item().unwrap().as_deref(), Some("# One"));
        assert_eq!(two_store.work_item().unwrap().as_deref(), Some("# Two"));
        assert_eq!(one_store.candidates(0).unwrap()[0].1, "one plan");
        assert_eq!(two_store.candidates(0).unwrap()[0].1, "two plan");
        assert_eq!(one_store.current_state().unwrap(), Some(State::Planning));
        assert_eq!(two_store.current_state().unwrap(), None);
        assert_eq!(one_store.count_events().unwrap(), 1);
        assert_eq!(two_store.count_events().unwrap(), 0);
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

        assert_eq!(
            store.candidates(0).unwrap(),
            vec![
                ("planner-a".to_string(), "plan A".to_string()),
                ("planner-b".to_string(), "plan B".to_string()),
            ]
        );
        assert_eq!(store.candidates(1).unwrap().len(), 1);
    }

    #[test]
    fn persists_implementation_round_lifecycle_and_tree() {
        let mut store = Store::open_in_memory().unwrap();
        let running = store.reserve_implementation_round(0, "start").unwrap();
        assert_eq!(running.status, ImplementationRoundStatus::Running);
        assert_eq!(store.implementation_round(0).unwrap(), Some(running));

        store
            .mark_implementation_agent_complete(0, "implemented")
            .unwrap();
        let agent_complete = store.implementation_round(0).unwrap().unwrap();
        assert_eq!(
            agent_complete.status,
            ImplementationRoundStatus::AgentComplete
        );
        assert_eq!(
            store.latest_implementation().unwrap(),
            Some((0, "implemented".to_string()))
        );

        store
            .complete_implementation_round(0, "result", "tree")
            .unwrap();
        let committed = store.implementation_round(0).unwrap().unwrap();
        assert_eq!(committed.status, ImplementationRoundStatus::Committed);
        assert_eq!(committed.result_commit.as_deref(), Some("result"));
        assert_eq!(committed.tree_sha.as_deref(), Some("tree"));
        assert_eq!(
            store.implementation_tree(0).unwrap().as_deref(),
            Some("tree")
        );
    }

    #[test]
    fn status_snapshot_assembles_scoped_observability_data() {
        let mut db = Database::open_in_memory().unwrap();
        let repository = register(&mut db, "/repo");
        let work_item = db
            .get_or_create_work_item(&repository.id, "observable")
            .unwrap();
        db.reserve_worktree(
            &work_item,
            "base",
            "quorum/observable",
            Path::new("/state/observable/implementation"),
        )
        .unwrap();
        db.mark_worktree_ready(&work_item).unwrap();
        let mut store = db.into_store(work_item.clone()).unwrap();
        store.set_work_item("# Observable").unwrap();
        store.save_candidate("planner-a", 0, "candidate").unwrap();
        store.set_plan("the plan", "iteration=0").unwrap();
        store
            .record_transition(Some(State::Intake), State::Planning, "start")
            .unwrap();
        store
            .record_activity(
                &ActivityEvent::new(ActivityKind::AgentStarted, "planner-a started")
                    .phase(State::Planning)
                    .role("PL:planner-a")
                    .iteration(0),
            )
            .unwrap();

        let snapshot = store.status_snapshot().unwrap();
        assert_eq!(snapshot.version, 1);
        assert_eq!(snapshot.identity.id, work_item.as_str());
        assert_eq!(snapshot.identity.slug, "observable");
        assert_eq!(snapshot.identity.repository_root, "/repo");
        assert_eq!(snapshot.state.current, State::Planning);
        assert_eq!(snapshot.planning.iterations, 1);
        assert_eq!(snapshot.planning.candidate_count, 1);
        assert_eq!(snapshot.planning.plan.as_deref(), Some("the plan"));
        assert_eq!(snapshot.activities.len(), 1);
        assert_eq!(
            snapshot.workspace.branch.as_deref(),
            Some("quorum/observable")
        );
    }
}
