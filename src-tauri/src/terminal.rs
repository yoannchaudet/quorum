use std::io;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::copilot::{ProcessOutput, ProcessRunner, PLANNING_SAFETY_ARGUMENTS};
use crate::error::{AppError, StoreError};
use crate::planning::{
    PlanningExecutor, PlanningService, PlanningStateDto, ReconcilePlanningAgentRequest,
};
use crate::settings::expand_terminal_arguments;
use crate::state::AppStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalLaunchOutcome {
    pub completion_observed: bool,
}

pub trait TerminalOpener: Send + Sync {
    fn open(
        &self,
        terminal_application: &str,
        terminal_arguments: &str,
        repository_path: &str,
        session_name: &str,
    ) -> Result<TerminalLaunchOutcome, AppError>;

    fn open_session(
        &self,
        terminal_application: &str,
        terminal_arguments: &str,
        repository_path: &str,
        session_name: &str,
    ) -> Result<(), AppError> {
        self.open(
            terminal_application,
            terminal_arguments,
            repository_path,
            session_name,
        )
        .map(|_| ())
    }
}

pub struct TerminalLauncher<R> {
    runner: R,
}

impl<R: ProcessRunner> TerminalLauncher<R> {
    pub const fn new(runner: R) -> Self {
        Self { runner }
    }
}

impl<R: ProcessRunner> TerminalOpener for TerminalLauncher<R> {
    fn open(
        &self,
        terminal_application: &str,
        terminal_arguments: &str,
        repository_path: &str,
        session_name: &str,
    ) -> Result<TerminalLaunchOutcome, AppError> {
        let mut arguments = expand_terminal_arguments(
            terminal_arguments,
            terminal_application,
            repository_path,
            session_name,
        )?;
        for safety_argument in PLANNING_SAFETY_ARGUMENTS {
            if !arguments.iter().any(|argument| argument == safety_argument) {
                arguments.push(safety_argument.to_owned());
            }
        }
        let open_argument_count = arguments
            .iter()
            .position(|argument| argument == "--args")
            .unwrap_or(arguments.len());
        let completion_observed = arguments[..open_argument_count]
            .iter()
            .any(|argument| matches!(argument.as_str(), "-W" | "--wait-apps"));
        let output = self
            .runner
            .run("/usr/bin/open", &arguments)
            .map_err(|error| launcher_start_error(&error))?;
        if output.success {
            Ok(TerminalLaunchOutcome {
                completion_observed,
            })
        } else {
            Err(launcher_failure(&output))
        }
    }

    fn open_session(
        &self,
        terminal_application: &str,
        terminal_arguments: &str,
        repository_path: &str,
        session_name: &str,
    ) -> Result<(), AppError> {
        let mut arguments = expand_terminal_arguments(
            terminal_arguments,
            terminal_application,
            repository_path,
            session_name,
        )?;
        let open_argument_count = arguments
            .iter()
            .position(|argument| argument == "--args")
            .unwrap_or(arguments.len());
        arguments = arguments
            .into_iter()
            .enumerate()
            .filter_map(|(index, argument)| {
                if index < open_argument_count && matches!(argument.as_str(), "-W" | "--wait-apps")
                {
                    None
                } else {
                    Some(argument)
                }
            })
            .collect();
        for safety_argument in PLANNING_SAFETY_ARGUMENTS {
            if !arguments.iter().any(|argument| argument == safety_argument) {
                arguments.push(safety_argument.to_owned());
            }
        }
        let output = self
            .runner
            .run("/usr/bin/open", &arguments)
            .map_err(|error| launcher_start_error(&error))?;
        if output.success {
            Ok(())
        } else {
            Err(launcher_failure(&output))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct LaunchTerminalHandoffRequest {
    pub work_item_id: String,
    pub planning_agent_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct OpenCopilotSessionRequest {
    pub work_item_id: String,
    pub planning_agent_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ResumeTerminalHandoffRequest {
    pub work_item_id: String,
    pub terminal_handoff_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct TerminalHandoffDto {
    pub id: String,
    pub work_item_id: String,
    pub planning_run_id: String,
    pub planning_agent_id: String,
    pub session_name: String,
    pub status: String,
    pub completion_observable: Option<bool>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub manual_reconcile_available: bool,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub planning_state: Option<PlanningStateDto>,
}

pub struct TerminalHandoffService<'a, L, E: PlanningExecutor> {
    store: &'a AppStore,
    launcher: &'a L,
    planning: PlanningService<'a, E>,
}

impl<'a, L: TerminalOpener, E: PlanningExecutor> TerminalHandoffService<'a, L, E> {
    pub const fn with_dependencies(store: &'a AppStore, launcher: &'a L, executor: &'a E) -> Self {
        Self {
            store,
            launcher,
            planning: PlanningService::with_executor(store, executor),
        }
    }

    pub fn open_session(&self, request: &OpenCopilotSessionRequest) -> Result<(), AppError> {
        let work_item_id = required(&request.work_item_id, "A work item ID is required.")?;
        let planning_agent_id = required(
            &request.planning_agent_id,
            "A planning agent ID is required.",
        )?;
        let target = self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            load_target(&transaction, &work_item_id, &planning_agent_id)
        })?;
        self.launcher.open_session(
            &target.terminal_application,
            &target.terminal_arguments,
            &target.repository_path,
            &target.session_name,
        )
    }

    pub fn launch(
        &self,
        request: &LaunchTerminalHandoffRequest,
    ) -> Result<TerminalHandoffDto, AppError> {
        let work_item_id = required(&request.work_item_id, "A work item ID is required.")?;
        let planning_agent_id = required(
            &request.planning_agent_id,
            "A planning agent ID is required.",
        )?;
        let idempotency_key = required(
            &request.idempotency_key,
            "A terminal handoff idempotency key is required.",
        )?;
        let creation = self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            if let Some(existing) = load_handoff_by_key(&transaction, &idempotency_key)? {
                if existing.work_item_id != work_item_id
                    || existing.planning_agent_id != planning_agent_id
                {
                    return Err(StoreError::App(AppError::conflict(
                        "This terminal handoff idempotency key belongs to another target.",
                    )));
                }
                return Err(StoreError::App(AppError::conflict(
                    "This terminal handoff has already been started.",
                )));
            }
            let target = load_target(&transaction, &work_item_id, &planning_agent_id)?;
            if !matches!(target.agent_status.as_str(), "blocked" | "failed") {
                return Err(StoreError::App(AppError::conflict(
                    "Only a blocked or failed persisted planning agent can be handed off.",
                )));
            }
            let timestamp = now();
            let record = HandoffRecord {
                id: Uuid::new_v4().to_string(),
                work_item_id,
                planning_run_id: target.planning_run_id.clone(),
                planning_agent_id,
                session_name: target.session_name.clone(),
                status: "launching".to_owned(),
                completion_observable: None,
                error_code: None,
                error_message: None,
                created_at: timestamp.clone(),
                updated_at: timestamp,
                completed_at: None,
            };
            transaction.execute(
                "INSERT INTO terminal_handoffs (
                    id, work_item_id, planning_run_id, planning_agent_id, session_name,
                    idempotency_key, status, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'launching', ?7, ?7)",
                params![
                    record.id,
                    record.work_item_id,
                    record.planning_run_id,
                    record.planning_agent_id,
                    record.session_name,
                    idempotency_key,
                    record.created_at
                ],
            )?;
            transaction.commit()?;
            Ok(HandoffCreation::Created { record, target })
        })?;

        let HandoffCreation::Created { record, target } = creation;

        match self.launcher.open(
            &target.terminal_application,
            &target.terminal_arguments,
            &target.repository_path,
            &record.session_name,
        ) {
            Err(error) => {
                let record = self.set_status(
                    &record.id,
                    "launch_failed",
                    None,
                    Some((&error.code, &error.message)),
                    false,
                )?;
                Ok(Self::dto(record, None))
            }
            Ok(outcome) if !outcome.completion_observed => {
                let record = self.set_status(
                    &record.id,
                    "awaiting_manual_reconcile",
                    Some(false),
                    None,
                    false,
                )?;
                Ok(Self::dto(record, None))
            }
            Ok(_) => self.reconcile(&record, Some(true)),
        }
    }

    pub fn resume_and_reconcile(
        &self,
        request: &ResumeTerminalHandoffRequest,
    ) -> Result<TerminalHandoffDto, AppError> {
        let work_item_id = required(&request.work_item_id, "A work item ID is required.")?;
        let handoff_id = required(
            &request.terminal_handoff_id,
            "A terminal handoff ID is required.",
        )?;
        let record = self.store.with_connection(|connection| {
            load_handoff(connection, &handoff_id)?
                .filter(|record| record.work_item_id == work_item_id)
                .ok_or_else(|| {
                    StoreError::App(AppError::not_found(
                        "The terminal handoff does not belong to the requested active work item.",
                    ))
                })
        })?;
        if record.status == "reconciled" {
            return Err(AppError::conflict(
                "This terminal handoff has already been reconciled.",
            ));
        }
        if !matches!(
            record.status.as_str(),
            "launch_failed" | "awaiting_manual_reconcile" | "reconcile_failed"
        ) {
            return Err(AppError::conflict(
                "This terminal handoff is already being processed.",
            ));
        }
        self.reconcile(&record, None)
    }

    fn reconcile(
        &self,
        record: &HandoffRecord,
        completion_observable: Option<bool>,
    ) -> Result<TerminalHandoffDto, AppError> {
        if let Err(error) = self.ensure_same_active_session(record) {
            let record = self.set_status(
                &record.id,
                "reconcile_failed",
                completion_observable,
                Some((&error.code, &error.message)),
                false,
            )?;
            return Ok(Self::dto(record, None));
        }
        let record = self.set_status(
            &record.id,
            "reconciling",
            completion_observable,
            None,
            false,
        )?;
        let result = self
            .planning
            .reconcile_terminal_agent(&ReconcilePlanningAgentRequest {
                work_item_id: record.work_item_id.clone(),
                planning_agent_id: record.planning_agent_id.clone(),
            });
        match result {
            Err(error) => {
                let record = self.set_status(
                    &record.id,
                    "reconcile_failed",
                    completion_observable,
                    Some((&error.code, &error.message)),
                    false,
                )?;
                Ok(Self::dto(record, None))
            }
            Ok(state) => {
                let agent = state
                    .agents
                    .iter()
                    .find(|agent| agent.id == record.planning_agent_id)
                    .ok_or_else(|| {
                        AppError::database(
                            "The reconciled planning state omitted the selected planning agent.",
                        )
                    })?;
                if agent.status == "failed" || agent.error_code.is_some() {
                    let code = agent
                        .error_code
                        .as_deref()
                        .unwrap_or("terminal_reconcile_failed");
                    let message = agent.error_message.as_deref().unwrap_or(
                        "The exact Copilot session could not be reconciled after terminal work.",
                    );
                    let record = self.set_status(
                        &record.id,
                        "reconcile_failed",
                        completion_observable,
                        Some((code, message)),
                        false,
                    )?;
                    Ok(Self::dto(record, Some(state)))
                } else {
                    let record = self.set_status(
                        &record.id,
                        "reconciled",
                        completion_observable,
                        None,
                        true,
                    )?;
                    Ok(Self::dto(record, Some(state)))
                }
            }
        }
    }

    fn ensure_same_active_session(&self, record: &HandoffRecord) -> Result<(), AppError> {
        self.store.with_connection(|connection| {
            let session_name = connection
                .query_row(
                    "SELECT planning_agents.session_name
                     FROM planning_agents
                     JOIN planning_runs
                       ON planning_runs.id = planning_agents.planning_run_id
                     JOIN work_items ON work_items.id = planning_runs.work_item_id
                     JOIN repositories ON repositories.id = work_items.repository_id
                     WHERE planning_agents.id = ?1
                       AND planning_runs.id = ?2
                       AND work_items.id = ?3
                       AND work_items.lifecycle_status = 'open'
                       AND repositories.archived_at IS NULL",
                    params![
                        record.planning_agent_id,
                        record.planning_run_id,
                        record.work_item_id
                    ],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::App(AppError::not_found(
                        "The terminal handoff target is no longer active.",
                    ))
                })?;
            if session_name != record.session_name {
                return Err(StoreError::App(AppError::conflict(
                    "The persisted Copilot session no longer matches this terminal handoff.",
                )));
            }
            Ok(())
        })
    }

    fn set_status(
        &self,
        handoff_id: &str,
        status: &str,
        completion_observable: Option<bool>,
        error: Option<(&str, &str)>,
        completed: bool,
    ) -> Result<HandoffRecord, AppError> {
        self.store.with_connection(|connection| {
            let timestamp = now();
            connection.execute(
                "UPDATE terminal_handoffs
                 SET status = ?2,
                     completion_observable = COALESCE(?3, completion_observable),
                     error_code = ?4, error_message = ?5, updated_at = ?6,
                     completed_at = ?7
                 WHERE id = ?1",
                params![
                    handoff_id,
                    status,
                    completion_observable,
                    error.map(|value| value.0),
                    error.map(|value| value.1),
                    timestamp,
                    completed.then_some(timestamp.as_str())
                ],
            )?;
            load_handoff(connection, handoff_id)?.ok_or_else(|| {
                StoreError::App(AppError::not_found(
                    "The terminal handoff could not be found.",
                ))
            })
        })
    }

    fn dto(record: HandoffRecord, planning_state: Option<PlanningStateDto>) -> TerminalHandoffDto {
        TerminalHandoffDto {
            manual_reconcile_available: matches!(
                record.status.as_str(),
                "launch_failed" | "awaiting_manual_reconcile" | "reconcile_failed"
            ),
            id: record.id,
            work_item_id: record.work_item_id,
            planning_run_id: record.planning_run_id,
            planning_agent_id: record.planning_agent_id,
            session_name: record.session_name,
            status: record.status,
            completion_observable: record.completion_observable,
            error_code: record.error_code,
            error_message: record.error_message,
            created_at: record.created_at,
            updated_at: record.updated_at,
            completed_at: record.completed_at,
            planning_state,
        }
    }
}

struct HandoffTarget {
    planning_run_id: String,
    session_name: String,
    agent_status: String,
    repository_path: String,
    terminal_application: String,
    terminal_arguments: String,
}

enum HandoffCreation {
    Created {
        record: HandoffRecord,
        target: HandoffTarget,
    },
}

#[derive(Clone)]
struct HandoffRecord {
    id: String,
    work_item_id: String,
    planning_run_id: String,
    planning_agent_id: String,
    session_name: String,
    status: String,
    completion_observable: Option<bool>,
    error_code: Option<String>,
    error_message: Option<String>,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

fn load_target(
    transaction: &Transaction<'_>,
    work_item_id: &str,
    planning_agent_id: &str,
) -> Result<HandoffTarget, StoreError> {
    transaction
        .query_row(
            "SELECT planning_runs.id, planning_agents.session_name,
                    planning_agents.status, repositories.root_path,
                    terminal_application.value, terminal_arguments.value
             FROM planning_agents
             JOIN planning_runs ON planning_runs.id = planning_agents.planning_run_id
             JOIN work_items ON work_items.id = planning_runs.work_item_id
             JOIN repositories ON repositories.id = work_items.repository_id
             JOIN app_settings AS terminal_application
               ON terminal_application.key = 'terminal_application'
             JOIN app_settings AS terminal_arguments
               ON terminal_arguments.key = 'terminal_arguments'
             WHERE planning_agents.id = ?1
               AND work_items.id = ?2
               AND work_items.lifecycle_status = 'open'
               AND repositories.archived_at IS NULL",
            params![planning_agent_id, work_item_id],
            |row| {
                Ok(HandoffTarget {
                    planning_run_id: row.get(0)?,
                    session_name: row.get(1)?,
                    agent_status: row.get(2)?,
                    repository_path: row.get(3)?,
                    terminal_application: row.get(4)?,
                    terminal_arguments: row.get(5)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            AppError::not_found(
                "The planning agent does not belong to the requested active work item.",
            )
            .into()
        })
}

fn load_handoff_by_key(
    connection: &Connection,
    idempotency_key: &str,
) -> Result<Option<HandoffRecord>, StoreError> {
    connection
        .query_row(
            "SELECT id, work_item_id, planning_run_id, planning_agent_id, session_name,
                    status, completion_observable, error_code, error_message,
                    created_at, updated_at, completed_at
             FROM terminal_handoffs WHERE idempotency_key = ?1",
            [idempotency_key],
            handoff_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn load_handoff(
    connection: &Connection,
    handoff_id: &str,
) -> Result<Option<HandoffRecord>, StoreError> {
    connection
        .query_row(
            "SELECT id, work_item_id, planning_run_id, planning_agent_id, session_name,
                    status, completion_observable, error_code, error_message,
                    created_at, updated_at, completed_at
             FROM terminal_handoffs WHERE id = ?1",
            [handoff_id],
            handoff_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn handoff_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HandoffRecord> {
    Ok(HandoffRecord {
        id: row.get(0)?,
        work_item_id: row.get(1)?,
        planning_run_id: row.get(2)?,
        planning_agent_id: row.get(3)?,
        session_name: row.get(4)?,
        status: row.get(5)?,
        completion_observable: row.get(6)?,
        error_code: row.get(7)?,
        error_message: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        completed_at: row.get(11)?,
    })
}

fn launcher_start_error(error: &io::Error) -> AppError {
    AppError::external(format!(
        "macOS could not start `/usr/bin/open`: {error}. Open the persisted Copilot session manually, then reconcile this handoff."
    ))
}

fn launcher_failure(output: &ProcessOutput) -> AppError {
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    AppError::external(if detail.is_empty() {
        format!(
            "The terminal launcher exited with status {}. Open the persisted Copilot session manually, then reconcile this handoff.",
            output.status
        )
    } else {
        format!(
            "The terminal launcher failed: {detail}. Open the persisted Copilot session manually, then reconcile this handoff."
        )
    })
}

fn required(value: &str, message: &str) -> Result<String, AppError> {
    let value = value.trim();
    if value.is_empty() {
        Err(AppError::validation(message))
    } else {
        Ok(value.to_owned())
    }
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::sync::Mutex;

    use serde_json::json;
    use tempfile::{tempdir, TempDir};

    use super::{
        LaunchTerminalHandoffRequest, OpenCopilotSessionRequest, ResumeTerminalHandoffRequest,
        TerminalHandoffService, TerminalLaunchOutcome, TerminalLauncher, TerminalOpener,
    };
    use crate::copilot::{
        AgentEnvelope, AgentOutcome, AgentSession, CompletedPlannerArtifact, CopilotEvent,
        CopilotRunOutput, NormalizedRequirements, ProcessOutput, ProcessRunner,
    };
    use crate::error::AppError;
    use crate::planning::PlanningExecutor;
    use crate::settings::{DEFAULT_TERMINAL_APPLICATION, DEFAULT_TERMINAL_ARGUMENTS};
    use crate::state::AppStore;

    struct CapturingRunner {
        call: Mutex<Option<(String, Vec<String>)>>,
    }

    impl ProcessRunner for &CapturingRunner {
        fn run(&self, program: &str, arguments: &[String]) -> std::io::Result<ProcessOutput> {
            *self.call.lock().expect("call") = Some((program.to_owned(), arguments.to_vec()));
            Ok(ProcessOutput {
                success: true,
                status: "0".to_owned(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
    }

    #[test]
    fn launches_planning_only_session_with_direct_argv_and_preserves_spaces_and_quotes() {
        let runner = CapturingRunner {
            call: Mutex::new(None),
        };
        let outcome = TerminalOpener::open(
            &TerminalLauncher::new(&runner),
            "Ghostty Preview.app",
            "-W -na \"{terminalApplication}\" --args -e copilot -C \"{repositoryPath}\" \"--resume={sessionName}\"",
            "/Users/example/repository with spaces",
            "quorum exact session",
        )
        .expect("launch");
        assert!(outcome.completion_observed);
        assert_eq!(
            runner.call.lock().expect("call").clone(),
            Some((
                "/usr/bin/open".to_owned(),
                vec![
                    "-W".to_owned(),
                    "-na".to_owned(),
                    "Ghostty Preview.app".to_owned(),
                    "--args".to_owned(),
                    "-e".to_owned(),
                    "copilot".to_owned(),
                    "-C".to_owned(),
                    "/Users/example/repository with spaces".to_owned(),
                    "--resume=quorum exact session".to_owned(),
                    "--plan".to_owned(),
                    "--no-custom-instructions".to_owned(),
                    "--disable-builtin-mcps".to_owned(),
                    "--disallow-temp-dir".to_owned(),
                    "--allow-all-tools".to_owned(),
                    "--allow-all-paths".to_owned(),
                    "--deny-tool=write".to_owned(),
                    "--deny-tool=shell".to_owned(),
                    "--no-remote-export".to_owned(),
                ]
            ))
        );
    }

    #[test]
    fn only_treats_open_wait_flags_before_app_arguments_as_observable() {
        let runner = CapturingRunner {
            call: Mutex::new(None),
        };
        let outcome = TerminalOpener::open(
            &TerminalLauncher::new(&runner),
            "Ghostty.app",
            "-na {terminalApplication} --args -e copilot -W -C {repositoryPath} --resume={sessionName}",
            "/Users/example/repository",
            "quorum-session",
        )
        .expect("launch");
        assert!(!outcome.completion_observed);
    }

    #[test]
    fn opens_an_existing_session_without_waiting_for_terminal_completion() {
        let runner = CapturingRunner {
            call: Mutex::new(None),
        };
        TerminalOpener::open_session(
            &TerminalLauncher::new(&runner),
            "Ghostty.app",
            "-W -na {terminalApplication} --args -e copilot -C {repositoryPath} --resume={sessionName}",
            "/Users/example/repository",
            "quorum-session",
        )
        .expect("open session");
        let (_, arguments) = runner.call.lock().expect("call").clone().expect("captured");
        assert!(!arguments.iter().any(|argument| argument == "-W"));
        assert!(arguments
            .iter()
            .any(|argument| argument == "--resume=quorum-session"));
    }

    struct FakeOpener {
        outcomes: Mutex<VecDeque<Result<TerminalLaunchOutcome, AppError>>>,
        calls: Mutex<Vec<(String, String, String, String)>>,
    }

    impl FakeOpener {
        fn new(outcomes: Vec<Result<TerminalLaunchOutcome, AppError>>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl TerminalOpener for FakeOpener {
        fn open(
            &self,
            terminal_application: &str,
            terminal_arguments: &str,
            repository_path: &str,
            session_name: &str,
        ) -> Result<TerminalLaunchOutcome, AppError> {
            self.calls.lock().expect("calls").push((
                terminal_application.to_owned(),
                terminal_arguments.to_owned(),
                repository_path.to_owned(),
                session_name.to_owned(),
            ));
            self.outcomes
                .lock()
                .expect("outcomes")
                .pop_front()
                .expect("configured opener outcome")
        }
    }

    struct FakePlanningExecutor {
        outputs: Mutex<VecDeque<Result<CopilotRunOutput, AppError>>>,
        resumes: Mutex<Vec<(String, String, String)>>,
    }

    impl FakePlanningExecutor {
        fn new(outputs: Vec<Result<CopilotRunOutput, AppError>>) -> Self {
            Self {
                outputs: Mutex::new(outputs.into()),
                resumes: Mutex::new(Vec::new()),
            }
        }
    }

    impl PlanningExecutor for FakePlanningExecutor {
        fn start_planner(
            &self,
            _repository_path: &str,
            _model: &str,
            _session: &AgentSession,
            _requirements: &NormalizedRequirements,
        ) -> Result<CopilotRunOutput, AppError> {
            Err(AppError::external("unexpected planner start"))
        }

        fn start_synthesizer(
            &self,
            _repository_path: &str,
            _model: &str,
            _session: &AgentSession,
            _requirements: &NormalizedRequirements,
            _artifacts: &[CompletedPlannerArtifact],
        ) -> Result<CopilotRunOutput, AppError> {
            Err(AppError::external("unexpected synthesizer start"))
        }

        fn resume_named(
            &self,
            repository_path: &str,
            session_name: &str,
            prompt: &str,
        ) -> Result<CopilotRunOutput, AppError> {
            self.resumes.lock().expect("resumes").push((
                repository_path.to_owned(),
                session_name.to_owned(),
                prompt.to_owned(),
            ));
            self.outputs
                .lock()
                .expect("outputs")
                .pop_front()
                .expect("configured reconcile output")
        }
    }

    struct Harness {
        _directory: TempDir,
        store: AppStore,
        repository_path: String,
    }

    const WORK_ITEM_ID: &str = "work-item";
    const RUN_ID: &str = "planning-run";
    const AGENT_ID: &str = "00000000-0000-4000-8000-000000000002";
    const SESSION_NAME: &str = "quorum-persisted-synthesizer-session";

    fn harness() -> Harness {
        let directory = tempdir().expect("temp directory");
        let repository = directory.path().join("repository with spaces");
        let data = directory.path().join("data");
        fs::create_dir_all(&repository).expect("repository");
        let store = AppStore::open(data).expect("store");
        let repository_path = repository.to_string_lossy().into_owned();
        store
            .with_connection(|connection| {
                connection.execute_batch(&format!(
                    "INSERT INTO repositories (
                       id, root_path, display_name, created_at, updated_at
                     ) VALUES (
                       'repository', '{}', 'repository', 'now', 'now'
                     );
                     INSERT INTO work_items (
                       id, repository_id, title, source_kind, source_metadata_json,
                       markdown_body, lifecycle_status, created_at, updated_at
                     ) VALUES (
                       '{WORK_ITEM_ID}', 'repository', 'Feature', 'inline_markdown',
                       '{{\"kind\":\"inline_markdown\"}}', '# Requirements',
                       'open', 'now', 'now'
                     );
                     INSERT INTO planning_runs (
                       id, work_item_id, status, created_at, updated_at
                     ) VALUES (
                       '{RUN_ID}', '{WORK_ITEM_ID}', 'waiting_for_answers', 'now', 'now'
                     );
                     INSERT INTO planning_agents (
                       id, planning_run_id, role, ordinal, model_id, session_name,
                       status, attempt, started_at, created_at, updated_at, completed_at
                     ) VALUES (
                       '00000000-0000-4000-8000-000000000001', '{RUN_ID}', 'planner', 0,
                       'planner-model', 'planner-session', 'succeeded', 1, 'now',
                       'now', 'now', 'now'
                     );
                     INSERT INTO planning_agents (
                       id, planning_run_id, role, ordinal, model_id, session_name,
                       status, attempt, started_at, created_at, updated_at
                     ) VALUES (
                       '{AGENT_ID}', '{RUN_ID}', 'synthesizer', 0,
                       'synthesizer-model', '{SESSION_NAME}', 'blocked', 1, 'now',
                       'now', 'now'
                     );
                     INSERT INTO planning_artifacts (
                       id, planning_run_id, planning_agent_id, artifact_kind,
                       markdown_body, attempt, sequence, created_at
                     ) VALUES (
                       'planner-artifact', '{RUN_ID}',
                       '00000000-0000-4000-8000-000000000001', 'planner_output',
                       '# Candidate', 1, 0, 'now'
                     );
                     INSERT INTO planning_questions (
                       id, planning_run_id, planning_agent_id, external_id, ordinal,
                       prompt_markdown, status, created_at, updated_at
                     ) VALUES (
                       'question', '{RUN_ID}', '{AGENT_ID}', 'choice', 0,
                       'Which approach?', 'open', 'now', 'now'
                     );",
                    repository_path.replace('\'', "''")
                ))?;
                Ok(())
            })
            .expect("seed");
        Harness {
            _directory: directory,
            store,
            repository_path,
        }
    }

    fn completed(markdown: &str) -> CopilotRunOutput {
        CopilotRunOutput {
            envelope: AgentEnvelope {
                version: 1,
                outcome: AgentOutcome::Completed,
                questions: Vec::new(),
                markdown: Some(markdown.to_owned()),
                error: None,
            },
            events: vec![CopilotEvent {
                sequence: 0,
                kind: Some("assistant.message".to_owned()),
                payload: json!({"marker": "terminal-reconcile"}),
            }],
        }
    }

    fn launch_request(key: &str) -> LaunchTerminalHandoffRequest {
        LaunchTerminalHandoffRequest {
            work_item_id: WORK_ITEM_ID.to_owned(),
            planning_agent_id: AGENT_ID.to_owned(),
            idempotency_key: key.to_owned(),
        }
    }

    #[test]
    fn opens_an_owned_completed_agent_without_creating_a_recovery_handoff() {
        let harness = harness();
        let opener = FakeOpener::new(vec![Ok(TerminalLaunchOutcome {
            completion_observed: false,
        })]);
        let executor = FakePlanningExecutor::new(Vec::new());
        let service = TerminalHandoffService::with_dependencies(&harness.store, &opener, &executor);

        service
            .open_session(&OpenCopilotSessionRequest {
                work_item_id: WORK_ITEM_ID.to_owned(),
                planning_agent_id: "00000000-0000-4000-8000-000000000001".to_owned(),
            })
            .expect("open completed planner");

        assert_eq!(opener.calls.lock().expect("calls").len(), 1);
        let handoffs: i64 = harness
            .store
            .with_connection(|connection| {
                connection
                    .query_row("SELECT count(*) FROM terminal_handoffs", [], |row| {
                        row.get(0)
                    })
                    .map_err(Into::into)
            })
            .expect("handoff count");
        assert_eq!(handoffs, 0);
    }

    #[test]
    fn enforces_ownership_then_monitors_and_reconciles_the_exact_persisted_session() {
        let harness = harness();
        let opener = FakeOpener::new(vec![Ok(TerminalLaunchOutcome {
            completion_observed: true,
        })]);
        let executor = FakePlanningExecutor::new(vec![Ok(completed("# Final plan"))]);
        let service = TerminalHandoffService::with_dependencies(&harness.store, &opener, &executor);

        let mut wrong_owner = launch_request("wrong-owner");
        wrong_owner.work_item_id = "another-work-item".to_owned();
        assert_eq!(
            service.launch(&wrong_owner).expect_err("ownership").code,
            "not_found"
        );
        assert!(opener.calls.lock().expect("calls").is_empty());

        let request = launch_request("monitored");
        let result = service.launch(&request).expect("handoff");
        assert_eq!(result.status, "reconciled");
        assert_eq!(result.completion_observable, Some(true));
        assert!(!result.manual_reconcile_available);
        assert_eq!(
            opener.calls.lock().expect("calls").as_slice(),
            [(
                DEFAULT_TERMINAL_APPLICATION.to_owned(),
                DEFAULT_TERMINAL_ARGUMENTS.to_owned(),
                harness.repository_path.clone(),
                SESSION_NAME.to_owned()
            )]
        );
        let resumes = executor.resumes.lock().expect("resumes");
        assert_eq!(resumes.len(), 1);
        assert_eq!(resumes[0].0, harness.repository_path);
        assert_eq!(resumes[0].1, SESSION_NAME);
        assert!(resumes[0].2.contains("exact persisted planning session"));
        drop(resumes);
        let state = result.planning_state.expect("planning state");
        assert_eq!(state.run.status, "succeeded");
        assert_eq!(
            state
                .agents
                .iter()
                .find(|agent| agent.id == AGENT_ID)
                .expect("agent")
                .session_name,
            SESSION_NAME
        );
        assert_eq!(state.questions[0].status, "dismissed");

        let repeated = service.launch(&request).expect_err("duplicate handoff");
        assert_eq!(repeated.code, "conflict");
        assert_eq!(opener.calls.lock().expect("calls").len(), 1);
        assert_eq!(executor.resumes.lock().expect("resumes").len(), 1);
    }

    #[test]
    fn unobservable_launch_waits_for_explicit_manual_reconciliation() {
        let harness = harness();
        let opener = FakeOpener::new(vec![Ok(TerminalLaunchOutcome {
            completion_observed: false,
        })]);
        let executor = FakePlanningExecutor::new(vec![Ok(completed("# Manual plan"))]);
        let service = TerminalHandoffService::with_dependencies(&harness.store, &opener, &executor);

        let launched = service
            .launch(&launch_request("unobservable"))
            .expect("launch");
        assert_eq!(launched.status, "awaiting_manual_reconcile");
        assert_eq!(launched.completion_observable, Some(false));
        assert!(launched.manual_reconcile_available);
        assert!(launched.planning_state.is_none());
        assert!(executor.resumes.lock().expect("resumes").is_empty());

        let reconciled = service
            .resume_and_reconcile(&ResumeTerminalHandoffRequest {
                work_item_id: WORK_ITEM_ID.to_owned(),
                terminal_handoff_id: launched.id,
            })
            .expect("manual reconcile");
        assert_eq!(reconciled.status, "reconciled");
        assert_eq!(executor.resumes.lock().expect("resumes").len(), 1);
    }

    #[test]
    fn launcher_failure_preserves_planning_state_and_allows_manual_fallback() {
        let harness = harness();
        let opener = FakeOpener::new(vec![Err(AppError::external(
            "configured terminal unavailable",
        ))]);
        let executor = FakePlanningExecutor::new(vec![Ok(completed("# Recovered plan"))]);
        let service = TerminalHandoffService::with_dependencies(&harness.store, &opener, &executor);

        let failed = service
            .launch(&launch_request("failure"))
            .expect("recorded failure");
        assert_eq!(failed.status, "launch_failed");
        assert_eq!(failed.error_code.as_deref(), Some("external"));
        assert!(failed.manual_reconcile_available);
        harness
            .store
            .with_connection(|connection| {
                let status: String = connection.query_row(
                    "SELECT status FROM planning_agents WHERE id = ?1",
                    [AGENT_ID],
                    |row| row.get(0),
                )?;
                assert_eq!(status, "blocked");
                Ok(())
            })
            .expect("preserved state");

        let recovered = service
            .resume_and_reconcile(&ResumeTerminalHandoffRequest {
                work_item_id: WORK_ITEM_ID.to_owned(),
                terminal_handoff_id: failed.id,
            })
            .expect("fallback");
        assert_eq!(recovered.status, "reconciled");
        assert_eq!(executor.resumes.lock().expect("resumes")[0].1, SESSION_NAME);
    }

    #[test]
    fn reconcile_failure_is_persisted_and_duplicate_manual_retry_conflicts() {
        let harness = harness();
        let opener = FakeOpener::new(vec![Ok(TerminalLaunchOutcome {
            completion_observed: true,
        })]);
        let executor = FakePlanningExecutor::new(vec![
            Err(AppError::external(
                "Copilot session temporarily unavailable",
            )),
            Ok(completed("# Retried plan")),
        ]);
        let service = TerminalHandoffService::with_dependencies(&harness.store, &opener, &executor);

        let failed = service
            .launch(&launch_request("reconcile-failure"))
            .expect("persisted reconcile failure");
        assert_eq!(failed.status, "reconcile_failed");
        assert!(failed.manual_reconcile_available);
        assert_eq!(
            failed
                .planning_state
                .as_ref()
                .expect("failed planning state")
                .agents
                .iter()
                .find(|agent| agent.id == AGENT_ID)
                .expect("agent")
                .status,
            "failed"
        );

        let request = ResumeTerminalHandoffRequest {
            work_item_id: WORK_ITEM_ID.to_owned(),
            terminal_handoff_id: failed.id,
        };
        let recovered = service
            .resume_and_reconcile(&request)
            .expect("manual retry");
        assert_eq!(recovered.status, "reconciled");
        let repeated = service
            .resume_and_reconcile(&request)
            .expect_err("duplicate manual retry");
        assert_eq!(repeated.code, "conflict");
        assert_eq!(executor.resumes.lock().expect("resumes").len(), 2);
    }
}
