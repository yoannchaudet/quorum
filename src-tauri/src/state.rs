use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, Transaction};

use crate::error::{AppError, StoreError};

const APPLICATION_ID: i32 = 0x5155_4F52;
const MIGRATIONS: &[&str] = &[include_str!("../migrations/0001_initial.sql")];

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
        store.with_connection(|_| Ok(()))?;
        Ok(store)
    }

    #[cfg(test)]
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

    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| AppError::migration(format!("Could not start migration: {error}")))?;
        apply_migration(&transaction, sql, index + 1)?;
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

    use super::AppStore;

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
                           id, repository_id, title, source_kind, markdown_body, lifecycle_status, created_at, updated_at
                         ) VALUES ('work', 'missing', 'title', 'inline_markdown', '', 'open', 'now', 'now')",
                        [],
                    )
                    .is_err());
                let version: i32 =
                    connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
                assert_eq!(version, 1);
                Ok(())
            })
            .expect("schema constraints");
    }
}
