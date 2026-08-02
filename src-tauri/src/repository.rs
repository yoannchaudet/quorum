use std::path::Path;
use std::process::Command;

use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::error::{AppError, StoreError};
use crate::state::AppStore;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RepositoryDto {
    pub id: String,
    pub root_path: String,
    pub display_name: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RegisterRepositoryRequest {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkItemDto {
    pub id: String,
    pub repository_id: String,
    pub title: String,
    pub source_kind: String,
    pub markdown_body: String,
    pub lifecycle_status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct CreateWorkItemRequest {
    pub repository_id: String,
    pub title: String,
    pub markdown_body: String,
}

pub struct RepositoryService<'a> {
    store: &'a AppStore,
}

impl<'a> RepositoryService<'a> {
    pub const fn new(store: &'a AppStore) -> Self {
        Self { store }
    }

    pub fn list_active(&self) -> Result<Vec<RepositoryDto>, AppError> {
        self.store.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, root_path, display_name, created_at, updated_at
                 FROM repositories WHERE archived_at IS NULL ORDER BY display_name COLLATE NOCASE, id",
            )?;
            let repositories = statement
                .query_map([], repository_from_row)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(repositories)
        })
    }

    pub fn register(&self, request: &RegisterRepositoryRequest) -> Result<RepositoryDto, AppError> {
        let root_path = git_root(&request.path)?;
        let display_name = root_path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                AppError::validation("The Git repository root needs a valid folder name.")
            })?
            .to_owned();
        let root_path = root_path.to_string_lossy().into_owned();
        let now = now();

        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let existing = transaction
                .query_row(
                    "SELECT id, root_path, display_name, created_at, updated_at
                     FROM repositories WHERE root_path = ?1",
                    [&root_path],
                    repository_from_row,
                )
                .optional()?;

            let repository = if let Some(existing) = existing {
                transaction.execute(
                    "UPDATE repositories
                     SET archived_at = NULL, display_name = ?2, updated_at = ?3
                     WHERE id = ?1",
                    params![existing.id, display_name, now],
                )?;
                RepositoryDto {
                    display_name,
                    updated_at: now,
                    ..existing
                }
            } else {
                let repository = RepositoryDto {
                    id: Uuid::new_v4().to_string(),
                    root_path,
                    display_name,
                    created_at: now.clone(),
                    updated_at: now,
                };
                transaction.execute(
                    "INSERT INTO repositories (id, root_path, display_name, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        repository.id,
                        repository.root_path,
                        repository.display_name,
                        repository.created_at,
                        repository.updated_at
                    ],
                )?;
                repository
            };
            transaction.commit()?;
            Ok(repository)
        })
    }

    pub fn archive(&self, repository_id: &str) -> Result<(), AppError> {
        self.store.with_connection(|connection| {
            let changed = connection.execute(
                "UPDATE repositories SET archived_at = ?2, updated_at = ?2
                 WHERE id = ?1 AND archived_at IS NULL",
                params![repository_id, now()],
            )?;
            if changed == 0 {
                return Err(StoreError::App(AppError::not_found(
                    "The active repository could not be found.",
                )));
            }
            Ok(())
        })
    }

    pub fn list_work_items(&self, repository_id: &str) -> Result<Vec<WorkItemDto>, AppError> {
        self.store.with_connection(|connection| {
            active_repository(connection, repository_id)?;
            let mut statement = connection.prepare(
                "SELECT id, repository_id, title, source_kind, markdown_body, lifecycle_status, created_at, updated_at
                 FROM work_items WHERE repository_id = ?1 ORDER BY updated_at DESC, id",
            )?;
            let items = statement
                .query_map([repository_id], work_item_from_row)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(items)
        })
    }

    pub fn create_work_item(
        &self,
        request: CreateWorkItemRequest,
    ) -> Result<WorkItemDto, AppError> {
        let title = request.title.trim();
        if title.is_empty() || title.len() > 500 {
            return Err(AppError::validation(
                "A work item title must contain between 1 and 500 characters.",
            ));
        }
        let item = WorkItemDto {
            id: Uuid::new_v4().to_string(),
            repository_id: request.repository_id,
            title: title.to_owned(),
            source_kind: "inline_markdown".to_owned(),
            markdown_body: request.markdown_body,
            lifecycle_status: "open".to_owned(),
            created_at: now(),
            updated_at: now(),
        };
        self.store.with_connection(|connection| {
            active_repository(connection, &item.repository_id)?;
            connection.execute(
                "INSERT INTO work_items (
                     id, repository_id, title, source_kind, markdown_body, lifecycle_status, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    item.id,
                    item.repository_id,
                    item.title,
                    item.source_kind,
                    item.markdown_body,
                    item.lifecycle_status,
                    item.created_at,
                    item.updated_at
                ],
            )?;
            Ok(item)
        })
    }

    pub fn get_work_item(&self, work_item_id: &str) -> Result<WorkItemDto, AppError> {
        self.store.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT work_items.id, work_items.repository_id, work_items.title, work_items.source_kind,
                            work_items.markdown_body, work_items.lifecycle_status, work_items.created_at, work_items.updated_at
                     FROM work_items JOIN repositories ON repositories.id = work_items.repository_id
                     WHERE work_items.id = ?1 AND repositories.archived_at IS NULL",
                    [work_item_id],
                    work_item_from_row,
                )
                .optional()?
                .ok_or_else(|| StoreError::App(AppError::not_found("The work item could not be found.")))
        })
    }
}

fn active_repository(
    connection: &rusqlite::Connection,
    repository_id: &str,
) -> Result<(), StoreError> {
    let is_active = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM repositories WHERE id = ?1 AND archived_at IS NULL)",
        [repository_id],
        |row| row.get::<_, bool>(0),
    )?;
    if is_active {
        Ok(())
    } else {
        Err(AppError::not_found("The active repository could not be found.").into())
    }
}

fn repository_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RepositoryDto> {
    Ok(RepositoryDto {
        id: row.get(0)?,
        root_path: row.get(1)?,
        display_name: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

fn work_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkItemDto> {
    Ok(WorkItemDto {
        id: row.get(0)?,
        repository_id: row.get(1)?,
        title: row.get(2)?,
        source_kind: row.get(3)?,
        markdown_body: row.get(4)?,
        lifecycle_status: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn git_root(path: &str) -> Result<std::path::PathBuf, AppError> {
    let selected_path = Path::new(path);
    let canonical_path = selected_path.canonicalize().map_err(|error| {
        AppError::path(format!("The selected folder cannot be accessed: {error}"))
    })?;
    if !canonical_path.is_dir() {
        return Err(AppError::validation(
            "Choose a folder containing a Git repository.",
        ));
    }
    let output = Command::new("git")
        .args(["-C"])
        .arg(&canonical_path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|error| {
            AppError::validation(format!("Git could not validate this folder: {error}"))
        })?;
    if !output.status.success() {
        return Err(AppError::validation(
            "The selected folder is not inside a Git working tree.",
        ));
    }
    let root = String::from_utf8(output.stdout)
        .map_err(|_| AppError::validation("Git returned an invalid repository path."))?;
    Path::new(root.trim()).canonicalize().map_err(|error| {
        AppError::path(format!(
            "The Git repository root cannot be accessed: {error}"
        ))
    })
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use tempfile::tempdir;

    use super::{
        CreateWorkItemRequest, RegisterRepositoryRequest, RepositoryDto, RepositoryService,
    };
    use crate::state::AppStore;

    fn git_repository() -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempdir().expect("temp dir");
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).expect("repository directory");
        assert!(Command::new("git")
            .args(["init", "--quiet"])
            .arg(&repository)
            .status()
            .expect("git init")
            .success());
        (directory, repository)
    }

    #[test]
    fn normalizes_subdirectories_and_restores_archived_registrations() {
        let app_data = tempdir().expect("app data");
        let (_repository_dir, repository) = git_repository();
        let nested = repository.join("nested");
        fs::create_dir(&nested).expect("nested");
        let store = AppStore::open(app_data.path()).expect("store");
        let service = RepositoryService::new(&store);
        let registered = service
            .register(&RegisterRepositoryRequest {
                path: nested.to_string_lossy().into_owned(),
            })
            .expect("register");
        assert_eq!(
            registered.root_path,
            repository
                .canonicalize()
                .expect("canonical")
                .to_string_lossy()
        );
        service.archive(&registered.id).expect("archive");
        assert!(service.list_active().expect("list").is_empty());
        let restored = service
            .register(&RegisterRepositoryRequest {
                path: repository.to_string_lossy().into_owned(),
            })
            .expect("restore");
        assert_eq!(restored.id, registered.id);
        assert_eq!(service.list_active().expect("list").len(), 1);
    }

    #[test]
    fn rejects_non_git_directories_and_retains_work_items_after_archiving() {
        let app_data = tempdir().expect("app data");
        let plain = tempdir().expect("plain");
        let store = AppStore::open(app_data.path()).expect("store");
        let service = RepositoryService::new(&store);
        assert_eq!(
            service
                .register(&RegisterRepositoryRequest {
                    path: plain.path().to_string_lossy().into_owned()
                })
                .expect_err("non git")
                .code,
            "validation"
        );

        let (_repository_dir, repository) = git_repository();
        let registered = service
            .register(&RegisterRepositoryRequest {
                path: repository.to_string_lossy().into_owned(),
            })
            .expect("register");
        let item = service
            .create_work_item(CreateWorkItemRequest {
                repository_id: registered.id.clone(),
                title: "Persist this".to_owned(),
                markdown_body: "# Notes".to_owned(),
            })
            .expect("create");
        service.archive(&registered.id).expect("archive");
        let count: i64 = store
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT count(*) FROM work_items WHERE id = ?1",
                        [&item.id],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })
            .expect("count");
        assert_eq!(count, 1);
    }

    #[test]
    fn creates_lists_and_reads_persisted_work_items() {
        let app_data = tempdir().expect("app data");
        let (_repository_dir, repository) = git_repository();
        let store = AppStore::open(app_data.path()).expect("store");
        let service = RepositoryService::new(&store);
        let registered = service
            .register(&RegisterRepositoryRequest {
                path: repository.to_string_lossy().into_owned(),
            })
            .expect("register");
        let created = service
            .create_work_item(CreateWorkItemRequest {
                repository_id: registered.id.clone(),
                title: "  Work item  ".to_owned(),
                markdown_body: "**Saved**".to_owned(),
            })
            .expect("create");
        assert_eq!(created.title, "Work item");
        assert_eq!(
            service.list_work_items(&registered.id).expect("list"),
            vec![created.clone()]
        );
        assert_eq!(service.get_work_item(&created.id).expect("read"), created);
    }

    #[test]
    fn ipc_dtos_use_camel_case_json() {
        let repository = RepositoryDto {
            id: "repository-id".to_owned(),
            root_path: "/tmp/repository".to_owned(),
            display_name: "repository".to_owned(),
            created_at: "created".to_owned(),
            updated_at: "updated".to_owned(),
        };
        let value = serde_json::to_value(repository).expect("serialize repository");
        assert_eq!(value["rootPath"], "/tmp/repository");
        assert_eq!(value["displayName"], "repository");
        assert!(value.get("root_path").is_none());

        let request: CreateWorkItemRequest = serde_json::from_value(serde_json::json!({
            "repositoryId": "repository-id",
            "title": "Work",
            "markdownBody": "# Work"
        }))
        .expect("deserialize work item request");
        assert_eq!(request.repository_id, "repository-id");
        assert_eq!(request.markdown_body, "# Work");
    }
}
