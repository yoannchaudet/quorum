use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
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
];

#[derive(Debug, Clone)]
pub struct AppStore {
    path: PathBuf,
}

impl AppStore {
    pub fn open(app_data_dir: impl AsRef<Path>) -> Result<Self, AppError> {
        let app_data_dir = app_data_dir.as_ref();
        fs::create_dir_all(app_data_dir).map_err(StoreError::from)?;
        let store = Self {
            path: app_data_dir.join("quorum.sqlite3"),
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
                assert_eq!(version, 7);
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
                assert_eq!(version, 7);
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
                assert_eq!(version, 7);
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
}
