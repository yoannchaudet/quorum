//! Global SQLite persistence. Mirrors `docs/persistence.md`.
//!
//! Quorum stores all structured state in one database. [`Database`] owns
//! catalog-level operations, while [`Store`] scopes every coordinator query to
//! one work item so data cannot leak across runs.

use crate::observability::{
    ActivityEvent, ActivityKind, ArtifactSnapshot, DeliverySnapshot, ImplementationSnapshot,
    PlanningSnapshot, ReviewSnapshot, StateSnapshot, StatusSnapshot, WorkItemIdentitySnapshot,
    WorkspaceSnapshot,
};
use crate::repository::RepositoryRoot;
use crate::repository::WorktreeStart;
use crate::state::State;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// The global database schema version.
pub const SCHEMA_VERSION: i64 = 9;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const DISPLAY_REFERENCE_LENGTH: usize = 8;

/// Stable internal identity for a work item.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkItemId(String);

impl WorkItemId {
    pub(crate) fn new() -> WorkItemId {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItemSummary {
    pub id: WorkItemId,
    pub reference: String,
    pub label: String,
    pub state: State,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkItem {
    pub id: WorkItemId,
    pub reference: String,
    pub label: String,
}

/// Persisted intent and lifecycle state for a work item's linked Git worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRecord {
    pub work_item_id: WorkItemId,
    pub requested_base: Option<String>,
    pub base_commit: String,
    pub branch: String,
    pub path: std::path::PathBuf,
    pub delivery_remote: Option<String>,
    pub target_branch: Option<String>,
    pub ready: bool,
}

/// All fields that must commit together when first reserving a work item.
pub(crate) struct NewWorkItemWorktreeIntent<'a> {
    pub id: WorkItemId,
    pub slug: &'a str,
    pub text: &'a str,
    pub start: &'a WorktreeStart,
    pub branch: &'a str,
    pub path: &'a Path,
}

/// A crash-recoverable checkpoint for Git/GitHub delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Pending,
    Pushed,
    PullRequestCreated,
}

impl DeliveryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryStatus::Pending => "pending",
            DeliveryStatus::Pushed => "pushed",
            DeliveryStatus::PullRequestCreated => "pull_request_created",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "pushed" => Some(Self::Pushed),
            "pull_request_created" => Some(Self::PullRequestCreated),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRecord {
    pub status: DeliveryStatus,
    pub final_head_commit: String,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub iteration: u32,
    pub path: String,
    pub media_type: String,
    pub created_at: String,
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
    #[error("work-item reference {reference:?} is invalid; use a UUID or leading UUID prefix")]
    InvalidWorkItemReference { reference: String },
    #[error(
        "work-item reference {reference:?} is ambiguous in this repository; use more UUID characters"
    )]
    AmbiguousWorkItemReference { reference: String },
    #[error("database schema version {found} is not supported (expected {expected})")]
    UnsupportedSchema { found: i64, expected: i64 },
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(std::path::PathBuf),
    #[error("stored implementation round status {0:?} is invalid")]
    InvalidRoundStatus(String),
    #[error("stored delivery status {0:?} is invalid")]
    InvalidDeliveryStatus(String),
    #[error("delivery settings are already persisted and cannot be changed")]
    DeliverySettingsAlreadyPersisted,
    #[error("a delivered pull request requires a nonzero number and URL")]
    InvalidDeliveryHandoff,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("artifact filesystem error at {path}: {source}")]
    ArtifactIo {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
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
        if (1..8).contains(&stored_version) {
            self.migrate_v7_to_v8()?;
        }
        if stored_version < 9 {
            self.migrate_v8_to_v9()?;
        }
        if (1..6).contains(&stored_version) {
            self.migrate_activity_role_names()?;
        }
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    fn migrate_activity_role_names(&self) -> Result<(), StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT id, data FROM activities ORDER BY id")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);

        let transaction = self.conn.unchecked_transaction()?;
        for (id, data) in rows {
            let mut event: ActivityEvent = serde_json::from_str(&data)?;
            normalize_legacy_activity(&mut event);
            transaction.execute(
                "UPDATE activities SET data = ?1 WHERE id = ?2",
                params![serde_json::to_string(&event)?, id],
            )?;
        }
        transaction.execute(
            "UPDATE events SET kind = 'human_answer' WHERE kind = 'hi_answer'",
            [],
        )?;
        transaction.execute(
            "UPDATE events SET kind = 'human_decision' WHERE kind = 'hi_decision'",
            [],
        )?;
        transaction.execute(
            "UPDATE transitions
             SET reason = 'human:' || substr(reason, 4)
             WHERE reason LIKE 'hi:%'",
            [],
        )?;
        transaction.commit()?;
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
                updated_at   TEXT NOT NULL
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
                requested_base TEXT,
                base_commit  TEXT NOT NULL,
                branch       TEXT NOT NULL,
                path         TEXT NOT NULL UNIQUE,
                delivery_remote TEXT,
                target_branch TEXT,
                status       TEXT NOT NULL CHECK (status IN ('creating', 'ready')),
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS deliveries (
                work_item_id TEXT PRIMARY KEY REFERENCES work_items(id) ON DELETE CASCADE,
                status       TEXT NOT NULL CHECK (status IN (
                    'pending', 'pushed', 'pull_request_created'
                )),
                final_head_commit TEXT NOT NULL,
                pr_number    INTEGER,
                pr_url       TEXT,
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

            CREATE TABLE IF NOT EXISTS artifacts (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
                iteration    INTEGER NOT NULL,
                path         TEXT NOT NULL,
                media_type   TEXT NOT NULL,
                created_at   TEXT NOT NULL,
                UNIQUE(work_item_id, path)
            );

            CREATE INDEX IF NOT EXISTS artifacts_work_item
                ON artifacts(work_item_id, id);
            "#,
        )?;
        Ok(())
    }

    fn migrate_v7_to_v8(&self) -> Result<(), StoreError> {
        self.conn.pragma_update(None, "foreign_keys", false)?;
        let result = self.conn.execute_batch(
            r#"
            BEGIN IMMEDIATE;

            CREATE TABLE work_items_v8 (
                id            TEXT PRIMARY KEY,
                repository_id TEXT REFERENCES repositories(id),
                slug          TEXT NOT NULL,
                text          TEXT,
                source        TEXT,
                origin_repo   TEXT,
                origin_issue  INTEGER,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
            );

            INSERT INTO work_items_v8
                (id, repository_id, slug, text, source, origin_repo, origin_issue, created_at, updated_at)
            SELECT id, repository_id, slug, text, source, origin_repo, origin_issue, created_at, updated_at
            FROM work_items;

            DROP TABLE work_items;
            ALTER TABLE work_items_v8 RENAME TO work_items;

            COMMIT;
            "#,
        );
        if result.is_err() {
            let _ = self.conn.execute_batch("ROLLBACK;");
        }

        let foreign_keys = self.conn.pragma_update(None, "foreign_keys", true);
        result?;
        foreign_keys?;
        Ok(())
    }

    fn migrate_v8_to_v9(&self) -> Result<(), StoreError> {
        // SQLite only gained conditional ADD COLUMN much later than the versions
        // shipped with common platforms, so inspect first for idempotent recovery.
        let mut columns = self.conn.prepare("PRAGMA table_info(worktrees)")?;
        let names = columns
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(columns);
        if !names.iter().any(|name| name == "requested_base") {
            self.conn
                .execute("ALTER TABLE worktrees ADD COLUMN requested_base TEXT", [])?;
        }
        if !names.iter().any(|name| name == "delivery_remote") {
            self.conn
                .execute("ALTER TABLE worktrees ADD COLUMN delivery_remote TEXT", [])?;
        }
        if !names.iter().any(|name| name == "target_branch") {
            self.conn
                .execute("ALTER TABLE worktrees ADD COLUMN target_branch TEXT", [])?;
        }
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS deliveries (
                work_item_id TEXT PRIMARY KEY REFERENCES work_items(id) ON DELETE CASCADE,
                status TEXT NOT NULL CHECK (status IN ('pending', 'pushed', 'pull_request_created')),
                final_head_commit TEXT NOT NULL,
                pr_number INTEGER,
                pr_url TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
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

    /// Deactivate a repository without deleting its identity or work-item history.
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

    /// Find the oldest work item with an exact repository-scoped label.
    ///
    /// This is retained for internal setup and migration helpers. User-facing
    /// commands resolve UUID references with [`Database::resolve_work_item`].
    pub fn work_item_id(
        &self,
        repository_id: &RepositoryId,
        slug: &str,
    ) -> Result<Option<WorkItemId>, StoreError> {
        match self.conn.query_row(
            "SELECT id FROM work_items
             WHERE repository_id = ?1 AND slug = ?2
             ORDER BY created_at, id
             LIMIT 1",
            params![repository_id.as_str(), slug],
            |row| row.get::<_, String>(0),
        ) {
            Ok(id) => Ok(Some(WorkItemId(id))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Resolve a full UUID or unique UUID prefix within one repository.
    pub fn resolve_work_item(
        &self,
        repository_id: &RepositoryId,
        reference: &str,
    ) -> Result<Option<ResolvedWorkItem>, StoreError> {
        if reference.is_empty()
            || reference.len() > 36
            || !reference
                .chars()
                .all(|character| character.is_ascii_hexdigit() || character == '-')
        {
            return Err(StoreError::InvalidWorkItemReference {
                reference: reference.to_string(),
            });
        }
        let normalized = reference.to_ascii_lowercase();
        let mut statement = self.conn.prepare(
            "SELECT id, slug FROM work_items
             WHERE repository_id = ?1
               AND substr(id, 1, length(?2)) = ?2
             ORDER BY id
             LIMIT 2",
        )?;
        let matches = statement
            .query_map(params![repository_id.as_str(), normalized], |row| {
                Ok(ResolvedWorkItem {
                    id: WorkItemId(row.get(0)?),
                    reference: String::new(),
                    label: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        match matches.as_slice() {
            [] => Ok(None),
            [work_item] => {
                let mut work_item = work_item.clone();
                work_item.reference = self.work_item_reference(&work_item.id)?;
                Ok(Some(work_item))
            }
            _ => Err(StoreError::AmbiguousWorkItemReference {
                reference: reference.to_string(),
            }),
        }
    }

    /// Return the shortest repository-unique UUID prefix, using at least eight characters.
    pub fn work_item_reference(&self, work_item_id: &WorkItemId) -> Result<String, StoreError> {
        repository_unique_reference(&self.conn, work_item_id)
    }

    /// Return the existing work item for `(repository, slug)`, or create an empty one.
    pub fn get_or_create_work_item(
        &mut self,
        repository_id: &RepositoryId,
        slug: &str,
    ) -> Result<WorkItemId, StoreError> {
        if let Some(id) = self.work_item_id(repository_id, slug)? {
            return Ok(id);
        }
        let id = WorkItemId::new();
        let ts = now_millis();
        self.conn.execute(
            "INSERT INTO work_items (id, repository_id, slug, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id.as_str(), repository_id.as_str(), slug, ts],
        )?;
        Ok(id)
    }

    /// Create a new repository-scoped work item. Labels are intentionally non-unique.
    pub fn create_work_item(
        &mut self,
        repository_id: &RepositoryId,
        slug: &str,
        text: &str,
    ) -> Result<WorkItemId, StoreError> {
        let id = WorkItemId::new();
        let ts = now_millis();
        self.conn.execute(
            "INSERT INTO work_items (id, repository_id, slug, text, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![id.as_str(), repository_id.as_str(), slug, text, ts],
        )?;
        Ok(id)
    }

    /// Atomically create a work item and reserve its immutable worktree intent.
    ///
    /// This transaction is deliberately completed before any Git mutation so a
    /// crash can only leave a recoverable `creating` reservation, never a row
    /// that might later select a different base or delivery destination.
    pub(crate) fn create_work_item_with_worktree_intent(
        &mut self,
        repository_id: &RepositoryId,
        intent: NewWorkItemWorktreeIntent<'_>,
    ) -> Result<WorktreeRecord, StoreError> {
        let path_text = intent
            .path
            .to_str()
            .ok_or_else(|| StoreError::NonUtf8Path(intent.path.to_path_buf()))?;
        let ts = now_millis();
        let transaction = self.conn.transaction()?;
        transaction.execute(
            "INSERT INTO work_items (id, repository_id, slug, text, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![
                intent.id.as_str(),
                repository_id.as_str(),
                intent.slug,
                intent.text,
                ts
            ],
        )?;
        transaction.execute(
            "INSERT INTO worktrees
             (work_item_id, requested_base, base_commit, branch, path, delivery_remote, target_branch, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'creating', ?8, ?8)",
            params![
                intent.id.as_str(),
                intent.start.requested_base,
                intent.start.base_commit,
                intent.branch,
                path_text,
                intent.start.delivery_remote,
                intent.start.target_branch,
                ts
            ],
        )?;
        transaction.commit()?;
        Ok(WorktreeRecord {
            work_item_id: intent.id,
            requested_base: Some(intent.start.requested_base.clone()),
            base_commit: intent.start.base_commit.clone(),
            branch: intent.branch.to_string(),
            path: intent.path.to_path_buf(),
            delivery_remote: Some(intent.start.delivery_remote.clone()),
            target_branch: Some(intent.start.target_branch.clone()),
            ready: false,
        })
    }

    /// List work items for one repository, most recently active first.
    pub fn work_items(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<Vec<WorkItemSummary>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT w.id, w.slug, COALESCE(s.state, 'Intake'),
                    COALESCE(
                        (SELECT MAX(a.ts) FROM activities a WHERE a.work_item_id = w.id),
                        CAST(s.updated_at AS INTEGER),
                        CAST(w.updated_at AS INTEGER)
                    ) AS latest
             FROM work_items w
             LEFT JOIN states s ON s.work_item_id = w.id
             WHERE w.repository_id = ?1
             ORDER BY latest DESC, w.slug ASC",
        )?;
        let rows = stmt.query_map(params![repository_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut summaries = Vec::new();
        for row in rows {
            let (id, slug, state, updated_at) = row?;
            let state = State::from_db_str(&state).ok_or(StoreError::UnknownState(state))?;
            summaries.push(WorkItemSummary {
                reference: self.work_item_reference(&WorkItemId(id.clone()))?,
                id: WorkItemId(id),
                label: slug,
                state,
                updated_at: updated_at as u64,
            });
        }
        Ok(summaries)
    }

    /// The persisted worktree intent for a work item, if setup has begun.
    pub fn worktree(
        &self,
        work_item_id: &WorkItemId,
    ) -> Result<Option<WorktreeRecord>, StoreError> {
        match self.conn.query_row(
            "SELECT requested_base, base_commit, branch, path, delivery_remote, target_branch, status
             FROM worktrees WHERE work_item_id = ?1",
            params![work_item_id.as_str()],
            |row| {
                Ok(WorktreeRecord {
                    work_item_id: work_item_id.clone(),
                    requested_base: row.get(0)?,
                    base_commit: row.get(1)?,
                    branch: row.get(2)?,
                    path: std::path::PathBuf::from(row.get::<_, String>(3)?),
                    delivery_remote: row.get(4)?,
                    target_branch: row.get(5)?,
                    ready: row.get::<_, String>(6)? == "ready",
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
             (work_item_id, requested_base, base_commit, branch, path, delivery_remote, target_branch, status, created_at, updated_at)
             VALUES (?1, 'HEAD', ?2, ?3, ?4, NULL, NULL, 'creating', ?5, ?5)",
            params![work_item_id.as_str(), base_commit, branch, path_text, ts],
        )?;
        Ok(WorktreeRecord {
            work_item_id: work_item_id.clone(),
            requested_base: Some("HEAD".to_string()),
            base_commit: base_commit.to_string(),
            branch: branch.to_string(),
            path: path.to_path_buf(),
            delivery_remote: None,
            target_branch: None,
            ready: false,
        })
    }

    /// Persist source and delivery intent before creating a linked worktree.
    pub fn reserve_worktree_with_delivery(
        &mut self,
        work_item_id: &WorkItemId,
        start: &WorktreeStart,
        branch: &str,
        path: &Path,
    ) -> Result<WorktreeRecord, StoreError> {
        let path_text = path
            .to_str()
            .ok_or_else(|| StoreError::NonUtf8Path(path.to_path_buf()))?;
        let ts = now_millis();
        self.conn.execute(
            "INSERT INTO worktrees
             (work_item_id, requested_base, base_commit, branch, path, delivery_remote, target_branch, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'creating', ?8, ?8)",
            params![
                work_item_id.as_str(),
                start.requested_base,
                start.base_commit,
                branch,
                path_text,
                start.delivery_remote,
                start.target_branch,
                ts
            ],
        )?;
        Ok(WorktreeRecord {
            work_item_id: work_item_id.clone(),
            requested_base: Some(start.requested_base.clone()),
            base_commit: start.base_commit.clone(),
            branch: branch.to_string(),
            path: path.to_path_buf(),
            delivery_remote: Some(start.delivery_remote.clone()),
            target_branch: Some(start.target_branch.clone()),
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
    /// Open a fresh in-memory database with one empty work item. Intended for tests.
    pub fn open_in_memory() -> Result<Store, StoreError> {
        let mut db = Database::open_in_memory()?;
        let root = RepositoryRoot::from_canonical("/test/repository");
        let repository = db.register_repository(&root)?;
        let id = db.get_or_create_work_item(&repository.id, "test-work-item")?;
        db.into_store(id)
    }

    /// The stable internal identity of this store's work item.
    pub fn work_item_id(&self) -> &WorkItemId {
        &self.work_item_id
    }

    /// The shortest repository-unique UUID prefix, using at least eight characters.
    pub fn work_item_reference(&self) -> Result<String, StoreError> {
        repository_unique_reference(&self.conn, &self.work_item_id)
    }

    /// The global schema version recorded by this connection.
    pub fn schema_version(&self) -> Result<i64, StoreError> {
        schema_version(&self.conn)
    }

    /// Read the current state for this work item.
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

    /// Read the delivery intent retained with this work item's worktree.
    pub fn worktree(&self) -> Result<Option<WorktreeRecord>, StoreError> {
        self.conn
                .query_row(
                    "SELECT requested_base, base_commit, branch, path, delivery_remote, target_branch, status
                     FROM worktrees WHERE work_item_id = ?1",
                    params![self.work_item_id.as_str()],
                    |row| {
                        Ok(WorktreeRecord {
                            work_item_id: self.work_item_id.clone(),
                            requested_base: row.get(0)?,
                            base_commit: row.get(1)?,
                            branch: row.get(2)?,
                            path: std::path::PathBuf::from(row.get::<_, String>(3)?),
                            delivery_remote: row.get(4)?,
                            target_branch: row.get(5)?,
                            ready: row.get::<_, String>(6)? == "ready",
                        })
                    },
                )
                .optional()
                .map_err(StoreError::from)
    }

    /// Supply delivery metadata only for a migrated item whose legacy row lacks it.
    pub fn fill_missing_delivery_settings(
        &mut self,
        remote: &str,
        target: &str,
    ) -> Result<(), StoreError> {
        let updated = self.conn.execute(
            "UPDATE worktrees
                 SET delivery_remote = ?1, target_branch = ?2, updated_at = ?3
                 WHERE work_item_id = ?4
                   AND delivery_remote IS NULL AND target_branch IS NULL",
            params![remote, target, now_millis(), self.work_item_id.as_str()],
        )?;
        if updated == 0 {
            return Err(StoreError::DeliverySettingsAlreadyPersisted);
        }
        Ok(())
    }

    /// Reserve the final head before contacting Git or GitHub.
    pub fn reserve_delivery(
        &mut self,
        final_head_commit: &str,
    ) -> Result<DeliveryRecord, StoreError> {
        let ts = now_millis();
        self.conn.execute(
            "INSERT INTO deliveries
                 (work_item_id, status, final_head_commit, created_at, updated_at)
                 VALUES (?1, 'pending', ?2, ?3, ?3)
                 ON CONFLICT(work_item_id) DO NOTHING",
            params![self.work_item_id.as_str(), final_head_commit, ts],
        )?;
        self.delivery()?
            .ok_or_else(|| StoreError::WorkItemNotFound(self.work_item_id.clone()))
    }

    pub fn delivery(&self) -> Result<Option<DeliveryRecord>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT status, final_head_commit, pr_number, pr_url, created_at, updated_at
                 FROM deliveries WHERE work_item_id = ?1",
                params![self.work_item_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        row.map(
            |(status, final_head_commit, pr_number, pr_url, created_at, updated_at)| {
                Ok(DeliveryRecord {
                    status: DeliveryStatus::from_str(&status)
                        .ok_or(StoreError::InvalidDeliveryStatus(status))?,
                    final_head_commit,
                    pr_number: pr_number.map(|value| value as u64),
                    pr_url,
                    created_at,
                    updated_at,
                })
            },
        )
        .transpose()
    }

    pub fn mark_delivery_pushed(&mut self) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE deliveries SET status = 'pushed', updated_at = ?1 WHERE work_item_id = ?2",
            params![now_millis(), self.work_item_id.as_str()],
        )?;
        Ok(())
    }

    pub fn mark_delivery_pull_request(&mut self, number: u64, url: &str) -> Result<(), StoreError> {
        if number == 0 || url.trim().is_empty() {
            return Err(StoreError::InvalidDeliveryHandoff);
        }
        self.conn.execute(
            "UPDATE deliveries
                 SET status = 'pull_request_created', pr_number = ?1, pr_url = ?2, updated_at = ?3
                 WHERE work_item_id = ?4",
            params![number as i64, url, now_millis(), self.work_item_id.as_str()],
        )?;
        Ok(())
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
        let work_item_id = self.work_item_id.as_str();
        let from_s = from.map(|s| s.as_str());
        let to_s = to.as_str();
        let tx = self.conn.transaction()?;
        for (kind, data) in extra {
            tx.execute(
                "INSERT INTO events (work_item_id, ts, kind, data) VALUES (?1, ?2, ?3, ?4)",
                params![work_item_id, ts, kind, data],
            )?;
        }
        tx.execute(
            "INSERT INTO states (work_item_id, state, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(work_item_id) DO UPDATE
             SET state = excluded.state, updated_at = excluded.updated_at",
            params![work_item_id, to_s, ts],
        )?;
        tx.execute(
            "INSERT INTO transitions
             (work_item_id, from_state, to_state, reason, ts)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![work_item_id, from_s, to_s, reason, ts],
        )?;
        let data = format!("{}->{}", from_s.unwrap_or("-"), to_s);
        tx.execute(
            "INSERT INTO events (work_item_id, ts, kind, data)
             VALUES (?1, ?2, 'transition', ?3)",
            params![work_item_id, ts, data],
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
        let artifacts = self
            .artifacts()?
            .into_iter()
            .map(|artifact| ArtifactSnapshot {
                iteration: artifact.iteration,
                path: artifact.path,
                media_type: artifact.media_type,
                created_at: artifact.created_at,
            })
            .collect();

        let worktree = self
            .conn
            .query_row(
                "SELECT path, branch, requested_base, base_commit, delivery_remote, target_branch, status
                 FROM worktrees WHERE work_item_id = ?1",
                params![self.work_item_id.as_str()],
                |row| {
                    Ok(WorkspaceSnapshot {
                        path: row.get(0)?,
                        branch: Some(row.get(1)?),
                        requested_base: row.get(2)?,
                        base_commit: Some(row.get(3)?),
                        delivery_remote: row.get(4)?,
                        target_branch: row.get(5)?,
                        ready: row.get::<_, String>(6)? == "ready",
                        head: None,
                        clean: None,
                    })
                },
            )
            .optional()?
            .unwrap_or(WorkspaceSnapshot {
                path: String::new(),
                branch: None,
                requested_base: None,
                base_commit: None,
                delivery_remote: None,
                target_branch: None,
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

        let execution = plan
            .as_ref()
            .and_then(|value| crate::ExecutionCapabilities::parse_plan(&value.0).ok());
        let delivery = self.delivery()?.map_or(
            DeliverySnapshot {
                status: None,
                final_head_commit: None,
                pr_number: None,
                pr_url: None,
            },
            |record| DeliverySnapshot {
                status: Some(record.status.as_str().to_string()),
                final_head_commit: Some(record.final_head_commit),
                pr_number: record.pr_number,
                pr_url: record.pr_url,
            },
        );

        let reference = repository_unique_reference(&self.conn, &WorkItemId(id.clone()))?;
        let session_id = id.clone();
        let session_name = if state.is_blocked() {
            self.session(state)?
                .or_else(|| Some(format!("quorum/{session_id}/{state}")))
        } else {
            None
        };
        Ok(StatusSnapshot {
            version: 7,
            identity: WorkItemIdentitySnapshot {
                id,
                reference: reference.clone(),
                label: slug,
                repository_root,
            },
            state: StateSnapshot {
                current: state,
                kind: state.kind(),
            },
            questions: self.questions()?,
            session_name,
            transitions: self.history()?,
            planning: PlanningSnapshot {
                iterations: iterations as u32,
                candidate_count: candidate_count as u32,
                planners,
                plan: plan.as_ref().map(|value| value.0.clone()),
                metrics: plan.and_then(|value| value.1),
                execution,
                feedback: self.plan_feedback()?,
            },
            implementations,
            reviews,
            artifacts,
            errors,
            activities,
            workspace: worktree,
            delivery,
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

    pub fn sync_artifacts(&mut self, iteration: u32, root: &Path) -> Result<usize, StoreError> {
        if !root.exists() {
            return Ok(0);
        }
        let files = artifact_files(root)?;
        let created_at = now_millis().to_string();
        for path in &files {
            let path_text = path
                .to_str()
                .ok_or_else(|| StoreError::NonUtf8Path(path.clone()))?;
            self.conn.execute(
                "INSERT INTO artifacts
                 (work_item_id, iteration, path, media_type, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(work_item_id, path) DO UPDATE SET
                   iteration = excluded.iteration,
                   media_type = excluded.media_type,
                   created_at = excluded.created_at",
                params![
                    self.work_item_id.as_str(),
                    iteration,
                    path_text,
                    artifact_media_type(path),
                    created_at
                ],
            )?;
        }
        Ok(files.len())
    }

    pub fn artifacts(&self) -> Result<Vec<Artifact>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT iteration, path, media_type, created_at
             FROM artifacts WHERE work_item_id = ?1 ORDER BY id",
        )?;
        let artifacts = statement
            .query_map(params![self.work_item_id.as_str()], |row| {
                Ok(Artifact {
                    iteration: row.get::<_, i64>(0)? as u32,
                    path: row.get(1)?,
                    media_type: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)?;
        Ok(artifacts)
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

    pub fn latest_committed_implementation(&self) -> Result<Option<String>, StoreError> {
        self.conn
            .query_row(
                "SELECT result_commit FROM implementation_rounds
                     WHERE work_item_id = ?1 AND status = 'committed'
                     ORDER BY iteration DESC LIMIT 1",
                params![self.work_item_id.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(StoreError::from)
            .map(|value| value.flatten())
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
             WHERE work_item_id = ?1 AND kind = 'human_answer' ORDER BY id ASC",
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

    pub fn plan_feedback(&self) -> Result<Option<String>, StoreError> {
        self.latest_human_feedback("plan_feedback")
    }

    pub fn work_feedback(&self) -> Result<Option<String>, StoreError> {
        self.latest_human_feedback("work_feedback")
    }

    fn latest_human_feedback(&self, kind: &str) -> Result<Option<String>, StoreError> {
        let feedback = self
            .conn
            .query_row(
                "SELECT data FROM events
                 WHERE work_item_id = ?1 AND kind = ?2 AND data IS NOT NULL
                 ORDER BY id DESC LIMIT 1",
                params![self.work_item_id.as_str(), kind],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::from)?;
        Ok(feedback.filter(|text: &String| !text.trim().is_empty()))
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

fn repository_unique_reference(
    conn: &Connection,
    work_item_id: &WorkItemId,
) -> Result<String, StoreError> {
    let mut statement = conn.prepare(
        "SELECT candidate.id
         FROM work_items target
         JOIN work_items candidate
           ON candidate.repository_id IS target.repository_id
         WHERE target.id = ?1 AND candidate.id != target.id",
    )?;
    let other_ids = statement
        .query_map(params![work_item_id.as_str()], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let required_length = other_ids
        .iter()
        .map(|other| {
            work_item_id
                .as_str()
                .bytes()
                .zip(other.bytes())
                .take_while(|(left, right)| left == right)
                .count()
                + 1
        })
        .max()
        .unwrap_or(0)
        .max(DISPLAY_REFERENCE_LENGTH)
        .min(work_item_id.as_str().len());
    Ok(work_item_id.as_str()[..required_length].to_string())
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
        ActivityKind::Delivery => "delivery",
        ActivityKind::Artifact => "artifact",
        ActivityKind::Transition => "transition",
        ActivityKind::HumanIntervention => "human_intervention",
        ActivityKind::Completed => "completed",
        ActivityKind::Failed => "failed",
    }
}

fn artifact_files(root: &Path) -> Result<Vec<std::path::PathBuf>, StoreError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|source| StoreError::ArtifactIo {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| StoreError::ArtifactIo {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| StoreError::ArtifactIo {
                path: path.clone(),
                source,
            })?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn artifact_media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "json" => "application/json",
        "zip" => "application/zip",
        "html" => "text/html",
        "txt" | "log" | "md" => "text/plain",
        _ => "application/octet-stream",
    }
}

fn normalize_legacy_activity(event: &mut ActivityEvent) {
    event.role = event.role.take().map(|role| expand_role_name(&role));
    if matches!(
        event.kind,
        ActivityKind::AgentStarted
            | ActivityKind::AgentRetrying
            | ActivityKind::AgentCompleted
            | ActivityKind::AgentFailed
    ) {
        event.message = expand_role_name(&event.message);
    }
    event.message = event.message.replace("hi: ", "human: ");
}

fn expand_role_name(value: &str) -> String {
    for (legacy, expanded) in [
        ("PL-intake:", "Intake Planner:"),
        ("PL:", "Planner:"),
        ("CO:merge", "Coordinator:merge"),
        ("IM", "Implementer"),
        ("RV", "Reviewer"),
    ] {
        if let Some(suffix) = value.strip_prefix(legacy) {
            return format!("{expanded}{suffix}");
        }
    }
    value.to_string()
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
             VALUES ('legacy-work-item', 'legacy', '# Legacy', '1', '1');
             INSERT INTO states (work_item_id, state, updated_at)
             VALUES ('legacy-work-item', 'Planning', '1');",
        )
        .unwrap();
        drop(conn);

        let db = Database::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let store = db
            .into_store(WorkItemId("legacy-work-item".to_string()))
            .unwrap();
        assert_eq!(store.work_item().unwrap().as_deref(), Some("# Legacy"));
        assert_eq!(store.current_state().unwrap(), Some(State::Planning));
    }

    #[test]
    fn migrates_v7_to_non_unique_work_item_labels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quorum.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta (key, value) VALUES ('schema_version', '7');
             CREATE TABLE repositories (
                 id TEXT PRIMARY KEY,
                 root TEXT NOT NULL UNIQUE,
                 registered INTEGER NOT NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             INSERT INTO repositories
                 (id, root, registered, created_at, updated_at)
             VALUES ('repo', '/repo', 1, '1', '1');
             CREATE TABLE work_items (
                 id TEXT PRIMARY KEY,
                 repository_id TEXT REFERENCES repositories(id),
                 slug TEXT NOT NULL,
                 text TEXT,
                 source TEXT,
                 origin_repo TEXT,
                 origin_issue INTEGER,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 UNIQUE (repository_id, slug)
             );
             INSERT INTO work_items
                 (id, repository_id, slug, text, created_at, updated_at)
             VALUES ('aaaaaaaa-1111-4111-8111-111111111111', 'repo', 'same', '# One', '1', '1');
             CREATE TABLE states (
                 work_item_id TEXT PRIMARY KEY REFERENCES work_items(id) ON DELETE CASCADE,
                 state TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             INSERT INTO states (work_item_id, state, updated_at)
             VALUES ('aaaaaaaa-1111-4111-8111-111111111111', 'PlanReview', '1');
             CREATE TABLE sessions (
                 work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
                 state TEXT NOT NULL,
                 session_name TEXT NOT NULL,
                 ts TEXT NOT NULL,
                 PRIMARY KEY (work_item_id, state)
             );
             INSERT INTO sessions (work_item_id, state, session_name, ts)
             VALUES (
                 'aaaaaaaa-1111-4111-8111-111111111111',
                 'PlanReview',
                 'quorum/same/PlanReview',
                 '1'
             );",
        )
        .unwrap();
        drop(conn);

        let mut db = Database::open(&path).unwrap();
        let repository = db
            .registered_repository(&RepositoryRoot::from_canonical("/repo"))
            .unwrap()
            .unwrap();
        let second = db
            .create_work_item(&repository.id, "same", "# Two")
            .unwrap();
        assert_ne!(second.as_str(), "aaaaaaaa-1111-4111-8111-111111111111");
        assert_eq!(db.work_items(&repository.id).unwrap().len(), 2);
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let foreign_key_errors: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(foreign_key_errors, 0);
        let store = db
            .into_store(WorkItemId(
                "aaaaaaaa-1111-4111-8111-111111111111".to_string(),
            ))
            .unwrap();
        assert_eq!(store.current_state().unwrap(), Some(State::PlanReview));
        assert_eq!(
            store.session(State::PlanReview).unwrap().as_deref(),
            Some("quorum/same/PlanReview")
        );
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
    fn migrates_v5_activity_roles_to_full_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quorum.db");
        let mut db = Database::open(&path).unwrap();
        let repository = register(&mut db, "/repo");
        let work_item = db
            .get_or_create_work_item(&repository.id, "terminology")
            .unwrap();
        for (role, message) in [
            ("PL-intake:planner-a", "PL-intake:planner-a started"),
            ("PL:planner-a", "PL:planner-a completed"),
            ("CO:merge", "CO:merge started"),
            ("IM", "IM failed; retrying"),
            ("RV", "RV completed"),
        ] {
            let data = serde_json::json!({
                "timestamp_ms": 1,
                "kind": "agent_started",
                "message": message,
                "role": role
            });
            db.conn
                .execute(
                    "INSERT INTO activities (work_item_id, ts, kind, data)
                     VALUES (?1, 1, 'agent_started', ?2)",
                    params![work_item.as_str(), data.to_string()],
                )
                .unwrap();
        }
        let free_text = ActivityEvent::new(ActivityKind::Failed, "RVM error remains literal");
        db.conn
            .execute(
                "INSERT INTO activities (work_item_id, ts, kind, data)
                 VALUES (?1, 1, 'failed', ?2)",
                params![
                    work_item.as_str(),
                    serde_json::to_string(&free_text).unwrap()
                ],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO events (work_item_id, ts, kind, data)
                 VALUES (?1, '1', 'hi_answer', 'legacy answer'),
                        (?1, '1', 'hi_decision', 'legacy decision')",
                params![work_item.as_str()],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO transitions
                 (work_item_id, from_state, to_state, reason, ts)
                 VALUES (?1, 'PlanReview', 'Implementing',
                         'hi: approve PlanReview -> Implementing', '1')",
                params![work_item.as_str()],
            )
            .unwrap();
        db.conn
            .execute(
                "UPDATE meta SET value = '5' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        drop(db);

        let db = Database::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let store = db.into_store(work_item).unwrap();
        let activities = store.activities().unwrap();
        assert_eq!(
            activities
                .iter()
                .filter_map(|event| event.role.as_deref())
                .collect::<Vec<_>>(),
            vec![
                "Intake Planner:planner-a",
                "Planner:planner-a",
                "Coordinator:merge",
                "Implementer",
                "Reviewer"
            ]
        );
        assert_eq!(activities[3].message, "Implementer failed; retrying");
        assert_eq!(activities[5].message, "RVM error remains literal");
        assert_eq!(store.count_events_of_kind("hi_answer").unwrap(), 0);
        assert_eq!(store.count_events_of_kind("hi_decision").unwrap(), 0);
        assert_eq!(store.count_events_of_kind("human_answer").unwrap(), 1);
        assert_eq!(store.count_events_of_kind("human_decision").unwrap(), 1);
        assert_eq!(
            store.history().unwrap()[0].reason,
            "human: approve PlanReview -> Implementing"
        );
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
    fn persists_delivery_intent_and_idempotent_checkpoints() {
        let mut db = Database::open_in_memory().unwrap();
        let repository = register(&mut db, "/repo");
        let work_item = db
            .get_or_create_work_item(&repository.id, "delivery")
            .unwrap();
        db.reserve_worktree_with_delivery(
            &work_item,
            &WorktreeStart {
                requested_base: "feature".to_string(),
                base_commit: "base".to_string(),
                delivery_remote: "origin".to_string(),
                target_branch: "main".to_string(),
            },
            "quorum/delivery",
            Path::new("/state/delivery/implementation"),
        )
        .unwrap();
        let mut store = db.into_store(work_item).unwrap();
        let reserved = store.reserve_delivery("final").unwrap();
        assert_eq!(reserved.status, DeliveryStatus::Pending);
        assert_eq!(store.reserve_delivery("final").unwrap(), reserved);
        store.mark_delivery_pushed().unwrap();
        store
            .mark_delivery_pull_request(42, "https://github.test/owner/repo/pull/42")
            .unwrap();
        let delivery = store.delivery().unwrap().unwrap();
        assert_eq!(delivery.status, DeliveryStatus::PullRequestCreated);
        assert_eq!(delivery.pr_number, Some(42));
        assert_eq!(
            delivery.pr_url.as_deref(),
            Some("https://github.test/owner/repo/pull/42")
        );
        let worktree = store.worktree().unwrap().unwrap();
        assert_eq!(worktree.requested_base.as_deref(), Some("feature"));
        assert_eq!(worktree.delivery_remote.as_deref(), Some("origin"));
        assert_eq!(worktree.target_branch.as_deref(), Some("main"));
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
    fn creates_and_lists_repository_work_items() {
        let mut db = Database::open_in_memory().unwrap();
        let repository = register(&mut db, "/repo");
        let first = db
            .create_work_item(&repository.id, "first", "# First")
            .unwrap();
        let duplicate = db
            .create_work_item(&repository.id, "first", "# Duplicate")
            .unwrap();
        assert_ne!(duplicate, first);
        {
            let mut store = db.into_store(first.clone()).unwrap();
            store
                .record_transition(None, State::PlanReview, "ready")
                .unwrap();
            db = Database { conn: store.conn };
        }

        let summaries = db.work_items(&repository.id).unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].id, first);
        assert_eq!(summaries[0].label, "first");
        assert_eq!(summaries[0].state, State::PlanReview);
        assert_eq!(summaries[1].id, duplicate);
        assert_eq!(summaries[1].label, "first");
    }

    #[test]
    fn resolves_repository_scoped_uuid_prefixes() {
        let mut db = Database::open_in_memory().unwrap();
        let repository = register(&mut db, "/repo");
        let other_repository = register(&mut db, "/other");
        let ts = now_millis();
        for (id, repository_id, label) in [
            (
                "aaaaaaaa-1111-4111-8111-111111111111",
                &repository.id,
                "first",
            ),
            (
                "aaaaaaaa-2222-4222-8222-222222222222",
                &repository.id,
                "second",
            ),
            (
                "aaaaaaaa-3333-4333-8333-333333333333",
                &other_repository.id,
                "other",
            ),
        ] {
            db.conn
                .execute(
                    "INSERT INTO work_items
                     (id, repository_id, slug, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?4)",
                    params![id, repository_id.as_str(), label, ts],
                )
                .unwrap();
        }

        assert!(matches!(
            db.resolve_work_item(&repository.id, "a"),
            Err(StoreError::AmbiguousWorkItemReference { .. })
        ));
        let resolved = db
            .resolve_work_item(&repository.id, "AAAAAAAA-1")
            .unwrap()
            .unwrap();
        assert_eq!(resolved.id.as_str(), "aaaaaaaa-1111-4111-8111-111111111111");
        assert_eq!(resolved.reference, "aaaaaaaa-1");
        assert_eq!(resolved.label, "first");
        let full = db
            .resolve_work_item(&repository.id, "aaaaaaaa-1111-4111-8111-111111111111")
            .unwrap()
            .unwrap();
        assert_eq!(full, resolved);
        let other = db
            .resolve_work_item(&other_repository.id, "a")
            .unwrap()
            .unwrap();
        assert_eq!(other.label, "other");
        assert!(db
            .resolve_work_item(&repository.id, "bbbbbbbb")
            .unwrap()
            .is_none());
        assert!(matches!(
            db.resolve_work_item(&repository.id, "not-a-uuid"),
            Err(StoreError::InvalidWorkItemReference { .. })
        ));
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
        store.set_work_item("# Work Item\nbody").unwrap();
        assert_eq!(
            store.work_item().unwrap().as_deref(),
            Some("# Work Item\nbody")
        );

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
    fn persists_execution_artifacts_without_following_symlinks() {
        let mut store = Store::open_in_memory().unwrap();
        let root = tempfile::tempdir().unwrap();
        let screenshot = root.path().join("page.png");
        std::fs::write(&screenshot, b"png").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", root.path().join("outside")).unwrap();

        assert_eq!(store.sync_artifacts(2, root.path()).unwrap(), 1);
        assert_eq!(
            store.artifacts().unwrap(),
            vec![Artifact {
                iteration: 2,
                path: screenshot.display().to_string(),
                media_type: "image/png".to_string(),
                created_at: store.artifacts().unwrap()[0].created_at.clone(),
            }]
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
            .record_transition_with_events(
                Some(State::Intake),
                State::Planning,
                "start",
                &[("plan_feedback", "add rollback")],
            )
            .unwrap();
        store
            .record_activity(
                &ActivityEvent::new(ActivityKind::AgentStarted, "planner-a started")
                    .phase(State::Planning)
                    .role("Planner:planner-a")
                    .iteration(0),
            )
            .unwrap();

        let reference = store.work_item_reference().unwrap();
        let snapshot = store.status_snapshot().unwrap();
        assert_eq!(snapshot.version, 7);
        assert_eq!(snapshot.identity.id, work_item.as_str());
        assert_eq!(snapshot.identity.reference, reference);
        assert_eq!(snapshot.identity.label, "observable");
        assert_eq!(snapshot.identity.repository_root, "/repo");
        assert_eq!(snapshot.state.current, State::Planning);
        assert_eq!(snapshot.planning.iterations, 1);
        assert_eq!(snapshot.planning.candidate_count, 1);
        assert_eq!(snapshot.planning.plan.as_deref(), Some("the plan"));
        assert_eq!(snapshot.planning.feedback.as_deref(), Some("add rollback"));
        assert_eq!(snapshot.activities.len(), 1);
        assert_eq!(
            snapshot.workspace.branch.as_deref(),
            Some("quorum/observable")
        );
    }
}
