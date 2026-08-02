use std::fs;
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
    pub require_plan_approval: bool,
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
    pub require_plan_approval: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct IntakeLocalMarkdownRequest {
    pub repository_id: String,
    pub path: String,
    pub require_plan_approval: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct IntakeGithubIssueRequest {
    pub repository_id: String,
    pub reference: String,
    pub require_plan_approval: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct IntakeWorkItemRequest {
    pub repository_id: String,
    pub require_plan_approval: bool,
    pub source: WorkItemSourceRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum WorkItemSourceRequest {
    InlineMarkdown { title: String, body: String },
    LocalMarkdown { path: String },
    GithubIssue { reference: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WorkItemSourceMetadata {
    InlineMarkdown,
    LocalMarkdown {
        path: String,
    },
    GithubIssue {
        owner: String,
        repository: String,
        number: u64,
        url: String,
    },
}

struct NormalizedWorkItem {
    title: String,
    source_kind: &'static str,
    source_metadata_json: String,
    markdown_body: String,
}

#[derive(Debug)]
struct CommandOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

trait CommandRunner {
    fn run(&self, program: &str, arguments: &[String]) -> std::io::Result<CommandOutput>;
}

struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, arguments: &[String]) -> std::io::Result<CommandOutput> {
        Command::new(program)
            .args(arguments)
            .output()
            .map(|output| CommandOutput {
                success: output.status.success(),
                stdout: output.stdout,
                stderr: output.stderr,
            })
    }
}

static SYSTEM_COMMAND_RUNNER: SystemCommandRunner = SystemCommandRunner;

pub struct RepositoryService<'a> {
    store: &'a AppStore,
    command_runner: &'a dyn CommandRunner,
}

impl<'a> RepositoryService<'a> {
    pub const fn new(store: &'a AppStore) -> Self {
        Self {
            store,
            command_runner: &SYSTEM_COMMAND_RUNNER,
        }
    }

    #[cfg(test)]
    fn with_command_runner(store: &'a AppStore, command_runner: &'a dyn CommandRunner) -> Self {
        Self {
            store,
            command_runner,
        }
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
                "SELECT id, repository_id, title, source_kind, markdown_body, lifecycle_status,
                        require_plan_approval, created_at, updated_at
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
        self.intake_work_item(IntakeWorkItemRequest {
            repository_id: request.repository_id,
            require_plan_approval: request.require_plan_approval,
            source: WorkItemSourceRequest::InlineMarkdown {
                title: request.title,
                body: request.markdown_body,
            },
        })
    }

    pub fn intake_local_markdown(
        &self,
        request: IntakeLocalMarkdownRequest,
    ) -> Result<WorkItemDto, AppError> {
        self.intake_work_item(IntakeWorkItemRequest {
            repository_id: request.repository_id,
            require_plan_approval: request.require_plan_approval,
            source: WorkItemSourceRequest::LocalMarkdown { path: request.path },
        })
    }

    pub fn intake_github_issue(
        &self,
        request: IntakeGithubIssueRequest,
    ) -> Result<WorkItemDto, AppError> {
        self.intake_work_item(IntakeWorkItemRequest {
            repository_id: request.repository_id,
            require_plan_approval: request.require_plan_approval,
            source: WorkItemSourceRequest::GithubIssue {
                reference: request.reference,
            },
        })
    }

    pub fn intake_work_item(
        &self,
        request: IntakeWorkItemRequest,
    ) -> Result<WorkItemDto, AppError> {
        let normalized = normalize_source(request.source, self.command_runner)?;
        let item = WorkItemDto {
            id: Uuid::new_v4().to_string(),
            repository_id: request.repository_id,
            title: normalized.title,
            source_kind: normalized.source_kind.to_owned(),
            markdown_body: normalized.markdown_body,
            lifecycle_status: "open".to_owned(),
            require_plan_approval: request.require_plan_approval,
            created_at: now(),
            updated_at: now(),
        };
        self.store.with_connection(|connection| {
            active_repository(connection, &item.repository_id)?;
            connection.execute(
                "INSERT INTO work_items (
                     id, repository_id, title, source_kind, source_metadata_json, markdown_body,
                     lifecycle_status, require_plan_approval, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    item.id,
                    item.repository_id,
                    item.title,
                    item.source_kind,
                    normalized.source_metadata_json,
                    item.markdown_body,
                    item.lifecycle_status,
                    item.require_plan_approval,
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
                            work_items.markdown_body, work_items.lifecycle_status,
                            work_items.require_plan_approval, work_items.created_at, work_items.updated_at
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

fn normalize_source(
    source: WorkItemSourceRequest,
    command_runner: &dyn CommandRunner,
) -> Result<NormalizedWorkItem, AppError> {
    match source {
        WorkItemSourceRequest::InlineMarkdown { title, body } => normalized_work_item(
            &title,
            "inline_markdown",
            &WorkItemSourceMetadata::InlineMarkdown,
            &body,
        ),
        WorkItemSourceRequest::LocalMarkdown { path } => normalize_local_markdown(&path),
        WorkItemSourceRequest::GithubIssue { reference } => {
            normalize_github_issue(&reference, command_runner)
        }
    }
}

fn normalize_local_markdown(path: &str) -> Result<NormalizedWorkItem, AppError> {
    let selected_path = Path::new(path);
    if !selected_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return Err(AppError::validation(
            "Choose a Markdown file with a .md extension.",
        ));
    }
    let canonical_path = selected_path.canonicalize().map_err(|error| {
        AppError::path(format!(
            "The selected Markdown file cannot be accessed: {error}"
        ))
    })?;
    if !canonical_path.is_file() {
        return Err(AppError::validation("Choose a local Markdown file."));
    }
    let bytes = fs::read(&canonical_path).map_err(|error| {
        AppError::path(format!(
            "The selected Markdown file cannot be read: {error}"
        ))
    })?;
    let body = String::from_utf8(bytes).map_err(|_| {
        AppError::validation("The selected Markdown file must contain valid UTF-8 text.")
    })?;
    let title = markdown_title(&body).or_else(|| {
        canonical_path
            .file_stem()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
    });
    let title = title.ok_or_else(|| {
        AppError::validation("The selected Markdown file needs a valid UTF-8 file name.")
    })?;
    normalized_work_item(
        &title,
        "local_markdown",
        &WorkItemSourceMetadata::LocalMarkdown {
            path: canonical_path.to_string_lossy().into_owned(),
        },
        &body,
    )
}

#[derive(Deserialize)]
struct GithubIssueResponse {
    title: String,
    body: Option<String>,
    url: String,
    number: u64,
}

fn normalize_github_issue(
    reference: &str,
    command_runner: &dyn CommandRunner,
) -> Result<NormalizedWorkItem, AppError> {
    let issue = parse_github_issue_reference(reference)?;
    let arguments = vec![
        "issue".to_owned(),
        "view".to_owned(),
        issue.number.to_string(),
        "--repo".to_owned(),
        format!("{}/{}", issue.owner, issue.repository),
        "--json".to_owned(),
        "title,body,url,number".to_owned(),
    ];
    let output = command_runner.run("gh", &arguments).map_err(|error| {
        AppError::github(format!(
            "GitHub CLI could not read the issue. Install `gh`, run `gh auth login`, and try again: {error}"
        ))
    })?;
    if !output.success {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        let detail = if detail.is_empty() {
            "GitHub CLI returned no details."
        } else {
            detail
        };
        return Err(AppError::github(format!(
            "GitHub CLI could not read {}/{}#{}. Check the reference and run `gh auth status`: {detail}",
            issue.owner, issue.repository, issue.number
        )));
    }
    let response: GithubIssueResponse =
        serde_json::from_slice(&output.stdout).map_err(|error| {
            AppError::github(format!(
                "GitHub CLI returned invalid issue data. Update `gh` and try again: {error}"
            ))
        })?;
    if response.number != issue.number {
        return Err(AppError::github(
            "GitHub CLI returned a different issue than requested.",
        ));
    }
    let source_metadata = WorkItemSourceMetadata::GithubIssue {
        owner: issue.owner,
        repository: issue.repository,
        number: response.number,
        url: response.url,
    };
    normalized_work_item(
        &response.title,
        "github_issue",
        &source_metadata,
        response.body.as_deref().unwrap_or_default(),
    )
}

#[derive(Debug)]
struct GithubIssueReference {
    owner: String,
    repository: String,
    number: u64,
}

fn parse_github_issue_reference(reference: &str) -> Result<GithubIssueReference, AppError> {
    let reference = reference.trim();
    let (owner, repository, number) =
        if let Some(path) = reference.strip_prefix("https://github.com/") {
            let path = path.trim_end_matches('/');
            let mut parts = path.split('/');
            let parsed = (
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
            );
            match parsed {
                (Some(owner), Some(repository), Some("issues"), Some(number), None) => {
                    (owner, repository, number)
                }
                _ => return Err(invalid_github_issue_reference()),
            }
        } else if let Some((repository, number)) = reference.rsplit_once('#') {
            let Some((owner, repository)) = repository.split_once('/') else {
                return Err(invalid_github_issue_reference());
            };
            if repository.contains('/') {
                return Err(invalid_github_issue_reference());
            }
            (owner, repository, number)
        } else {
            return Err(invalid_github_issue_reference());
        };

    if !valid_github_name(owner) || !valid_github_name(repository) {
        return Err(invalid_github_issue_reference());
    }
    let number = number
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(invalid_github_issue_reference)?;
    Ok(GithubIssueReference {
        owner: owner.to_owned(),
        repository: repository.to_owned(),
        number,
    })
}

fn valid_github_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn invalid_github_issue_reference() -> AppError {
    AppError::validation(
        "Enter a GitHub issue URL or an issue reference in owner/repository#number form.",
    )
}

fn markdown_title(markdown: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        line.strip_prefix("# ")
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned)
    })
}

fn normalized_work_item(
    title: &str,
    source_kind: &'static str,
    source_metadata: &WorkItemSourceMetadata,
    markdown_body: &str,
) -> Result<NormalizedWorkItem, AppError> {
    let title = title.trim();
    if title.is_empty() || title.len() > 500 {
        return Err(AppError::validation(
            "A work item title must contain between 1 and 500 characters.",
        ));
    }
    let source_metadata_json = serde_json::to_string(&source_metadata).map_err(|error| {
        AppError::database(format!(
            "Quorum could not preserve the work item source metadata: {error}"
        ))
    })?;
    Ok(NormalizedWorkItem {
        title: title.to_owned(),
        source_kind,
        source_metadata_json,
        markdown_body: normalize_markdown(markdown_body),
    })
}

fn normalize_markdown(markdown: &str) -> String {
    markdown.replace("\r\n", "\n").replace('\r', "\n")
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
        require_plan_approval: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
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
    use std::cell::RefCell;
    use std::fs;
    use std::process::Command;

    use tempfile::tempdir;

    use super::{
        parse_github_issue_reference, CommandOutput, CommandRunner, CreateWorkItemRequest,
        IntakeWorkItemRequest, RegisterRepositoryRequest, RepositoryDto, RepositoryService,
        WorkItemSourceRequest,
    };
    use crate::state::AppStore;

    struct FakeCommandRunner {
        success: bool,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl FakeCommandRunner {
        fn success(json: &serde_json::Value) -> Self {
            Self {
                success: true,
                stdout: serde_json::to_vec(&json).expect("serialize fake response"),
                stderr: Vec::new(),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn failure(stderr: &str) -> Self {
            Self {
                success: false,
                stdout: Vec::new(),
                stderr: stderr.as_bytes().to_vec(),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl CommandRunner for FakeCommandRunner {
        fn run(&self, program: &str, arguments: &[String]) -> std::io::Result<CommandOutput> {
            self.calls
                .borrow_mut()
                .push((program.to_owned(), arguments.to_vec()));
            Ok(CommandOutput {
                success: self.success,
                stdout: self.stdout.clone(),
                stderr: self.stderr.clone(),
            })
        }
    }

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

    fn assert_source_metadata(store: &AppStore, markdown_path: &std::path::Path) {
        let metadata = store
            .with_connection(|connection| {
                let mut statement = connection.prepare(
                    "SELECT source_kind, source_metadata_json
                     FROM work_items ORDER BY source_kind",
                )?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .expect("source metadata")
            .into_iter()
            .map(|(kind, json)| {
                (
                    kind,
                    serde_json::from_str::<serde_json::Value>(&json).expect("metadata JSON"),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(metadata[0].0, "github_issue");
        assert_eq!(metadata[0].1["owner"], "octo");
        assert_eq!(metadata[0].1["repository"], "project");
        assert_eq!(metadata[0].1["number"], 42);
        assert_eq!(
            metadata[0].1["url"],
            "https://github.com/octo/project/issues/42"
        );
        assert_eq!(metadata[1].0, "inline_markdown");
        assert_eq!(
            metadata[1].1,
            serde_json::json!({"kind": "inline_markdown"})
        );
        assert_eq!(metadata[2].0, "local_markdown");
        assert_eq!(
            metadata[2].1["path"],
            markdown_path
                .canonicalize()
                .expect("canonical Markdown path")
                .to_string_lossy()
                .as_ref()
        );
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
                require_plan_approval: true,
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
                require_plan_approval: true,
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
    fn normalizes_all_intake_sources_and_preserves_metadata() {
        let app_data = tempdir().expect("app data");
        let source_directory = tempdir().expect("source directory");
        let markdown_path = source_directory.path().join("requirements.md");
        fs::write(&markdown_path, "# Work\r\n\r\nDetails\r\n").expect("write Markdown");
        let store = AppStore::open(app_data.path()).expect("store");
        store
            .with_connection(|connection| {
                connection.execute(
                    "INSERT INTO repositories (
                       id, root_path, display_name, created_at, updated_at
                     ) VALUES ('repository', '/repo', 'repo', 'now', 'now')",
                    [],
                )?;
                Ok(())
            })
            .expect("seed repository");
        let runner = FakeCommandRunner::success(&serde_json::json!({
            "title": "Work",
            "body": "# Work\r\n\r\nDetails\r\n",
            "url": "https://github.com/octo/project/issues/42",
            "number": 42
        }));
        let service = RepositoryService::with_command_runner(&store, &runner);

        let inline = service
            .intake_work_item(IntakeWorkItemRequest {
                repository_id: "repository".to_owned(),
                require_plan_approval: true,
                source: WorkItemSourceRequest::InlineMarkdown {
                    title: " Work ".to_owned(),
                    body: "# Work\r\n\r\nDetails\r\n".to_owned(),
                },
            })
            .expect("inline intake");
        let local = service
            .intake_work_item(IntakeWorkItemRequest {
                repository_id: "repository".to_owned(),
                require_plan_approval: true,
                source: WorkItemSourceRequest::LocalMarkdown {
                    path: markdown_path.to_string_lossy().into_owned(),
                },
            })
            .expect("local intake");
        let github = service
            .intake_work_item(IntakeWorkItemRequest {
                repository_id: "repository".to_owned(),
                require_plan_approval: true,
                source: WorkItemSourceRequest::GithubIssue {
                    reference: "octo/project#42".to_owned(),
                },
            })
            .expect("GitHub intake");

        assert_eq!(inline.title, local.title);
        assert_eq!(local.title, github.title);
        assert_eq!(inline.markdown_body, local.markdown_body);
        assert_eq!(local.markdown_body, github.markdown_body);
        assert_eq!(inline.markdown_body, "# Work\n\nDetails\n");
        assert_eq!(inline.source_kind, "inline_markdown");
        assert_eq!(local.source_kind, "local_markdown");
        assert_eq!(github.source_kind, "github_issue");
        assert_eq!(
            runner.calls.borrow().as_slice(),
            [(
                "gh".to_owned(),
                vec![
                    "issue".to_owned(),
                    "view".to_owned(),
                    "42".to_owned(),
                    "--repo".to_owned(),
                    "octo/project".to_owned(),
                    "--json".to_owned(),
                    "title,body,url,number".to_owned(),
                ]
            )]
        );

        assert_source_metadata(&store, &markdown_path);
    }

    #[test]
    fn validates_local_and_github_intake_failures() {
        let app_data = tempdir().expect("app data");
        let source_directory = tempdir().expect("source directory");
        let invalid_utf8 = source_directory.path().join("invalid.md");
        fs::write(&invalid_utf8, [0xFF, 0xFE]).expect("write invalid UTF-8");
        let store = AppStore::open(app_data.path()).expect("store");
        let runner = FakeCommandRunner::failure("authentication required");
        let service = RepositoryService::with_command_runner(&store, &runner);

        let invalid_file = service
            .intake_work_item(IntakeWorkItemRequest {
                repository_id: "repository".to_owned(),
                require_plan_approval: true,
                source: WorkItemSourceRequest::LocalMarkdown {
                    path: invalid_utf8.to_string_lossy().into_owned(),
                },
            })
            .expect_err("invalid UTF-8");
        assert_eq!(invalid_file.code, "validation");
        assert!(invalid_file.message.contains("UTF-8"));

        let invalid_reference = service
            .intake_work_item(IntakeWorkItemRequest {
                repository_id: "repository".to_owned(),
                require_plan_approval: true,
                source: WorkItemSourceRequest::GithubIssue {
                    reference: "not-an-issue".to_owned(),
                },
            })
            .expect_err("invalid reference");
        assert_eq!(invalid_reference.code, "validation");
        assert!(runner.calls.borrow().is_empty());

        let command_failure = service
            .intake_work_item(IntakeWorkItemRequest {
                repository_id: "repository".to_owned(),
                require_plan_approval: true,
                source: WorkItemSourceRequest::GithubIssue {
                    reference: "https://github.com/octo/project/issues/42".to_owned(),
                },
            })
            .expect_err("GitHub failure");
        assert_eq!(command_failure.code, "external");
        assert!(command_failure.message.contains("gh auth status"));
        assert!(command_failure.message.contains("authentication required"));
        assert_eq!(
            command_failure.recovery.as_deref(),
            Some("Check that GitHub CLI is installed and authenticated, then try again.")
        );
    }

    #[test]
    fn parses_supported_github_issue_reference_forms() {
        for reference in [
            "octo/project#42",
            "https://github.com/octo/project/issues/42",
            " https://github.com/octo/project/issues/42/ ",
        ] {
            let parsed = parse_github_issue_reference(reference).expect("valid reference");
            assert_eq!(parsed.owner, "octo");
            assert_eq!(parsed.repository, "project");
            assert_eq!(parsed.number, 42);
        }
        for reference in [
            "octo/project",
            "octo/project#0",
            "octo/other/project#42",
            "http://github.com/octo/project/issues/42",
        ] {
            assert_eq!(
                parse_github_issue_reference(reference)
                    .expect_err("invalid reference")
                    .code,
                "validation"
            );
        }
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
            "markdownBody": "# Work",
            "requirePlanApproval": true
        }))
        .expect("deserialize work item request");
        assert_eq!(request.repository_id, "repository-id");
        assert_eq!(request.markdown_body, "# Work");

        let intake: IntakeWorkItemRequest = serde_json::from_value(serde_json::json!({
            "repositoryId": "repository-id",
            "requirePlanApproval": false,
            "source": {
                "kind": "github_issue",
                "reference": "octo/project#42"
            }
        }))
        .expect("deserialize intake request");
        assert_eq!(intake.repository_id, "repository-id");
        assert!(matches!(
            intake.source,
            WorkItemSourceRequest::GithubIssue { reference }
                if reference == "octo/project#42"
        ));
    }
}
