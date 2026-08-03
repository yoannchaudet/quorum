use std::fs;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use fs2::FileExt;
use rusqlite::{Connection, Transaction};

use crate::error::{AppError, StoreError};

const APPLICATION_ID: i32 = 0x5155_4F52;
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_initial.sql"),
    include_str!("../migrations/0002_model_settings.sql"),
    include_str!("../migrations/0003_planning_intake.sql"),
    include_str!("../migrations/0004_planning_state_machine.sql"),
    include_str!("../migrations/0005_terminal_handoffs.sql"),
    include_str!("../migrations/0006_planning_ipc.sql"),
    include_str!("../migrations/0007_work_item_approval_policy.sql"),
    include_str!("../migrations/0008_execution.sql"),
];

#[derive(Debug, Clone)]
pub struct AppStore {
    path: PathBuf,
    _lease: Arc<File>,
}

impl AppStore {
    pub fn open(app_data_dir: impl AsRef<Path>) -> Result<Self, AppError> {
        let app_data_dir = app_data_dir.as_ref();
        fs::create_dir_all(app_data_dir).map_err(StoreError::from)?;
        let lease_path = app_data_dir.join("quorum.lock");
        match fs::symlink_metadata(&lease_path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(AppError::path(format!(
                    "{} is not a regular lock file.",
                    lease_path.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StoreError::from(error).into()),
        }
        let mut lease_options = OpenOptions::new();
        lease_options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            lease_options.custom_flags(libc::O_NOFOLLOW);
        }
        let lease = lease_options.open(&lease_path).map_err(StoreError::from)?;
        FileExt::try_lock_exclusive(&lease).map_err(|error| {
            AppError::conflict(format!(
                "Another live Quorum instance owns {}. Startup recovery was not run: {error}",
                lease_path.display()
            ))
        })?;
        let store = Self {
            path: app_data_dir.join("quorum.sqlite3"),
            _lease: Arc::new(lease),
        };
        let connection = Connection::open(&store.path).map_err(StoreError::from)?;
        configure(&connection).map_err(StoreError::from)?;
        migrate(&connection)?;
        recover_interrupted_work(&connection)?;
        Ok(store)
    }

    pub fn database_path(&self) -> &Path {
        &self.path
    }

    pub fn app_data_dir(&self) -> &Path {
        self.path
            .parent()
            .expect("Quorum's database always has an application-data parent")
    }

    pub fn run_lease_path(&self, run_id: &str) -> Result<PathBuf, AppError> {
        let directory = self.app_data_dir().join("run-leases");
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(AppError::path(format!(
                    "{} is not a real directory.",
                    directory.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&directory).map_err(StoreError::from)?;
            }
            Err(error) => return Err(StoreError::from(error).into()),
        }
        let root = fs::canonicalize(self.app_data_dir()).map_err(StoreError::from)?;
        let resolved = fs::canonicalize(&directory).map_err(StoreError::from)?;
        if !resolved.starts_with(root) {
            return Err(AppError::path(
                "Quorum's execution lease directory escapes application data.",
            ));
        }
        let path = directory.join(format!("{run_id}.lock"));
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(AppError::path(format!(
                    "{} is not a regular execution lock file.",
                    path.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StoreError::from(error).into()),
        }
        Ok(path)
    }

    pub fn with_connection<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, AppError> {
        let connection = Connection::open(&self.path).map_err(StoreError::from)?;
        configure(&connection).map_err(StoreError::from)?;
        migrate(&connection)?;
        operation(&connection).map_err(AppError::from)
    }
}

fn configure(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;",
    )
}

#[allow(clippy::too_many_lines)]
fn recover_interrupted_work(connection: &Connection) -> Result<(), AppError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(StoreError::from)?;
    let timestamp = Utc::now().to_rfc3339();
    transaction
        .execute(
            "UPDATE planning_runs
             SET status = 'blocked', error_code = 'interrupted',
                 error_message = 'Running planning work became unobservable after restart.',
                 updated_at = ?1, completed_at = ?1
             WHERE EXISTS (
               SELECT 1 FROM planning_agents
               WHERE planning_agents.planning_run_id = planning_runs.id
                 AND planning_agents.status = 'running'
             )",
            [&timestamp],
        )
        .map_err(StoreError::from)?;
    transaction
        .execute(
            "UPDATE execution_commands
             SET status = 'interrupted', completed_at = ?1
             WHERE status = 'running'",
            [&timestamp],
        )
        .map_err(StoreError::from)?;
    transaction
        .execute(
            "UPDATE execution_attempts
             SET status = 'interrupted', error_code = 'interrupted',
                 error_message = 'Quorum restarted while this owned execution attempt was running.',
                 completed_at = ?1
             WHERE status = 'running'",
            [&timestamp],
        )
        .map_err(StoreError::from)?;
    transaction
        .execute(
            "UPDATE runs
             SET outcome = 'blocked', updated_at = ?1
             WHERE id IN (
               SELECT run_id FROM execution_runs
               WHERE status IN (
                 'starting', 'building', 'verifying', 'reviewing', 'remediating', 'cancelling'
               )
             )",
            [&timestamp],
        )
        .map_err(StoreError::from)?;
    transaction
        .execute(
            "UPDATE execution_runs
             SET status = 'blocked', error_code = 'interrupted',
                 error_message = 'Quorum restarted while execution was running. Resume to create a new owned attempt; persisted process identifiers were not reused.',
                 updated_at = ?1, completed_at = ?1
             WHERE status IN (
               'starting', 'building', 'verifying', 'reviewing', 'remediating', 'cancelling'
             )",
            [&timestamp],
        )
        .map_err(StoreError::from)?;
    transaction
        .execute(
            "UPDATE execution_runs
             SET builder_session_state = 'not_started'
             WHERE builder_session_state = 'launching'",
            [],
        )
        .map_err(StoreError::from)?;
    transaction
        .execute(
            "UPDATE execution_runs
             SET reviewer_session_state = 'not_started'
             WHERE reviewer_session_state = 'launching'",
            [],
        )
        .map_err(StoreError::from)?;
    transaction
        .execute(
            "UPDATE planning_agents
             SET status = 'blocked', error_code = 'interrupted',
                 error_message = 'The prior process ended while this agent was running.',
                 updated_at = ?1, completed_at = ?1
             WHERE status = 'running'",
            [&timestamp],
        )
        .map_err(StoreError::from)?;
    transaction
        .execute(
            "UPDATE terminal_handoffs
             SET status = 'awaiting_manual_reconcile', completion_observable = 0,
                 error_code = 'interrupted',
                 error_message = 'Quorum restarted while terminal launch was in progress. Check the terminal for the persisted session, then reconcile manually.',
                 updated_at = ?1, completed_at = NULL
             WHERE status = 'launching'",
            [&timestamp],
        )
        .map_err(StoreError::from)?;
    transaction
        .execute(
            "UPDATE terminal_handoffs
             SET status = 'reconcile_failed', error_code = 'interrupted',
                 error_message = 'Quorum restarted while terminal reconciliation was in progress. Reconcile the persisted session again.',
                 updated_at = ?1, completed_at = NULL
             WHERE status = 'reconciling'",
            [&timestamp],
        )
        .map_err(StoreError::from)?;
    transaction.commit().map_err(StoreError::from)?;
    Ok(())
}

fn migrate(connection: &Connection) -> Result<(), AppError> {
    let application_id: i32 = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(|error| {
            AppError::migration(format!("Could not read database identity: {error}"))
        })?;
    if application_id != 0 && application_id != APPLICATION_ID {
        return Err(AppError::migration(
            "This database does not belong to Quorum; it was not modified.",
        ));
    }

    let version: usize = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| {
            AppError::migration(format!("Could not read database version: {error}"))
        })?;
    if version > MIGRATIONS.len() {
        return Err(AppError::migration(format!(
            "This Quorum database uses a newer schema version ({version})."
        )));
    }
    if version == MIGRATIONS.len() {
        return Ok(());
    }

    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .map_err(|error| {
            AppError::migration(format!(
                "Could not suspend foreign key checks for migration: {error}"
            ))
        })?;
    let migration_result = apply_pending_migrations(connection, version);
    let restore_result = connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|error| {
            AppError::migration(format!(
                "Could not restore foreign key checks after migration: {error}"
            ))
        });
    migration_result?;
    restore_result
}

fn apply_pending_migrations(connection: &Connection, version: usize) -> Result<(), AppError> {
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| AppError::migration(format!("Could not start migration: {error}")))?;
        apply_migration(&transaction, sql, index + 1)?;
        let has_foreign_key_violation: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
                [],
                |row| row.get(0),
            )
            .map_err(|error| {
                AppError::migration(format!(
                    "Could not validate migration {}: {error}",
                    index + 1
                ))
            })?;
        if has_foreign_key_violation {
            return Err(AppError::migration(format!(
                "Migration {} would leave invalid relationships.",
                index + 1
            )));
        }
        transaction
            .commit()
            .map_err(|error| AppError::migration(format!("Could not commit migration: {error}")))?;
    }
    Ok(())
}

fn apply_migration(
    transaction: &Transaction<'_>,
    sql: &str,
    version: usize,
) -> Result<(), AppError> {
    transaction
        .execute_batch(sql)
        .map_err(|error| AppError::migration(format!("Migration {version} failed: {error}")))?;
    transaction
        .execute_batch(&format!(
            "PRAGMA application_id = {APPLICATION_ID}; PRAGMA user_version = {version};"
        ))
        .map_err(|error| {
            AppError::migration(format!("Could not record migration {version}: {error}"))
        })
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::{AppStore, APPLICATION_ID};
    use crate::settings::{DEFAULT_TERMINAL_APPLICATION, DEFAULT_TERMINAL_ARGUMENTS};

    #[test]
    fn initializes_and_reopens_without_resetting_data() {
        let directory = tempdir().expect("temp dir");
        let store = AppStore::open(directory.path()).expect("open store");
        store
            .with_connection(|connection| {
                connection.execute(
                    "INSERT INTO repositories (id, root_path, display_name, created_at, updated_at)
                     VALUES ('id', '/repo', 'repo', 'now', 'now')",
                    [],
                )?;
                Ok(())
            })
            .expect("write");
        drop(store);
        let reopened = AppStore::open(directory.path()).expect("reopen store");
        let count: i64 = reopened
            .with_connection(|connection| {
                connection
                    .query_row("SELECT count(*) FROM repositories", [], |row| row.get(0))
                    .map_err(Into::into)
            })
            .expect("read");
        assert_eq!(count, 1);
        assert_eq!(
            reopened.database_path(),
            directory.path().join("quorum.sqlite3")
        );
    }

    #[test]
    fn rejects_a_second_live_store_owner_and_reopens_after_release() {
        let directory = tempdir().expect("temp dir");
        let store = AppStore::open(directory.path()).expect("first owner");
        let error = AppStore::open(directory.path()).expect_err("second owner must fail closed");
        assert_eq!(error.code, "conflict");
        assert!(error.message.contains("Another live Quorum instance"));
        drop(store);
        AppStore::open(directory.path()).expect("lease released after owner drop");
    }

    #[test]
    fn rejects_another_app_database_without_rebuilding_it() {
        let directory = tempdir().expect("temp dir");
        let database = directory.path().join("quorum.sqlite3");
        let connection = Connection::open(&database).expect("create database");
        connection
            .execute_batch("PRAGMA application_id = 42; CREATE TABLE sentinel (value TEXT);")
            .expect("seed");
        drop(connection);
        assert_eq!(
            AppStore::open(directory.path())
                .expect_err("must reject")
                .code,
            "migration"
        );
        let connection = Connection::open(database).expect("reopen");
        let exists: i32 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'sentinel'",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(exists, 1);
    }

    #[test]
    fn upgrades_a_version_one_database_with_model_defaults() {
        let directory = tempdir().expect("temp dir");
        let database = directory.path().join("quorum.sqlite3");
        let connection = Connection::open(&database).expect("create database");
        connection
            .execute_batch(include_str!("../migrations/0001_initial.sql"))
            .expect("version one schema");
        connection
            .execute_batch(&format!(
                "PRAGMA application_id = {APPLICATION_ID}; PRAGMA user_version = 1;"
            ))
            .expect("version one identity");
        drop(connection);

        let store = AppStore::open(directory.path()).expect("upgrade");
        store
            .with_connection(|connection| {
                let assignments: i32 =
                    connection.query_row("SELECT count(*) FROM model_assignments", [], |row| {
                        row.get(0)
                    })?;
                let version: i32 =
                    connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
                let terminal_application: String = connection.query_row(
                    "SELECT value FROM app_settings WHERE key = 'terminal_application'",
                    [],
                    |row| row.get(0),
                )?;
                let terminal_arguments: String = connection.query_row(
                    "SELECT value FROM app_settings WHERE key = 'terminal_arguments'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(assignments, 4);
                assert_eq!(version, 8);
                assert_eq!(terminal_application, DEFAULT_TERMINAL_APPLICATION);
                assert_eq!(terminal_arguments, DEFAULT_TERMINAL_ARGUMENTS);
                Ok(())
            })
            .expect("upgraded defaults");
    }

    #[test]
    fn upgrades_version_two_without_losing_related_data() {
        let directory = tempdir().expect("temp dir");
        let database = directory.path().join("quorum.sqlite3");
        let connection = Connection::open(&database).expect("create database");
        connection
            .execute_batch(include_str!("../migrations/0001_initial.sql"))
            .expect("version one schema");
        connection
            .execute_batch(include_str!("../migrations/0002_model_settings.sql"))
            .expect("version two schema");
        connection
            .execute_batch(
                "INSERT INTO repositories (
                   id, root_path, display_name, created_at, updated_at
                 ) VALUES ('repository', '/repo', 'repo', 'created', 'updated');
                 INSERT INTO work_items (
                   id, repository_id, title, source_kind, markdown_body, lifecycle_status,
                   created_at, updated_at
                 ) VALUES (
                   'work', 'repository', 'Existing work', 'inline_markdown', '# Snapshot',
                   'open', 'created', 'updated'
                 );
                 INSERT INTO plans (
                   id, work_item_id, revision, markdown_body, approval_policy, approval_status,
                   created_at, updated_at
                 ) VALUES (
                   'plan', 'work', 1, '# Plan', 'required', 'approved', 'created', 'updated'
                 );
                 INSERT INTO runs (
                   id, work_item_id, plan_id, phase, outcome, created_at, updated_at
                 ) VALUES (
                   'run', 'work', 'plan', 'planning', 'succeeded', 'created', 'updated'
                 );
                 INSERT INTO phase_events (
                   id, run_id, sequence, event_kind, payload_json, created_at
                 ) VALUES ('event', 'run', 0, 'saved', '{}', 'created');
                 INSERT INTO queue_entries (
                   id, work_item_id, run_id, position, scheduling_status, created_at, updated_at
                 ) VALUES ('queue', 'work', 'run', 0, 'queued', 'created', 'updated');",
            )
            .expect("seed version two data");
        connection
            .execute_batch(&format!(
                "PRAGMA application_id = {APPLICATION_ID}; PRAGMA user_version = 2;"
            ))
            .expect("version two identity");
        drop(connection);

        let store = AppStore::open(directory.path()).expect("upgrade");
        store
            .with_connection(|connection| {
                let work_item: (String, String, String) = connection.query_row(
                    "SELECT source_kind, source_metadata_json, markdown_body
                     FROM work_items WHERE id = 'work'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                assert_eq!(
                    work_item,
                    (
                        "inline_markdown".to_owned(),
                        r#"{"kind":"inline_markdown"}"#.to_owned(),
                        "# Snapshot".to_owned()
                    )
                );
                for table in ["plans", "runs", "phase_events", "queue_entries"] {
                    let count: i64 = connection.query_row(
                        &format!("SELECT count(*) FROM {table}"),
                        [],
                        |row| row.get(0),
                    )?;
                    assert_eq!(count, 1, "{table} data was not preserved");
                }
                let foreign_key_errors: i64 = connection.query_row(
                    "SELECT count(*) FROM pragma_foreign_key_check",
                    [],
                    |row| row.get(0),
                )?;
                let version: i32 =
                    connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
                assert_eq!(foreign_key_errors, 0);
                assert_eq!(version, 8);
                Ok(())
            })
            .expect("preserved data");
    }

    #[test]
    fn schema_enforces_repository_uniqueness_and_work_item_foreign_keys() {
        let directory = tempdir().expect("temp dir");
        let store = AppStore::open(directory.path()).expect("open store");
        store
            .with_connection(|connection| {
                connection.execute(
                    "INSERT INTO repositories (id, root_path, display_name, created_at, updated_at)
                     VALUES ('one', '/repo', 'repo', 'now', 'now')",
                    [],
                )?;
                assert!(connection
                    .execute(
                        "INSERT INTO repositories (id, root_path, display_name, created_at, updated_at)
                         VALUES ('two', '/repo', 'other', 'now', 'now')",
                        [],
                    )
                    .is_err());
                assert!(connection
                    .execute(
                        "INSERT INTO work_items (
                           id, repository_id, title, source_kind, source_metadata_json, markdown_body,
                           lifecycle_status, created_at, updated_at
                         ) VALUES (
                           'work', 'missing', 'title', 'inline_markdown',
                           '{\"kind\":\"inline_markdown\"}', '', 'open', 'now', 'now'
                         )",
                        [],
                    )
                    .is_err());
                let version: i32 =
                    connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
                assert_eq!(version, 8);
                Ok(())
            })
            .expect("schema constraints");
    }

    #[test]
    fn planning_schema_enforces_identity_and_queue_idempotency() {
        let directory = tempdir().expect("temp dir");
        let store = AppStore::open(directory.path()).expect("open store");
        store
            .with_connection(|connection| {
                connection.execute_batch(
                    "INSERT INTO repositories (
                       id, root_path, display_name, created_at, updated_at
                     ) VALUES ('repository', '/repo', 'repo', 'now', 'now');
                     INSERT INTO work_items (
                       id, repository_id, title, source_kind, source_metadata_json, markdown_body,
                       lifecycle_status, created_at, updated_at
                     ) VALUES (
                       'work', 'repository', 'title', 'local_markdown',
                       '{\"kind\":\"local_markdown\",\"path\":\"/work.md\"}', '# Work',
                       'open', 'now', 'now'
                     );
                     INSERT INTO planning_runs (
                       id, work_item_id, status, created_at, updated_at
                     ) VALUES ('planning-run', 'work', 'running', 'now', 'now');
                     INSERT INTO planning_agents (
                       id, planning_run_id, role, ordinal, model_id, session_name, status,
                       created_at, updated_at
                     ) VALUES (
                       'agent-one', 'planning-run', 'planner', 0, 'model-one',
                       'quorum-planning-run-planner-0', 'running', 'now', 'now'
                     );
                     INSERT INTO plans (
                       id, work_item_id, revision, markdown_body, approval_policy, approval_status,
                       created_at, updated_at, planning_run_id, queue_eligibility_key,
                       queue_eligible_at
                     ) VALUES (
                       'plan', 'work', 1, '# Plan', 'not_required', 'draft', 'now', 'now',
                       'planning-run', 'eligible-plan', 'now'
                     );
                     INSERT INTO queue_entries (
                       id, work_item_id, position, scheduling_status, created_at, updated_at,
                       plan_id, idempotency_key
                     ) VALUES (
                       'queue-one', 'work', 0, 'queued', 'now', 'now', 'plan', 'eligible-plan'
                     );",
                )?;
                assert!(connection
                    .execute(
                        "INSERT INTO planning_agents (
                           id, planning_run_id, role, ordinal, model_id, session_name, status,
                           created_at, updated_at
                         ) VALUES (
                           'agent-two', 'planning-run', 'planner', 1, 'model-two',
                           'quorum-planning-run-planner-0', 'pending', 'now', 'now'
                         )",
                        [],
                    )
                    .is_err());
                assert!(connection
                    .execute(
                        "INSERT INTO queue_entries (
                           id, work_item_id, position, scheduling_status, created_at, updated_at,
                           plan_id, idempotency_key
                         ) VALUES (
                           'queue-two', 'work', 1, 'queued', 'now', 'now', 'plan', 'eligible-plan'
                         )",
                        [],
                    )
                    .is_err());
                connection.execute(
                    "INSERT INTO plans (
                       id, work_item_id, revision, markdown_body, approval_policy, approval_status,
                       created_at, updated_at, queue_eligibility_key, queue_eligible_at
                     ) VALUES (
                       'unapproved-plan', 'work', 2, '# Plan', 'required', 'pending',
                       'now', 'now', 'unapproved-plan', 'now'
                     )",
                    [],
                )?;
                assert!(connection
                    .execute(
                        "INSERT INTO queue_entries (
                           id, work_item_id, position, scheduling_status, created_at, updated_at,
                           plan_id, idempotency_key
                         ) VALUES (
                           'queue-unapproved', 'work', 2, 'queued', 'now', 'now',
                           'unapproved-plan', 'unapproved-plan'
                         )",
                        [],
                    )
                    .is_err());
                assert!(connection
                    .execute(
                        "UPDATE work_items SET markdown_body = '# Changed' WHERE id = 'work'",
                        [],
                    )
                    .is_err());
                Ok(())
            })
            .expect("planning constraints");
    }

    #[test]
    fn reopening_recovers_running_agents_and_terminal_handoffs() {
        let directory = tempdir().expect("temp dir");
        let store = AppStore::open(directory.path()).expect("open store");
        store
            .with_connection(|connection| {
                connection.execute_batch(
                    "INSERT INTO repositories (
                       id, root_path, display_name, created_at, updated_at
                     ) VALUES ('repository', '/repo', 'repo', 'before', 'before');
                     INSERT INTO work_items (
                       id, repository_id, title, source_kind, source_metadata_json,
                       markdown_body, lifecycle_status, created_at, updated_at
                     ) VALUES (
                       'work', 'repository', 'title', 'inline_markdown',
                       '{\"kind\":\"inline_markdown\"}', '# Work', 'open',
                       'before', 'before'
                     );
                     INSERT INTO planning_runs (
                       id, work_item_id, status, created_at, updated_at
                     ) VALUES ('run', 'work', 'running', 'before', 'before');
                     INSERT INTO planning_agents (
                       id, planning_run_id, role, ordinal, model_id, session_name,
                       status, attempt, started_at, created_at, updated_at
                     ) VALUES (
                       'agent', 'run', 'planner', 0, 'model', 'session',
                       'running', 1, 'before', 'before', 'before'
                     );
                     INSERT INTO terminal_handoffs (
                       id, work_item_id, planning_run_id, planning_agent_id,
                       session_name, idempotency_key, status, created_at, updated_at
                     ) VALUES (
                       'launching', 'work', 'run', 'agent', 'session',
                       'launching-key', 'launching', 'before', 'before'
                     );
                     INSERT INTO terminal_handoffs (
                       id, work_item_id, planning_run_id, planning_agent_id,
                       session_name, idempotency_key, status, created_at, updated_at
                     ) VALUES (
                       'reconciling', 'work', 'run', 'agent', 'session',
                       'reconciling-key', 'reconciling', 'before', 'before'
                     );",
                )?;
                Ok(())
            })
            .expect("seed interrupted work");

        drop(store);
        let reopened = AppStore::open(directory.path()).expect("recover on reopen");
        reopened
            .with_connection(|connection| {
                let run: (String, Option<String>) = connection.query_row(
                    "SELECT status, error_code FROM planning_runs WHERE id = 'run'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let agent: (String, Option<String>) = connection.query_row(
                    "SELECT status, error_code FROM planning_agents WHERE id = 'agent'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let launching: (String, Option<String>) = connection.query_row(
                    "SELECT status, error_code FROM terminal_handoffs
                     WHERE id = 'launching'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let reconciling: (String, Option<String>) = connection.query_row(
                    "SELECT status, error_code FROM terminal_handoffs
                     WHERE id = 'reconciling'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!(run, ("blocked".to_owned(), Some("interrupted".to_owned())));
                assert_eq!(
                    agent,
                    ("blocked".to_owned(), Some("interrupted".to_owned()))
                );
                assert_eq!(
                    launching,
                    (
                        "awaiting_manual_reconcile".to_owned(),
                        Some("interrupted".to_owned())
                    )
                );
                assert_eq!(
                    reconciling,
                    (
                        "reconcile_failed".to_owned(),
                        Some("interrupted".to_owned())
                    )
                );
                Ok(())
            })
            .expect("verify recovered work");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn reopening_interrupts_execution_without_reusing_process_identity() {
        let directory = tempdir().expect("temp dir");
        let store = AppStore::open(directory.path()).expect("open store");
        store
            .with_connection(|connection| {
                connection.execute_batch(
                    "INSERT INTO repositories (
                       id, root_path, display_name, created_at, updated_at
                     ) VALUES ('repository', '/repo', 'repo', 'before', 'before');
                     INSERT INTO work_items (
                       id, repository_id, title, source_kind, source_metadata_json,
                       markdown_body, lifecycle_status, created_at, updated_at
                     ) VALUES (
                       'work', 'repository', 'title', 'inline_markdown',
                       '{\"kind\":\"inline_markdown\"}', '# Work', 'open',
                       'before', 'before'
                     );
                     INSERT INTO planning_runs (
                       id, work_item_id, status, created_at, updated_at, completed_at
                     ) VALUES ('planning', 'work', 'succeeded', 'before', 'before', 'before');
                     INSERT INTO plans (
                       id, work_item_id, revision, markdown_body, approval_policy,
                       approval_status, created_at, updated_at, planning_run_id,
                       queue_eligibility_key, queue_eligible_at
                     ) VALUES (
                       'plan', 'work', 1, '# Plan', 'not_required', 'draft',
                       'before', 'before', 'planning', 'eligible', 'before'
                     );
                     INSERT INTO runs (
                       id, work_item_id, plan_id, phase, outcome, created_at, updated_at
                     ) VALUES (
                       'execution', 'work', 'plan', 'building', 'running', 'before', 'before'
                     );
                     INSERT INTO queue_entries (
                       id, work_item_id, position, scheduling_status, created_at,
                       updated_at, plan_id, idempotency_key, run_id
                     ) VALUES (
                       'queue', 'work', 0, 'queued', 'before', 'before',
                       'plan', 'eligible', 'execution'
                     );
                     INSERT INTO execution_runs (
                       run_id, queue_entry_id, source_repository_path, base_commit,
                       branch_name, worktree_path, ownership_token, copilot_program,
                       builder_session_id, builder_session_name, builder_model,
                       reviewer_session_id, reviewer_session_name, reviewer_model,
                       verification_program, verification_args_json, status, current_step,
                       builder_session_state, reviewer_session_state,
                       idempotency_key, created_at, updated_at
                     ) VALUES (
                       'execution', 'queue', '/repo', 'base', 'quorum/work-execution',
                       '/app/worktree', 'owner', 'copilot', 'builder-id', 'builder-session', 'builder',
                       'reviewer-id', 'reviewer-session', 'reviewer',
                       'make', '[\"check\"]', 'building', 'building', 'launching', 'launching',
                       'execution-key', 'before', 'before'
                     );
                     INSERT INTO execution_attempts (
                       id, run_id, number, reason, status, started_at
                     ) VALUES (
                       'attempt', 'execution', 1, 'start', 'running', 'before'
                     );
                     INSERT INTO execution_commands (
                       id, run_id, execution_attempt_id, ordinal, phase, program,
                       args_json, cwd, status, started_at
                     ) VALUES (
                       'command', 'execution', 'attempt', 0, 'building', 'copilot',
                       '[]', '/app/worktree', 'running', 'before'
                     );",
                )?;
                Ok(())
            })
            .expect("seed interrupted execution");

        drop(store);
        let reopened = AppStore::open(directory.path()).expect("recover on reopen");
        reopened
            .with_connection(|connection| {
                let execution: (String, Option<String>, String, String, String) = connection
                    .query_row(
                        "SELECT status, error_code, current_step,
                            builder_session_state, reviewer_session_state
                     FROM execution_runs WHERE run_id = 'execution'",
                        [],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                            ))
                        },
                    )?;
                let run: String = connection.query_row(
                    "SELECT outcome FROM runs WHERE id = 'execution'",
                    [],
                    |row| row.get(0),
                )?;
                let attempt: String = connection.query_row(
                    "SELECT status FROM execution_attempts WHERE id = 'attempt'",
                    [],
                    |row| row.get(0),
                )?;
                let command: String = connection.query_row(
                    "SELECT status FROM execution_commands WHERE id = 'command'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(
                    execution,
                    (
                        "blocked".to_owned(),
                        Some("interrupted".to_owned()),
                        "building".to_owned(),
                        "not_started".to_owned(),
                        "not_started".to_owned()
                    )
                );
                assert_eq!(run, "blocked");
                assert_eq!(attempt, "interrupted");
                assert_eq!(command, "interrupted");
                Ok(())
            })
            .expect("verify interrupted execution");
    }
}
