use std::collections::{HashMap, HashSet};
use std::thread;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ts_rs::TS;
use uuid::Uuid;

use crate::copilot::{
    AgentOutcome, AgentRole, AgentSession, CompletedPlannerArtifact, CopilotClient,
    CopilotRunOutput, NormalizedRequirements, SystemProcessRunner,
};
use crate::error::{AppError, StoreError};
use crate::state::AppStore;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct StartPlanningRequest {
    pub work_item_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ReplanWorkItemRequest {
    pub planning_run_id: String,
    pub expected_plan_updated_at: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanningAnswerInput {
    pub question_id: String,
    pub answer_markdown: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SubmitPlanningAnswersRequest {
    pub planning_run_id: String,
    pub planning_agent_id: String,
    pub expected_run_updated_at: String,
    pub answers: Vec<PlanningAnswerInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RetryPlanningRequest {
    pub planning_run_id: String,
    pub planning_agent_id: Option<String>,
    pub expected_run_updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ReconcilePlanningAgentRequest {
    pub work_item_id: String,
    pub planning_agent_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct UpdateSynthesizedPlanRequest {
    pub planning_run_id: String,
    pub expected_plan_updated_at: String,
    pub markdown_body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanApprovalRequest {
    pub planning_run_id: String,
    pub expected_plan_updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct EnqueuePlanRequest {
    pub planning_run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanningRunDto {
    pub id: String,
    pub work_item_id: String,
    pub status: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub idempotency_key: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanningAgentDto {
    pub id: String,
    pub role: String,
    pub ordinal: usize,
    pub model_id: String,
    pub session_name: String,
    pub status: String,
    pub attempt: usize,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanningEventDto {
    pub id: String,
    pub planning_agent_id: String,
    pub attempt: usize,
    pub sequence: usize,
    pub event_kind: Option<String>,
    #[ts(type = "unknown")]
    pub payload: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanningArtifactDto {
    pub id: String,
    pub planning_agent_id: Option<String>,
    pub artifact_kind: String,
    pub markdown_body: String,
    pub attempt: usize,
    pub sequence: usize,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanningQuestionDto {
    pub id: String,
    pub planning_agent_id: String,
    pub external_id: String,
    pub ordinal: usize,
    pub prompt_markdown: String,
    pub status: String,
    pub answer_markdown: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanRevisionDto {
    pub id: String,
    pub revision: usize,
    pub edit_revision: usize,
    pub markdown_body: String,
    pub approval_policy: String,
    pub approval_status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanningStateDto {
    pub run: PlanningRunDto,
    pub agents: Vec<PlanningAgentDto>,
    pub events: Vec<PlanningEventDto>,
    pub artifacts: Vec<PlanningArtifactDto>,
    pub questions: Vec<PlanningQuestionDto>,
    pub plan: Option<PlanRevisionDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanningSourceDto {
    pub work_item_id: String,
    pub repository_id: String,
    pub title: String,
    pub kind: String,
    pub reference: Option<String>,
    pub markdown_body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct QueueEntryDto {
    pub id: String,
    pub position: usize,
    pub scheduling_status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanningQueueDto {
    pub state: String,
    pub eligible: bool,
    pub reason: Option<String>,
    pub entry: Option<QueueEntryDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct TerminalHandoffSummaryDto {
    pub id: String,
    pub planning_agent_id: String,
    pub session_name: String,
    pub status: String,
    pub manual_reconcile_available: bool,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanningDetailDto {
    pub source: PlanningSourceDto,
    pub current_phase: String,
    pub status: String,
    pub run: PlanningRunDto,
    pub agents: Vec<PlanningAgentDto>,
    pub pending_questions: Vec<PlanningQuestionDto>,
    pub answered_questions: Vec<PlanningQuestionDto>,
    pub plan: Option<PlanRevisionDto>,
    pub queue: PlanningQueueDto,
    pub recent_events: Vec<PlanningEventDto>,
    pub terminal_handoff: Option<TerminalHandoffSummaryDto>,
}

pub trait PlanningExecutor: Send + Sync {
    fn start_planner(
        &self,
        repository_path: &str,
        model: &str,
        session: &AgentSession,
        requirements: &NormalizedRequirements,
    ) -> Result<CopilotRunOutput, AppError>;

    fn start_synthesizer(
        &self,
        repository_path: &str,
        model: &str,
        session: &AgentSession,
        requirements: &NormalizedRequirements,
        artifacts: &[CompletedPlannerArtifact],
    ) -> Result<CopilotRunOutput, AppError>;

    fn resume_named(
        &self,
        repository_path: &str,
        session_name: &str,
        prompt: &str,
    ) -> Result<CopilotRunOutput, AppError>;
}

pub struct SystemPlanningExecutor {
    client: CopilotClient<SystemProcessRunner>,
}

impl Default for SystemPlanningExecutor {
    fn default() -> Self {
        Self {
            client: CopilotClient::new(SystemProcessRunner),
        }
    }
}

impl PlanningExecutor for SystemPlanningExecutor {
    fn start_planner(
        &self,
        repository_path: &str,
        model: &str,
        session: &AgentSession,
        requirements: &NormalizedRequirements,
    ) -> Result<CopilotRunOutput, AppError> {
        self.client
            .start_planner(repository_path, model, session, requirements)
    }

    fn start_synthesizer(
        &self,
        repository_path: &str,
        model: &str,
        session: &AgentSession,
        requirements: &NormalizedRequirements,
        artifacts: &[CompletedPlannerArtifact],
    ) -> Result<CopilotRunOutput, AppError> {
        self.client
            .start_synthesizer(repository_path, model, session, requirements, artifacts)
    }

    fn resume_named(
        &self,
        repository_path: &str,
        session_name: &str,
        prompt: &str,
    ) -> Result<CopilotRunOutput, AppError> {
        self.client
            .resume_named(repository_path, session_name, prompt)
    }
}

pub struct PlanningService<'a, E: PlanningExecutor> {
    store: &'a AppStore,
    executor: &'a E,
}

impl<'a, E: PlanningExecutor> PlanningService<'a, E> {
    pub const fn with_executor(store: &'a AppStore, executor: &'a E) -> Self {
        Self { store, executor }
    }

    pub fn start(&self, request: &StartPlanningRequest) -> Result<PlanningStateDto, AppError> {
        let idempotency_key = required(
            &request.idempotency_key,
            "A planning idempotency key is required.",
        )?;
        let creation = self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let existing: Option<String> = transaction
                .query_row(
                    "SELECT id FROM planning_runs WHERE work_item_id = ?1",
                    [&request.work_item_id],
                    |row| row.get(0),
                )
                .optional()?;
            if existing.is_some() {
                return Err(StoreError::App(AppError::conflict(
                    "Planning has already been started for this work item.",
                )));
            }

            let work_item = transaction
                .query_row(
                    "SELECT work_items.id, work_items.title, work_items.markdown_body,
                            repositories.root_path
                     FROM work_items
                     JOIN repositories ON repositories.id = work_items.repository_id
                     WHERE work_items.id = ?1 AND work_items.lifecycle_status = 'open'
                       AND repositories.archived_at IS NULL",
                    [&request.work_item_id],
                    |row| {
                        Ok(WorkItem {
                            id: row.get(0)?,
                            title: row.get(1)?,
                            markdown: row.get(2)?,
                            repository_path: row.get(3)?,
                        })
                    },
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::App(AppError::not_found(
                        "The open work item could not be found.",
                    ))
                })?;
            let models = planner_models(&transaction)?;
            let run_id = Uuid::new_v4().to_string();
            let timestamp = now();
            transaction.execute(
                "INSERT INTO planning_runs (
                    id, work_item_id, status, idempotency_key, created_at, updated_at
                 ) VALUES (?1, ?2, 'running', ?3, ?4, ?4)",
                params![run_id, work_item.id, idempotency_key, timestamp],
            )?;
            let mut planners = Vec::with_capacity(models.len());
            for (ordinal, model) in models.iter().enumerate() {
                let session = AgentSession::planner(&work_item.id, &run_id, ordinal);
                insert_agent(
                    &transaction,
                    &run_id,
                    &session,
                    model,
                    "running",
                    1,
                    &timestamp,
                )?;
                planners.push(Invocation {
                    agent_id: session.id.to_string(),
                    model: model.clone(),
                    session,
                    repository_path: work_item.repository_path.clone(),
                    requirements: NormalizedRequirements::new(
                        &work_item.title,
                        &work_item.markdown,
                    )
                    .map_err(StoreError::App)?,
                    attempt: 1,
                });
            }
            let synthesizer = AgentSession::synthesizer(&work_item.id, &run_id);
            insert_agent(
                &transaction,
                &run_id,
                &synthesizer,
                &models[0],
                "pending",
                0,
                &timestamp,
            )?;
            transaction.commit()?;
            Ok(Creation::Created { run_id, planners })
        })?;

        match creation {
            Creation::Created { run_id, planners } => {
                self.execute_initial_planners(&run_id, planners)?;
                self.advance(&run_id)
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn replan(&self, request: &ReplanWorkItemRequest) -> Result<PlanningStateDto, AppError> {
        let idempotency_key = required(
            &request.idempotency_key,
            "A re-planning idempotency key is required.",
        )?;
        let creation = self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let plan = load_plan_mutation(
                &transaction,
                &request.planning_run_id,
                &request.expected_plan_updated_at,
            )?;
            if plan.approval_status == "approved" {
                return Err(StoreError::App(AppError::conflict(
                    "An approved plan cannot be replaced by re-planning.",
                )));
            }
            let has_queue_entry: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE plan_id = ?1)",
                [&plan.id],
                |row| row.get(0),
            )?;
            if has_queue_entry {
                return Err(StoreError::App(AppError::conflict(
                    "A queued plan cannot be replaced by re-planning.",
                )));
            }
            let work_item = transaction
                .query_row(
                    "SELECT work_items.id, work_items.title, work_items.markdown_body,
                            repositories.root_path
                     FROM planning_runs
                     JOIN work_items ON work_items.id = planning_runs.work_item_id
                     JOIN repositories ON repositories.id = work_items.repository_id
                     WHERE planning_runs.id = ?1
                       AND work_items.lifecycle_status = 'open'
                       AND repositories.archived_at IS NULL",
                    [&request.planning_run_id],
                    |row| {
                        Ok(WorkItem {
                            id: row.get(0)?,
                            title: row.get(1)?,
                            markdown: row.get(2)?,
                            repository_path: row.get(3)?,
                        })
                    },
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::App(AppError::not_found(
                        "The open work item could not be found.",
                    ))
                })?;
            let models = planner_models(&transaction)?;
            transaction.execute(
                "DELETE FROM planning_answers
                 WHERE question_id IN (
                   SELECT id FROM planning_questions WHERE planning_run_id = ?1
                 )",
                [&request.planning_run_id],
            )?;
            for table in [
                "terminal_handoffs",
                "planning_agent_events",
                "planning_artifacts",
                "planning_questions",
                "planning_agents",
            ] {
                transaction.execute(
                    &format!("DELETE FROM {table} WHERE planning_run_id = ?1"),
                    [&request.planning_run_id],
                )?;
            }
            transaction.execute(
                "DELETE FROM plans WHERE planning_run_id = ?1",
                [&request.planning_run_id],
            )?;
            transaction.execute(
                "DELETE FROM planning_runs WHERE id = ?1",
                [&request.planning_run_id],
            )?;

            let run_id = Uuid::new_v4().to_string();
            let timestamp = now();
            transaction.execute(
                "INSERT INTO planning_runs (
                    id, work_item_id, status, idempotency_key, created_at, updated_at
                 ) VALUES (?1, ?2, 'running', ?3, ?4, ?4)",
                params![run_id, work_item.id, idempotency_key, timestamp],
            )?;
            let mut planners = Vec::with_capacity(models.len());
            for (ordinal, model) in models.iter().enumerate() {
                let session = AgentSession::planner(&work_item.id, &run_id, ordinal);
                insert_agent(
                    &transaction,
                    &run_id,
                    &session,
                    model,
                    "running",
                    1,
                    &timestamp,
                )?;
                planners.push(Invocation {
                    agent_id: session.id.to_string(),
                    model: model.clone(),
                    session,
                    repository_path: work_item.repository_path.clone(),
                    requirements: NormalizedRequirements::new(
                        &work_item.title,
                        &work_item.markdown,
                    )
                    .map_err(StoreError::App)?,
                    attempt: 1,
                });
            }
            let synthesizer = AgentSession::synthesizer(&work_item.id, &run_id);
            insert_agent(
                &transaction,
                &run_id,
                &synthesizer,
                &models[0],
                "pending",
                0,
                &timestamp,
            )?;
            transaction.commit()?;
            Ok(Creation::Created { run_id, planners })
        })?;

        match creation {
            Creation::Created { run_id, planners } => {
                self.execute_initial_planners(&run_id, planners)?;
                self.advance(&run_id)
            }
        }
    }

    pub fn get(&self, planning_run_id: &str) -> Result<PlanningStateDto, AppError> {
        self.advance(planning_run_id)
    }

    pub(crate) fn current_state(
        &self,
        planning_run_id: &str,
    ) -> Result<PlanningStateDto, AppError> {
        self.state(planning_run_id)
    }

    pub fn latest_for_work_item(&self, work_item_id: &str) -> Result<PlanningStateDto, AppError> {
        let work_item_id = required(work_item_id, "A work item ID is required.")?;
        let run_id = self.store.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT planning_runs.id
                     FROM planning_runs
                     JOIN work_items ON work_items.id = planning_runs.work_item_id
                     JOIN repositories ON repositories.id = work_items.repository_id
                     WHERE work_items.id = ?1
                       AND work_items.lifecycle_status = 'open'
                       AND repositories.archived_at IS NULL
                     ORDER BY planning_runs.created_at DESC LIMIT 1",
                    [&work_item_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::App(AppError::not_found(
                        "Planning has not been started for this work item.",
                    ))
                })
        })?;
        self.get(&run_id)
    }

    pub fn detail(&self, planning_run_id: &str) -> Result<PlanningDetailDto, AppError> {
        let state = self.current_state(planning_run_id)?;
        self.store
            .with_connection(|connection| load_detail(connection, state))
    }

    pub fn update_plan(
        &self,
        request: &UpdateSynthesizedPlanRequest,
    ) -> Result<PlanningStateDto, AppError> {
        let markdown = required(
            &request.markdown_body,
            "The synthesized plan cannot be empty.",
        )?;
        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let plan = load_plan_mutation(
                &transaction,
                &request.planning_run_id,
                &request.expected_plan_updated_at,
            )?;
            ensure_plan_not_queued(&transaction, &plan.id)?;
            if plan.markdown_body == markdown {
                return Err(StoreError::App(AppError::conflict(
                    "The synthesized plan already contains this content.",
                )));
            }
            let timestamp = now();
            let approval_status = if plan.approval_policy == "required" {
                "pending"
            } else {
                "draft"
            };
            let eligibility_key =
                (plan.approval_policy != "required").then(|| Uuid::new_v4().to_string());
            let eligible_at = (plan.approval_policy != "required").then_some(timestamp.as_str());
            transaction.execute(
                "UPDATE plans
                 SET markdown_body = ?2, edit_revision = edit_revision + 1,
                     approval_status = ?3, approved_at = NULL,
                     queue_eligibility_key = ?4, queue_eligible_at = ?5,
                     updated_at = ?6
                 WHERE id = ?1",
                params![
                    plan.id,
                    markdown,
                    approval_status,
                    eligibility_key,
                    eligible_at,
                    timestamp
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })?;
        self.current_state(&request.planning_run_id)
    }

    pub fn approve_plan(
        &self,
        request: &PlanApprovalRequest,
    ) -> Result<PlanningStateDto, AppError> {
        self.set_plan_approval(request, "approved")
    }

    pub fn reject_plan(&self, request: &PlanApprovalRequest) -> Result<PlanningStateDto, AppError> {
        self.set_plan_approval(request, "rejected")
    }

    pub fn enqueue_plan(&self, request: &EnqueuePlanRequest) -> Result<PlanningStateDto, AppError> {
        let planning_run_id = required(&request.planning_run_id, "A planning run ID is required.")?;
        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let plan = transaction
                .query_row(
                    "SELECT plans.id, plans.work_item_id, plans.queue_eligibility_key,
                            plans.approval_policy, plans.approval_status
                     FROM plans
                     JOIN planning_runs ON planning_runs.id = plans.planning_run_id
                     WHERE plans.planning_run_id = ?1
                       AND planning_runs.status = 'succeeded'",
                    [&planning_run_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::App(AppError::conflict(
                        "A completed synthesized plan is required before enqueueing.",
                    ))
                })?;
            if transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE plan_id = ?1)",
                [&plan.0],
                |row| row.get::<_, bool>(0),
            )? {
                transaction.commit()?;
                return Ok(());
            }
            let eligibility_key = plan.2.ok_or_else(|| {
                StoreError::App(AppError::conflict(
                    "The synthesized plan is not eligible for the queue.",
                ))
            })?;
            if plan.3 == "required" && plan.4 != "approved" {
                return Err(StoreError::App(AppError::conflict(
                    "Approve the synthesized plan before enqueueing it.",
                )));
            }
            let position: usize = transaction.query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM queue_entries",
                [],
                |row| row.get(0),
            )?;
            let timestamp = now();
            transaction.execute(
                "INSERT INTO queue_entries (
                    id, work_item_id, position, scheduling_status, created_at,
                    updated_at, plan_id, idempotency_key
                 ) VALUES (?1, ?2, ?3, 'queued', ?4, ?4, ?5, ?6)",
                params![
                    Uuid::new_v4().to_string(),
                    plan.1,
                    position,
                    timestamp,
                    plan.0,
                    eligibility_key
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })?;
        self.current_state(&planning_run_id)
    }

    fn set_plan_approval(
        &self,
        request: &PlanApprovalRequest,
        decision: &str,
    ) -> Result<PlanningStateDto, AppError> {
        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let plan = load_plan_mutation(
                &transaction,
                &request.planning_run_id,
                &request.expected_plan_updated_at,
            )?;
            ensure_plan_not_queued(&transaction, &plan.id)?;
            if plan.approval_policy != "required" {
                return Err(StoreError::App(AppError::conflict(
                    "This synthesized plan does not require approval.",
                )));
            }
            if plan.approval_status == decision {
                return Err(StoreError::App(AppError::conflict(format!(
                    "The synthesized plan is already {decision}."
                ))));
            }
            let timestamp = now();
            let eligibility_key = (decision == "approved").then(|| Uuid::new_v4().to_string());
            transaction.execute(
                "UPDATE plans
                 SET approval_status = ?2, approved_at = ?3,
                     queue_eligibility_key = ?4, queue_eligible_at = ?3,
                     updated_at = ?5
                 WHERE id = ?1",
                params![
                    plan.id,
                    decision,
                    (decision == "approved").then_some(timestamp.as_str()),
                    eligibility_key,
                    timestamp
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })?;
        self.current_state(&request.planning_run_id)
    }

    #[allow(clippy::too_many_lines)]
    pub fn submit_answers(
        &self,
        request: &SubmitPlanningAnswersRequest,
    ) -> Result<PlanningStateDto, AppError> {
        let invocation = self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            ensure_run_version(
                &transaction,
                &request.planning_run_id,
                &request.expected_run_updated_at,
            )?;
            let agent = load_agent_record(
                &transaction,
                &request.planning_run_id,
                &request.planning_agent_id,
            )?;
            let open_questions = load_open_questions(&transaction, &agent.id)?;
            if open_questions.is_empty() {
                return Err(StoreError::App(AppError::conflict(
                    "This agent has no open planning questions.",
                )));
            }
            let mut answers = HashMap::new();
            for answer in &request.answers {
                let question_id = required(&answer.question_id, "A question ID is required.")
                    .map_err(StoreError::App)?;
                let answer_markdown =
                    required(&answer.answer_markdown, "Planning answers cannot be empty.")
                        .map_err(StoreError::App)?;
                if answers.insert(question_id, answer_markdown).is_some() {
                    return Err(StoreError::App(AppError::conflict(
                        "A planning question was answered more than once.",
                    )));
                }
            }
            let expected_ids = open_questions
                .iter()
                .map(|question| question.id.as_str())
                .collect::<HashSet<_>>();
            let provided_ids = answers.keys().map(String::as_str).collect::<HashSet<_>>();
            if expected_ids != provided_ids {
                return Err(StoreError::App(AppError::conflict(
                    "Answers must cover exactly the currently open questions.",
                )));
            }
            let timestamp = now();
            for question in &open_questions {
                transaction.execute(
                    "INSERT INTO planning_answers (
                        id, question_id, answer_markdown, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?4)",
                    params![
                        Uuid::new_v4().to_string(),
                        question.id,
                        answers[&question.id],
                        timestamp
                    ],
                )?;
                transaction.execute(
                    "UPDATE planning_questions
                     SET status = 'answered', updated_at = ?2 WHERE id = ?1 AND status = 'open'",
                    params![question.id, timestamp],
                )?;
            }
            let attempt = agent.attempt + 1;
            transaction.execute(
                "UPDATE planning_agents
                 SET status = 'running', attempt = ?2, error_code = NULL, error_message = NULL,
                     started_at = ?3, updated_at = ?3, completed_at = NULL
                 WHERE id = ?1",
                params![agent.id, attempt, timestamp],
            )?;
            let run_status = if agent.role == "synthesizer" {
                "synthesizing"
            } else {
                "running"
            };
            transaction.execute(
                "UPDATE planning_runs
                 SET status = ?2, error_code = NULL, error_message = NULL,
                     updated_at = ?3, completed_at = NULL WHERE id = ?1",
                params![request.planning_run_id, run_status, timestamp],
            )?;
            let prompt = answer_prompt(&open_questions, &answers).map_err(StoreError::App)?;
            let invocation = resume_invocation(
                &transaction,
                &request.planning_run_id,
                &agent,
                attempt,
                prompt,
            )?;
            transaction.commit()?;
            Ok(invocation)
        })?;
        let result = self.executor.resume_named(
            &invocation.repository_path,
            &invocation.session.name,
            &invocation.prompt,
        );
        self.persist_result(
            &request.planning_run_id,
            &invocation.agent_id,
            invocation.attempt,
            result,
            false,
        )?;
        self.advance(&request.planning_run_id)
    }

    pub fn retry(&self, request: &RetryPlanningRequest) -> Result<PlanningStateDto, AppError> {
        let invocations = self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            ensure_run_version(
                &transaction,
                &request.planning_run_id,
                &request.expected_run_updated_at,
            )?;
            let candidates = retry_candidates(
                &transaction,
                &request.planning_run_id,
                request.planning_agent_id.as_deref(),
            )?;
            if candidates.is_empty() {
                return Err(StoreError::App(AppError::conflict(
                    "There is no failed or blocked planning agent to retry.",
                )));
            }
            for agent in &candidates {
                if !load_open_questions(&transaction, &agent.id)?.is_empty() {
                    return Err(StoreError::App(AppError::conflict(
                        "Answer the agent's open questions instead of retrying it.",
                    )));
                }
            }
            let timestamp = now();
            let mut invocations = Vec::with_capacity(candidates.len());
            for agent in candidates {
                let attempt = agent.attempt + 1;
                let prompt = retry_prompt(&transaction, &agent.id)?;
                transaction.execute(
                    "UPDATE planning_agents
                     SET status = 'running', attempt = ?2, error_code = NULL,
                         error_message = NULL, started_at = ?3, updated_at = ?3,
                         completed_at = NULL WHERE id = ?1",
                    params![agent.id, attempt, timestamp],
                )?;
                invocations.push(resume_invocation(
                    &transaction,
                    &request.planning_run_id,
                    &agent,
                    attempt,
                    prompt,
                )?);
            }
            let status = if invocations
                .iter()
                .all(|invocation| invocation.session.role == AgentRole::Synthesizer)
            {
                "synthesizing"
            } else {
                "running"
            };
            transaction.execute(
                "UPDATE planning_runs SET status = ?2, error_code = NULL,
                 error_message = NULL, updated_at = ?3, completed_at = NULL WHERE id = ?1",
                params![request.planning_run_id, status, timestamp],
            )?;
            transaction.commit()?;
            Ok(invocations)
        })?;
        self.execute_resumes(&request.planning_run_id, invocations)?;
        self.advance(&request.planning_run_id)
    }

    pub fn reconcile_terminal_agent(
        &self,
        request: &ReconcilePlanningAgentRequest,
    ) -> Result<PlanningStateDto, AppError> {
        let work_item_id = required(&request.work_item_id, "A work item ID is required.")?;
        let planning_agent_id = required(
            &request.planning_agent_id,
            "A planning agent ID is required.",
        )?;
        let (run_id, invocation) = self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let run_id = transaction
                .query_row(
                    "SELECT planning_runs.id
                     FROM planning_agents
                     JOIN planning_runs
                       ON planning_runs.id = planning_agents.planning_run_id
                     JOIN work_items ON work_items.id = planning_runs.work_item_id
                     JOIN repositories ON repositories.id = work_items.repository_id
                     WHERE planning_agents.id = ?1
                       AND work_items.id = ?2
                       AND work_items.lifecycle_status = 'open'
                       AND repositories.archived_at IS NULL",
                    params![planning_agent_id, work_item_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::App(AppError::not_found(
                        "The planning agent does not belong to the requested active work item.",
                    ))
                })?;
            let agent = load_agent_record(&transaction, &run_id, &planning_agent_id)?;
            if !matches!(agent.status.as_str(), "blocked" | "failed") {
                return Err(StoreError::App(AppError::conflict(
                    "Only a blocked or failed persisted planning agent can be reconciled.",
                )));
            }
            let attempt = agent.attempt + 1;
            let timestamp = now();
            transaction.execute(
                "UPDATE planning_agents
                 SET status = 'running', attempt = ?2, error_code = NULL,
                     error_message = NULL, started_at = ?3, updated_at = ?3,
                     completed_at = NULL WHERE id = ?1",
                params![agent.id, attempt, timestamp],
            )?;
            let run_status = if agent.role == "synthesizer" {
                "synthesizing"
            } else {
                "running"
            };
            transaction.execute(
                "UPDATE planning_runs
                 SET status = ?2, error_code = NULL, error_message = NULL,
                     updated_at = ?3, completed_at = NULL WHERE id = ?1",
                params![run_id, run_status, timestamp],
            )?;
            let invocation = resume_invocation(
                &transaction,
                &run_id,
                &agent,
                attempt,
                "Reconcile this exact persisted planning session after interactive terminal work. Return the current result using the planning contract envelope; do not repeat already completed work.".to_owned(),
            )?;
            transaction.commit()?;
            Ok((run_id, invocation))
        })?;
        let result = self.executor.resume_named(
            &invocation.repository_path,
            &invocation.session.name,
            &invocation.prompt,
        );
        self.persist_result(
            &run_id,
            &invocation.agent_id,
            invocation.attempt,
            result,
            true,
        )?;
        self.advance(&run_id)
    }

    fn execute_initial_planners(
        &self,
        run_id: &str,
        invocations: Vec<Invocation>,
    ) -> Result<(), AppError> {
        let results = thread::scope(|scope| {
            invocations
                .into_iter()
                .map(|invocation| {
                    let agent_id = invocation.agent_id.clone();
                    let attempt = invocation.attempt;
                    let handle = scope.spawn(move || {
                        self.executor.start_planner(
                            &invocation.repository_path,
                            &invocation.model,
                            &invocation.session,
                            &invocation.requirements,
                        )
                    });
                    (agent_id, attempt, handle)
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|(agent_id, attempt, handle)| {
                    let result = handle.join().unwrap_or_else(|_| {
                        Err(AppError::external(
                            "A planning worker panicked unexpectedly.",
                        ))
                    });
                    (agent_id, attempt, result)
                })
                .collect::<Vec<_>>()
        });
        for (agent_id, attempt, result) in results {
            self.persist_result(run_id, &agent_id, attempt, result, false)?;
        }
        Ok(())
    }

    fn execute_resumes(
        &self,
        run_id: &str,
        invocations: Vec<ResumeInvocation>,
    ) -> Result<(), AppError> {
        let results = thread::scope(|scope| {
            invocations
                .into_iter()
                .map(|invocation| {
                    let agent_id = invocation.agent_id.clone();
                    let attempt = invocation.attempt;
                    let handle = scope.spawn(move || {
                        self.executor.resume_named(
                            &invocation.repository_path,
                            &invocation.session.name,
                            &invocation.prompt,
                        )
                    });
                    (agent_id, attempt, handle)
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|(agent_id, attempt, handle)| {
                    let result = handle.join().unwrap_or_else(|_| {
                        Err(AppError::external(
                            "A planning worker panicked unexpectedly.",
                        ))
                    });
                    (agent_id, attempt, result)
                })
                .collect::<Vec<_>>()
        });
        for (agent_id, attempt, result) in results {
            self.persist_result(run_id, &agent_id, attempt, result, false)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn persist_result(
        &self,
        run_id: &str,
        agent_id: &str,
        attempt: usize,
        result: Result<CopilotRunOutput, AppError>,
        supersede_open_questions: bool,
    ) -> Result<(), AppError> {
        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let timestamp = now();
            match result {
                Err(error) => {
                    transaction.execute(
                        "UPDATE planning_agents
                         SET status = 'failed', error_code = ?2, error_message = ?3,
                             updated_at = ?4, completed_at = ?4
                         WHERE id = ?1 AND planning_run_id = ?5 AND attempt = ?6",
                        params![
                            agent_id,
                            error.code,
                            error.message,
                            timestamp,
                            run_id,
                            attempt
                        ],
                    )?;
                }
                Ok(output) => {
                    if supersede_open_questions {
                        transaction.execute(
                            "UPDATE planning_questions
                             SET status = 'dismissed', updated_at = ?2
                             WHERE planning_agent_id = ?1 AND status = 'open'",
                            params![agent_id, timestamp],
                        )?;
                    }
                    for event in output.events {
                        let payload_json =
                            serde_json::to_string(&event.payload).map_err(|error| {
                                StoreError::App(AppError::external(format!(
                                    "Quorum could not persist a Copilot event: {error}"
                                )))
                            })?;
                        transaction.execute(
                            "INSERT INTO planning_agent_events (
                                id, planning_run_id, planning_agent_id, attempt, sequence,
                                event_kind, payload_json, created_at
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            params![
                                Uuid::new_v4().to_string(),
                                run_id,
                                agent_id,
                                attempt,
                                event.sequence,
                                event.kind,
                                payload_json,
                                timestamp
                            ],
                        )?;
                    }
                    match output.envelope.outcome {
                        AgentOutcome::Completed => {
                            let role: String = transaction.query_row(
                                "SELECT role FROM planning_agents WHERE id = ?1",
                                [agent_id],
                                |row| row.get(0),
                            )?;
                            let artifact_kind = if role == "planner" {
                                "planner_output"
                            } else {
                                "synthesized_plan"
                            };
                            transaction.execute(
                                "INSERT INTO planning_artifacts (
                                    id, planning_run_id, planning_agent_id, artifact_kind,
                                    markdown_body, attempt, sequence, created_at
                                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
                                params![
                                    Uuid::new_v4().to_string(),
                                    run_id,
                                    agent_id,
                                    artifact_kind,
                                    output.envelope.markdown.expect("validated markdown"),
                                    attempt,
                                    timestamp
                                ],
                            )?;
                            transaction.execute(
                                "UPDATE planning_agents
                                 SET status = 'succeeded', error_code = NULL, error_message = NULL,
                                     updated_at = ?2, completed_at = ?2
                                 WHERE id = ?1 AND attempt = ?3",
                                params![agent_id, timestamp, attempt],
                            )?;
                        }
                        AgentOutcome::NeedsInput => {
                            let next_ordinal: usize = transaction.query_row(
                                "SELECT COALESCE(MAX(ordinal) + 1, 0)
                                 FROM planning_questions WHERE planning_agent_id = ?1",
                                [agent_id],
                                |row| row.get(0),
                            )?;
                            for (offset, question) in
                                output.envelope.questions.into_iter().enumerate()
                            {
                                transaction.execute(
                                    "INSERT INTO planning_questions (
                                        id, planning_run_id, planning_agent_id, external_id,
                                        ordinal, prompt_markdown, status, created_at, updated_at
                                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'open', ?7, ?7)",
                                    params![
                                        Uuid::new_v4().to_string(),
                                        run_id,
                                        agent_id,
                                        question.id,
                                        next_ordinal + offset,
                                        question.prompt,
                                        timestamp
                                    ],
                                )?;
                            }
                            transaction.execute(
                                "UPDATE planning_agents
                                 SET status = 'blocked', error_code = NULL, error_message = NULL,
                                     updated_at = ?2, completed_at = NULL
                                 WHERE id = ?1 AND attempt = ?3",
                                params![agent_id, timestamp, attempt],
                            )?;
                        }
                        AgentOutcome::Blocked => {
                            transaction.execute(
                                "UPDATE planning_agents
                                 SET status = 'blocked', error_code = 'agent_blocked',
                                     error_message = ?2, updated_at = ?3, completed_at = ?3
                                 WHERE id = ?1 AND attempt = ?4",
                                params![
                                    agent_id,
                                    output.envelope.error.expect("validated error"),
                                    timestamp,
                                    attempt
                                ],
                            )?;
                        }
                    }
                }
            }
            transaction.execute(
                "UPDATE planning_runs SET updated_at = ?2 WHERE id = ?1",
                params![run_id, timestamp],
            )?;
            transaction.commit()?;
            Ok(())
        })
    }

    #[allow(clippy::too_many_lines)]
    fn advance(&self, run_id: &str) -> Result<PlanningStateDto, AppError> {
        let action = self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let statuses = agent_statuses(&transaction, run_id)?;
            if let Some(failed) = statuses
                .iter()
                .find(|agent| agent.status == "failed" && agent.role == "planner")
            {
                set_run_error(&transaction, run_id, "failed", failed)?;
                transaction.commit()?;
                return Ok(Advance::Return);
            }
            if let Some(blocked) = statuses.iter().find(|agent| {
                agent.role == "planner" && agent.status == "blocked" && agent.error_code.is_some()
            }) {
                set_run_error(&transaction, run_id, "blocked", blocked)?;
                transaction.commit()?;
                return Ok(Advance::Return);
            }
            let open_questions: bool = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM planning_questions
                    WHERE planning_run_id = ?1 AND status = 'open'
                 )",
                [run_id],
                |row| row.get(0),
            )?;
            if open_questions {
                update_run_status(
                    &transaction,
                    run_id,
                    "waiting_for_answers",
                    None,
                    None,
                    false,
                )?;
                transaction.commit()?;
                return Ok(Advance::Return);
            }
            if statuses
                .iter()
                .any(|agent| agent.role == "planner" && agent.status != "succeeded")
            {
                update_run_status(&transaction, run_id, "running", None, None, false)?;
                transaction.commit()?;
                return Ok(Advance::Return);
            }
            let synthesizer = statuses
                .iter()
                .find(|agent| agent.role == "synthesizer")
                .ok_or_else(|| {
                    StoreError::App(AppError::database(
                        "The planning run has no synthesizer agent.",
                    ))
                })?;
            match synthesizer.status.as_str() {
                "pending" => {
                    let invocation = begin_synthesis(&transaction, run_id, synthesizer)?;
                    transaction.commit()?;
                    Ok(Advance::Synthesize(Box::new(invocation)))
                }
                "running" => {
                    update_run_status(&transaction, run_id, "synthesizing", None, None, false)?;
                    transaction.commit()?;
                    Ok(Advance::Return)
                }
                "failed" => {
                    set_run_error(&transaction, run_id, "failed", synthesizer)?;
                    transaction.commit()?;
                    Ok(Advance::Return)
                }
                "blocked" if synthesizer.error_code.is_some() => {
                    set_run_error(&transaction, run_id, "blocked", synthesizer)?;
                    transaction.commit()?;
                    Ok(Advance::Return)
                }
                "blocked" => {
                    update_run_status(
                        &transaction,
                        run_id,
                        "waiting_for_answers",
                        None,
                        None,
                        false,
                    )?;
                    transaction.commit()?;
                    Ok(Advance::Return)
                }
                "succeeded" => {
                    persist_plan(&transaction, run_id)?;
                    transaction.commit()?;
                    Ok(Advance::Return)
                }
                status => Err(StoreError::App(AppError::database(format!(
                    "Unknown synthesizer status {status}."
                )))),
            }
        })?;

        if let Advance::Synthesize(invocation) = action {
            let result = self.executor.start_synthesizer(
                &invocation.repository_path,
                &invocation.model,
                &invocation.session,
                &invocation.requirements,
                &invocation.artifacts,
            );
            self.persist_result(
                run_id,
                &invocation.agent_id,
                invocation.attempt,
                result,
                false,
            )?;
            return self.advance(run_id);
        }
        self.state(run_id)
    }

    fn state(&self, run_id: &str) -> Result<PlanningStateDto, AppError> {
        self.store
            .with_connection(|connection| load_state(connection, run_id))
    }
}

#[derive(Clone)]
struct WorkItem {
    id: String,
    title: String,
    markdown: String,
    repository_path: String,
}

enum Creation {
    Created {
        run_id: String,
        planners: Vec<Invocation>,
    },
}

struct Invocation {
    agent_id: String,
    model: String,
    session: AgentSession,
    repository_path: String,
    requirements: NormalizedRequirements,
    attempt: usize,
}

struct ResumeInvocation {
    agent_id: String,
    session: AgentSession,
    repository_path: String,
    prompt: String,
    attempt: usize,
}

struct SynthesisInvocation {
    agent_id: String,
    model: String,
    session: AgentSession,
    repository_path: String,
    requirements: NormalizedRequirements,
    artifacts: Vec<CompletedPlannerArtifact>,
    attempt: usize,
}

enum Advance {
    Return,
    Synthesize(Box<SynthesisInvocation>),
}

#[derive(Clone)]
struct AgentRecord {
    id: String,
    role: String,
    ordinal: usize,
    model: String,
    session_name: String,
    status: String,
    attempt: usize,
    error_code: Option<String>,
    error_message: Option<String>,
}

struct OpenQuestion {
    id: String,
    external_id: String,
    prompt: String,
}

fn planner_models(transaction: &Transaction<'_>) -> Result<Vec<String>, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT model_id FROM model_assignments
         WHERE role = 'planner' ORDER BY position",
    )?;
    let models = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    if !(2..=3).contains(&models.len()) {
        return Err(AppError::validation("Configure between two and three planners.").into());
    }
    Ok(models)
}

fn insert_agent(
    transaction: &Transaction<'_>,
    run_id: &str,
    session: &AgentSession,
    model: &str,
    status: &str,
    attempt: usize,
    timestamp: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO planning_agents (
            id, planning_run_id, role, ordinal, model_id, session_name, status,
            attempt, started_at, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
        params![
            session.id.to_string(),
            run_id,
            role_name(session.role),
            session.ordinal,
            model,
            session.name,
            status,
            attempt,
            (status == "running").then_some(timestamp),
            timestamp
        ],
    )?;
    Ok(())
}

fn role_name(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Planner => "planner",
        AgentRole::Synthesizer => "synthesizer",
    }
}

fn parse_role(role: &str) -> Result<AgentRole, StoreError> {
    match role {
        "planner" => Ok(AgentRole::Planner),
        "synthesizer" => Ok(AgentRole::Synthesizer),
        _ => Err(AppError::database(format!("Unknown planning agent role {role}.")).into()),
    }
}

fn parse_session(agent: &AgentRecord) -> Result<AgentSession, StoreError> {
    let id = Uuid::parse_str(&agent.id).map_err(|error| {
        StoreError::App(AppError::database(format!(
            "A planning agent has an invalid identifier: {error}"
        )))
    })?;
    Ok(AgentSession::persisted(
        id,
        agent.session_name.clone(),
        parse_role(&agent.role)?,
        agent.ordinal,
    ))
}

fn load_agent_record(
    transaction: &Transaction<'_>,
    run_id: &str,
    agent_id: &str,
) -> Result<AgentRecord, StoreError> {
    transaction
        .query_row(
            "SELECT id, role, ordinal, model_id, session_name, status, attempt,
                    error_code, error_message
             FROM planning_agents WHERE id = ?1 AND planning_run_id = ?2",
            params![agent_id, run_id],
            agent_record_from_row,
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::App(AppError::not_found(
                "The planning agent could not be found.",
            ))
        })
}

fn agent_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRecord> {
    Ok(AgentRecord {
        id: row.get(0)?,
        role: row.get(1)?,
        ordinal: row.get(2)?,
        model: row.get(3)?,
        session_name: row.get(4)?,
        status: row.get(5)?,
        attempt: row.get(6)?,
        error_code: row.get(7)?,
        error_message: row.get(8)?,
    })
}

fn load_open_questions(
    transaction: &Transaction<'_>,
    agent_id: &str,
) -> Result<Vec<OpenQuestion>, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT id, external_id, prompt_markdown
         FROM planning_questions
         WHERE planning_agent_id = ?1 AND status = 'open'
         ORDER BY ordinal",
    )?;
    let questions = statement
        .query_map([agent_id], |row| {
            Ok(OpenQuestion {
                id: row.get(0)?,
                external_id: row.get(1)?,
                prompt: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(questions)
}

fn answer_prompt(
    questions: &[OpenQuestion],
    answers: &HashMap<String, String>,
) -> Result<String, AppError> {
    let answered = questions
        .iter()
        .map(|question| {
            json!({
                "questionId": question.external_id,
                "question": question.prompt,
                "answer": answers[&question.id],
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&answered)
        .map(|json| {
            format!(
                "Continue the existing planning session using these persisted answers. Return the next planning contract envelope.\n\nPERSISTED_ANSWERS_JSON\n{json}"
            )
        })
        .map_err(|error| {
            AppError::external(format!("Quorum could not serialize planning answers: {error}"))
        })
}

fn retry_prompt(transaction: &Transaction<'_>, agent_id: &str) -> Result<String, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT planning_questions.external_id, planning_questions.prompt_markdown,
                planning_answers.answer_markdown
         FROM planning_questions
         JOIN planning_answers ON planning_answers.question_id = planning_questions.id
         WHERE planning_questions.planning_agent_id = ?1
         ORDER BY planning_questions.ordinal",
    )?;
    let answers = statement
        .query_map([agent_id], |row| {
            Ok(json!({
                "questionId": row.get::<_, String>(0)?,
                "question": row.get::<_, String>(1)?,
                "answer": row.get::<_, String>(2)?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if answers.is_empty() {
        return Ok(
            "Retry the interrupted planning operation. Use the prior session context and return a fresh contract envelope."
                .to_owned(),
        );
    }
    let answers = serde_json::to_string_pretty(&answers).map_err(|error| {
        StoreError::App(AppError::external(format!(
            "Quorum could not serialize persisted planning answers: {error}"
        )))
    })?;
    Ok(format!(
        "Retry the interrupted planning operation. Apply these durable answers even if the prior process ended before they reached this session, then return a fresh planning contract envelope.\n\nPERSISTED_ANSWERS_JSON\n{answers}"
    ))
}

fn resume_invocation(
    transaction: &Transaction<'_>,
    run_id: &str,
    agent: &AgentRecord,
    attempt: usize,
    prompt: String,
) -> Result<ResumeInvocation, StoreError> {
    let repository_path: String = transaction.query_row(
        "SELECT repositories.root_path
         FROM planning_runs
         JOIN work_items ON work_items.id = planning_runs.work_item_id
         JOIN repositories ON repositories.id = work_items.repository_id
         WHERE planning_runs.id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    Ok(ResumeInvocation {
        agent_id: agent.id.clone(),
        session: parse_session(agent)?,
        repository_path,
        prompt,
        attempt,
    })
}

fn retry_candidates(
    transaction: &Transaction<'_>,
    run_id: &str,
    selected_agent_id: Option<&str>,
) -> Result<Vec<AgentRecord>, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT id, role, ordinal, model_id, session_name, status, attempt,
                error_code, error_message
         FROM planning_agents
         WHERE planning_run_id = ?1
           AND status IN ('blocked', 'failed')
           AND (?2 IS NULL OR id = ?2)
         ORDER BY CASE role WHEN 'planner' THEN 0 ELSE 1 END, ordinal",
    )?;
    let candidates = statement
        .query_map(params![run_id, selected_agent_id], agent_record_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    if selected_agent_id.is_some() && candidates.is_empty() {
        return Err(AppError::conflict("The selected planning agent is not retryable.").into());
    }
    Ok(candidates)
}

fn agent_statuses(
    transaction: &Transaction<'_>,
    run_id: &str,
) -> Result<Vec<AgentRecord>, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT id, role, ordinal, model_id, session_name, status, attempt,
                error_code, error_message
         FROM planning_agents WHERE planning_run_id = ?1
         ORDER BY CASE role WHEN 'planner' THEN 0 ELSE 1 END, ordinal",
    )?;
    let statuses = statement
        .query_map([run_id], agent_record_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(statuses)
}

fn begin_synthesis(
    transaction: &Transaction<'_>,
    run_id: &str,
    synthesizer: &AgentRecord,
) -> Result<SynthesisInvocation, StoreError> {
    let work_item: WorkItem = transaction.query_row(
        "SELECT work_items.id, work_items.title, work_items.markdown_body,
                repositories.root_path
         FROM planning_runs
         JOIN work_items ON work_items.id = planning_runs.work_item_id
         JOIN repositories ON repositories.id = work_items.repository_id
         WHERE planning_runs.id = ?1",
        [run_id],
        |row| {
            Ok(WorkItem {
                id: row.get(0)?,
                title: row.get(1)?,
                markdown: row.get(2)?,
                repository_path: row.get(3)?,
            })
        },
    )?;
    let mut statement = transaction.prepare(
        "SELECT planning_agents.session_name, planning_agents.model_id,
                planning_artifacts.markdown_body
         FROM planning_agents
         JOIN planning_artifacts ON planning_artifacts.planning_agent_id = planning_agents.id
         WHERE planning_agents.planning_run_id = ?1
           AND planning_agents.role = 'planner'
           AND planning_agents.status = 'succeeded'
           AND planning_artifacts.artifact_kind = 'planner_output'
         ORDER BY planning_agents.ordinal, planning_artifacts.attempt DESC",
    )?;
    let rows = statement
        .query_map([run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut artifacts = Vec::new();
    let mut seen = HashSet::new();
    for (session_name, model, markdown) in rows {
        if seen.insert(session_name.clone()) {
            artifacts.push(
                CompletedPlannerArtifact::new(&session_name, &model, &markdown)
                    .map_err(StoreError::App)?,
            );
        }
    }
    let attempt = synthesizer.attempt + 1;
    let timestamp = now();
    let synthesis_input = serde_json::to_string_pretty(&artifacts).map_err(|error| {
        StoreError::App(AppError::external(format!(
            "Quorum could not serialize synthesis input: {error}"
        )))
    })?;
    transaction.execute(
        "INSERT INTO planning_artifacts (
            id, planning_run_id, planning_agent_id, artifact_kind, markdown_body,
            attempt, sequence, created_at
         ) VALUES (?1, ?2, ?3, 'synthesis_input', ?4, ?5, 0, ?6)",
        params![
            Uuid::new_v4().to_string(),
            run_id,
            synthesizer.id,
            synthesis_input,
            attempt,
            timestamp
        ],
    )?;
    transaction.execute(
        "UPDATE planning_agents
         SET status = 'running', attempt = ?2, started_at = ?3, updated_at = ?3
         WHERE id = ?1",
        params![synthesizer.id, attempt, timestamp],
    )?;
    update_run_status(transaction, run_id, "synthesizing", None, None, false)?;
    Ok(SynthesisInvocation {
        agent_id: synthesizer.id.clone(),
        model: synthesizer.model.clone(),
        session: parse_session(synthesizer)?,
        repository_path: work_item.repository_path,
        requirements: NormalizedRequirements::new(&work_item.title, &work_item.markdown)
            .map_err(StoreError::App)?,
        artifacts,
        attempt,
    })
}

fn persist_plan(transaction: &Transaction<'_>, run_id: &str) -> Result<(), StoreError> {
    let (work_item_id, markdown, require_approval): (String, String, bool) = transaction
        .query_row(
            "SELECT planning_runs.work_item_id, planning_artifacts.markdown_body,
                work_items.require_plan_approval
         FROM planning_runs
         JOIN work_items ON work_items.id = planning_runs.work_item_id
         JOIN planning_artifacts ON planning_artifacts.planning_run_id = planning_runs.id
         WHERE planning_runs.id = ?1
           AND planning_artifacts.artifact_kind = 'synthesized_plan'
         ORDER BY planning_artifacts.attempt DESC LIMIT 1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let timestamp = now();
    let existing: Option<(String, String)> = transaction
        .query_row(
            "SELECT id, markdown_body FROM plans WHERE planning_run_id = ?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((plan_id, existing_markdown)) = existing {
        if existing_markdown != markdown {
            transaction.execute(
                "UPDATE plans
                 SET markdown_body = ?2, edit_revision = edit_revision + 1, updated_at = ?3
                 WHERE id = ?1",
                params![plan_id, markdown, timestamp],
            )?;
        }
    } else {
        let revision: usize = transaction.query_row(
            "SELECT COALESCE(MAX(revision), 0) + 1 FROM plans WHERE work_item_id = ?1",
            [&work_item_id],
            |row| row.get(0),
        )?;
        let approval_policy = if require_approval {
            "required"
        } else {
            "not_required"
        };
        let approval_status = if require_approval { "pending" } else { "draft" };
        let eligibility_key = (!require_approval).then(|| Uuid::new_v4().to_string());
        let eligible_at = (!require_approval).then_some(timestamp.as_str());
        transaction.execute(
            "INSERT INTO plans (
                id, work_item_id, revision, markdown_body, approval_policy,
                approval_status, created_at, updated_at, planning_run_id, edit_revision,
                queue_eligibility_key, queue_eligible_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, 1, ?9, ?10)",
            params![
                Uuid::new_v4().to_string(),
                work_item_id,
                revision,
                markdown,
                approval_policy,
                approval_status,
                timestamp,
                run_id,
                eligibility_key,
                eligible_at
            ],
        )?;
    }
    let run_status: String = transaction.query_row(
        "SELECT status FROM planning_runs WHERE id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    if run_status == "succeeded" {
        Ok(())
    } else {
        update_run_status(transaction, run_id, "succeeded", None, None, true)
    }
}

fn set_run_error(
    transaction: &Transaction<'_>,
    run_id: &str,
    status: &str,
    agent: &AgentRecord,
) -> Result<(), StoreError> {
    update_run_status(
        transaction,
        run_id,
        status,
        agent.error_code.as_deref(),
        agent.error_message.as_deref(),
        true,
    )
}

fn update_run_status(
    transaction: &Transaction<'_>,
    run_id: &str,
    status: &str,
    error_code: Option<&str>,
    error_message: Option<&str>,
    completed: bool,
) -> Result<(), StoreError> {
    let timestamp = now();
    transaction.execute(
        "UPDATE planning_runs
         SET status = ?2, error_code = ?3, error_message = ?4, updated_at = ?5,
             completed_at = ?6 WHERE id = ?1",
        params![
            run_id,
            status,
            error_code,
            error_message,
            timestamp,
            completed.then_some(timestamp.as_str())
        ],
    )?;
    Ok(())
}

fn ensure_run_version(
    transaction: &Transaction<'_>,
    run_id: &str,
    expected_updated_at: &str,
) -> Result<(), StoreError> {
    let actual: Option<String> = transaction
        .query_row(
            "SELECT updated_at FROM planning_runs WHERE id = ?1",
            [run_id],
            |row| row.get(0),
        )
        .optional()?;
    match actual {
        None => Err(AppError::not_found("The planning run could not be found.").into()),
        Some(actual) if actual != expected_updated_at => {
            Err(AppError::conflict("The planning run changed after it was loaded.").into())
        }
        Some(_) => Ok(()),
    }
}

struct PlanMutation {
    id: String,
    markdown_body: String,
    approval_policy: String,
    approval_status: String,
}

fn load_plan_mutation(
    transaction: &Transaction<'_>,
    run_id: &str,
    expected_updated_at: &str,
) -> Result<PlanMutation, StoreError> {
    let expected_updated_at = required(
        expected_updated_at,
        "The expected plan version is required.",
    )
    .map_err(StoreError::App)?;
    transaction
        .query_row(
            "SELECT plans.id, plans.markdown_body, plans.approval_policy,
                    plans.approval_status, plans.updated_at
             FROM plans
             JOIN planning_runs ON planning_runs.id = plans.planning_run_id
             WHERE plans.planning_run_id = ?1
               AND planning_runs.status = 'succeeded'",
            [run_id],
            |row| {
                Ok((
                    PlanMutation {
                        id: row.get(0)?,
                        markdown_body: row.get(1)?,
                        approval_policy: row.get(2)?,
                        approval_status: row.get(3)?,
                    },
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::App(AppError::conflict(
                "A completed synthesized plan is required for this operation.",
            ))
        })
        .and_then(|(plan, updated_at)| {
            if updated_at == expected_updated_at {
                Ok(plan)
            } else {
                Err(StoreError::App(AppError::conflict(
                    "The synthesized plan changed after it was loaded.",
                )))
            }
        })
}

fn ensure_plan_not_queued(transaction: &Transaction<'_>, plan_id: &str) -> Result<(), StoreError> {
    let queued = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM queue_entries
            WHERE plan_id = ?1 AND scheduling_status IN ('queued', 'paused')
         )",
        [plan_id],
        |row| row.get::<_, bool>(0),
    )?;
    if queued {
        Err(AppError::conflict("A queued plan can no longer be changed.").into())
    } else {
        Ok(())
    }
}

#[allow(clippy::too_many_lines)]
fn load_detail(
    connection: &Connection,
    state: PlanningStateDto,
) -> Result<PlanningDetailDto, StoreError> {
    let (source, source_metadata_json) = connection
        .query_row(
            "SELECT work_items.id, work_items.repository_id, work_items.title,
                    work_items.source_kind, work_items.markdown_body,
                    work_items.source_metadata_json
             FROM work_items
             JOIN repositories ON repositories.id = work_items.repository_id
             WHERE work_items.id = ?1
               AND work_items.lifecycle_status = 'open'
               AND repositories.archived_at IS NULL",
            [&state.run.work_item_id],
            |row| {
                Ok((
                    PlanningSourceDto {
                        work_item_id: row.get(0)?,
                        repository_id: row.get(1)?,
                        title: row.get(2)?,
                        kind: row.get(3)?,
                        reference: None,
                        markdown_body: row.get(4)?,
                    },
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::App(AppError::not_found(
                "The active planning source could not be found.",
            ))
        })?;
    let metadata: Value = serde_json::from_str(&source_metadata_json).map_err(|error| {
        StoreError::App(AppError::database(format!(
            "The planning source metadata is invalid JSON: {error}"
        )))
    })?;
    let source = PlanningSourceDto {
        reference: metadata
            .get("url")
            .or_else(|| metadata.get("path"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        ..source
    };

    let queue_entry = state
        .plan
        .as_ref()
        .map(|plan| {
            connection
                .query_row(
                    "SELECT id, position, scheduling_status, created_at, updated_at
                     FROM queue_entries WHERE plan_id = ?1
                     ORDER BY created_at DESC LIMIT 1",
                    [&plan.id],
                    |row| {
                        Ok(QueueEntryDto {
                            id: row.get(0)?,
                            position: row.get(1)?,
                            scheduling_status: row.get(2)?,
                            created_at: row.get(3)?,
                            updated_at: row.get(4)?,
                        })
                    },
                )
                .optional()
        })
        .transpose()?
        .flatten();
    let has_eligibility: bool = state
        .plan
        .as_ref()
        .map(|plan| {
            connection.query_row(
                "SELECT queue_eligibility_key IS NOT NULL
                        AND queue_eligible_at IS NOT NULL
                 FROM plans WHERE id = ?1",
                [&plan.id],
                |row| row.get(0),
            )
        })
        .transpose()?
        .unwrap_or(false);
    let (queue_state, eligible, queue_reason) = if let Some(entry) = &queue_entry {
        (entry.scheduling_status.clone(), false, None)
    } else if let Some(plan) = &state.plan {
        if plan.approval_policy == "required" && plan.approval_status != "approved" {
            (
                "awaiting_approval".to_owned(),
                false,
                Some("Approve the synthesized plan before enqueueing it.".to_owned()),
            )
        } else if has_eligibility {
            ("eligible".to_owned(), true, None)
        } else {
            (
                "not_eligible".to_owned(),
                false,
                Some("The synthesized plan is not currently queue eligible.".to_owned()),
            )
        }
    } else {
        (
            "not_ready".to_owned(),
            false,
            Some("Planning must finish before the work item can be queued.".to_owned()),
        )
    };
    let queue = PlanningQueueDto {
        state: queue_state,
        eligible,
        reason: queue_reason,
        entry: queue_entry,
    };
    let current_phase = if queue.entry.is_some() {
        "queue"
    } else if state.run.status != "succeeded" {
        "planning"
    } else if state.plan.as_ref().is_some_and(|plan| {
        plan.approval_policy == "required" && plan.approval_status != "approved"
    }) {
        "approval"
    } else {
        "ready"
    }
    .to_owned();
    let status = match current_phase.as_str() {
        "queue" | "ready" => queue.state.clone(),
        "approval" => state.plan.as_ref().map_or_else(
            || state.run.status.clone(),
            |plan| plan.approval_status.clone(),
        ),
        _ => state.run.status.clone(),
    };
    let terminal_handoff = connection
        .query_row(
            "SELECT id, planning_agent_id, session_name, status, error_code, error_message,
                    created_at, updated_at
             FROM terminal_handoffs
             WHERE planning_run_id = ?1
             ORDER BY created_at DESC LIMIT 1",
            [&state.run.id],
            |row| {
                let status: String = row.get(3)?;
                Ok(TerminalHandoffSummaryDto {
                    id: row.get(0)?,
                    planning_agent_id: row.get(1)?,
                    session_name: row.get(2)?,
                    manual_reconcile_available: matches!(
                        status.as_str(),
                        "launch_failed" | "awaiting_manual_reconcile" | "reconcile_failed"
                    ),
                    status,
                    error_code: row.get(4)?,
                    error_message: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            },
        )
        .optional()?;
    let pending_questions = state
        .questions
        .iter()
        .filter(|question| question.status == "open")
        .cloned()
        .collect();
    let answered_questions = state
        .questions
        .iter()
        .filter(|question| question.status == "answered")
        .cloned()
        .collect();
    let recent_events = state
        .events
        .iter()
        .rev()
        .take(25)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Ok(PlanningDetailDto {
        source,
        current_phase,
        status,
        run: state.run,
        agents: state.agents,
        pending_questions,
        answered_questions,
        plan: state.plan,
        queue,
        recent_events,
        terminal_handoff,
    })
}

#[allow(clippy::too_many_lines)]
fn load_state(connection: &Connection, run_id: &str) -> Result<PlanningStateDto, StoreError> {
    let run = connection
        .query_row(
            "SELECT id, work_item_id, status, error_code, error_message, idempotency_key,
                    created_at, updated_at, completed_at
             FROM planning_runs WHERE id = ?1",
            [run_id],
            |row| {
                Ok(PlanningRunDto {
                    id: row.get(0)?,
                    work_item_id: row.get(1)?,
                    status: row.get(2)?,
                    error_code: row.get(3)?,
                    error_message: row.get(4)?,
                    idempotency_key: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                    completed_at: row.get(8)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::App(AppError::not_found("The planning run could not be found."))
        })?;
    let mut agent_statement = connection.prepare(
        "SELECT planning_agents.id, planning_agents.role, planning_agents.ordinal,
                planning_agents.model_id, planning_agents.session_name,
                CASE
                  WHEN planning_agents.status = 'blocked' AND EXISTS(
                    SELECT 1 FROM planning_questions
                    WHERE planning_agent_id = planning_agents.id AND status = 'open'
                  ) THEN 'waiting_for_answers'
                  ELSE planning_agents.status
                END,
                planning_agents.attempt,
                error_code, error_message, created_at, updated_at, completed_at
         FROM planning_agents WHERE planning_agents.planning_run_id = ?1
         ORDER BY CASE role WHEN 'planner' THEN 0 ELSE 1 END, ordinal",
    )?;
    let agents = agent_statement
        .query_map([run_id], |row| {
            Ok(PlanningAgentDto {
                id: row.get(0)?,
                role: row.get(1)?,
                ordinal: row.get(2)?,
                model_id: row.get(3)?,
                session_name: row.get(4)?,
                status: row.get(5)?,
                attempt: row.get(6)?,
                error_code: row.get(7)?,
                error_message: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
                completed_at: row.get(11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut event_statement = connection.prepare(
        "SELECT planning_agent_events.id, planning_agent_events.planning_agent_id,
                planning_agent_events.attempt, planning_agent_events.sequence,
                planning_agent_events.event_kind, planning_agent_events.payload_json,
                planning_agent_events.created_at
         FROM planning_agent_events
         JOIN planning_agents ON planning_agents.id = planning_agent_events.planning_agent_id
         WHERE planning_agent_events.planning_run_id = ?1
         ORDER BY CASE planning_agents.role WHEN 'planner' THEN 0 ELSE 1 END,
                  planning_agents.ordinal, planning_agent_events.attempt,
                  planning_agent_events.sequence",
    )?;
    let event_rows = event_statement
        .query_map([run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, usize>(2)?,
                row.get::<_, usize>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let events = event_rows
        .into_iter()
        .map(
            |(id, planning_agent_id, attempt, sequence, event_kind, payload_json, created_at)| {
                let payload = serde_json::from_str(&payload_json).map_err(|error| {
                    StoreError::App(AppError::database(format!(
                        "A persisted planning event is invalid JSON: {error}"
                    )))
                })?;
                Ok(PlanningEventDto {
                    id,
                    planning_agent_id,
                    attempt,
                    sequence,
                    event_kind,
                    payload,
                    created_at,
                })
            },
        )
        .collect::<Result<Vec<_>, StoreError>>()?;
    let mut artifact_statement = connection.prepare(
        "SELECT planning_artifacts.id, planning_artifacts.planning_agent_id,
                planning_artifacts.artifact_kind, planning_artifacts.markdown_body,
                planning_artifacts.attempt, planning_artifacts.sequence,
                planning_artifacts.created_at
         FROM planning_artifacts
         LEFT JOIN planning_agents ON planning_agents.id = planning_artifacts.planning_agent_id
         WHERE planning_artifacts.planning_run_id = ?1
         ORDER BY CASE planning_agents.role
                    WHEN 'planner' THEN 0
                    WHEN 'synthesizer' THEN 1
                    ELSE 2
                  END,
                  planning_agents.ordinal, planning_artifacts.attempt,
                  CASE planning_artifacts.artifact_kind
                    WHEN 'synthesis_input' THEN 0
                    ELSE 1
                  END,
                  planning_artifacts.sequence, planning_artifacts.id",
    )?;
    let artifacts = artifact_statement
        .query_map([run_id], |row| {
            Ok(PlanningArtifactDto {
                id: row.get(0)?,
                planning_agent_id: row.get(1)?,
                artifact_kind: row.get(2)?,
                markdown_body: row.get(3)?,
                attempt: row.get(4)?,
                sequence: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut question_statement = connection.prepare(
        "SELECT planning_questions.id, planning_questions.planning_agent_id,
                planning_questions.external_id, planning_questions.ordinal,
                planning_questions.prompt_markdown, planning_questions.status,
                planning_answers.answer_markdown, planning_questions.created_at,
                planning_questions.updated_at
         FROM planning_questions
         JOIN planning_agents ON planning_agents.id = planning_questions.planning_agent_id
         LEFT JOIN planning_answers ON planning_answers.question_id = planning_questions.id
         WHERE planning_questions.planning_run_id = ?1
         ORDER BY CASE planning_agents.role WHEN 'planner' THEN 0 ELSE 1 END,
                  planning_agents.ordinal, planning_questions.ordinal",
    )?;
    let questions = question_statement
        .query_map([run_id], |row| {
            Ok(PlanningQuestionDto {
                id: row.get(0)?,
                planning_agent_id: row.get(1)?,
                external_id: row.get(2)?,
                ordinal: row.get(3)?,
                prompt_markdown: row.get(4)?,
                status: row.get(5)?,
                answer_markdown: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let plan = connection
        .query_row(
            "SELECT id, revision, edit_revision, markdown_body, approval_policy,
                    approval_status, created_at, updated_at
             FROM plans WHERE planning_run_id = ?1",
            [run_id],
            |row| {
                Ok(PlanRevisionDto {
                    id: row.get(0)?,
                    revision: row.get(1)?,
                    edit_revision: row.get(2)?,
                    markdown_body: row.get(3)?,
                    approval_policy: row.get(4)?,
                    approval_status: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            },
        )
        .optional()?;
    Ok(PlanningStateDto {
        run,
        agents,
        events,
        artifacts,
        questions,
        plan,
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
    use std::collections::{HashMap, VecDeque};
    use std::fs;
    use std::sync::Mutex;

    use serde_json::json;
    use tempfile::{tempdir, TempDir};
    use uuid::Uuid;

    use super::{
        EnqueuePlanRequest, PlanApprovalRequest, PlanningAnswerInput, PlanningExecutor,
        PlanningService, ReplanWorkItemRequest, RetryPlanningRequest, StartPlanningRequest,
        SubmitPlanningAnswersRequest, UpdateSynthesizedPlanRequest,
    };
    use crate::copilot::{
        AgentEnvelope, AgentOutcome, AgentQuestion, AgentSession, CompletedPlannerArtifact,
        CopilotEvent, CopilotRunOutput, NormalizedRequirements,
    };
    use crate::error::AppError;
    use crate::state::AppStore;

    #[derive(Debug, Clone)]
    enum Call {
        Planner {
            ordinal: usize,
            model: String,
            session_name: String,
        },
        Synthesizer {
            session_name: String,
            artifacts: serde_json::Value,
        },
        Resume {
            session_name: String,
            prompt: String,
        },
    }

    struct FakeExecutor {
        planner_outputs: Mutex<HashMap<usize, VecDeque<Result<CopilotRunOutput, AppError>>>>,
        synthesizer_outputs: Mutex<VecDeque<Result<CopilotRunOutput, AppError>>>,
        resume_outputs: Mutex<VecDeque<Result<CopilotRunOutput, AppError>>>,
        calls: Mutex<Vec<Call>>,
    }

    impl FakeExecutor {
        fn new(
            planners: impl IntoIterator<Item = (usize, Vec<Result<CopilotRunOutput, AppError>>)>,
            synthesizers: Vec<Result<CopilotRunOutput, AppError>>,
            resumes: Vec<Result<CopilotRunOutput, AppError>>,
        ) -> Self {
            Self {
                planner_outputs: Mutex::new(
                    planners
                        .into_iter()
                        .map(|(ordinal, outputs)| (ordinal, outputs.into()))
                        .collect(),
                ),
                synthesizer_outputs: Mutex::new(synthesizers.into()),
                resume_outputs: Mutex::new(resumes.into()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().expect("calls").clone()
        }
    }

    impl PlanningExecutor for FakeExecutor {
        fn start_planner(
            &self,
            _repository_path: &str,
            model: &str,
            session: &AgentSession,
            _requirements: &NormalizedRequirements,
        ) -> Result<CopilotRunOutput, AppError> {
            self.calls.lock().expect("calls").push(Call::Planner {
                ordinal: session.ordinal,
                model: model.to_owned(),
                session_name: session.name.clone(),
            });
            self.planner_outputs
                .lock()
                .expect("planner outputs")
                .get_mut(&session.ordinal)
                .and_then(VecDeque::pop_front)
                .expect("configured planner output")
        }

        fn start_synthesizer(
            &self,
            _repository_path: &str,
            _model: &str,
            session: &AgentSession,
            _requirements: &NormalizedRequirements,
            artifacts: &[CompletedPlannerArtifact],
        ) -> Result<CopilotRunOutput, AppError> {
            self.calls.lock().expect("calls").push(Call::Synthesizer {
                session_name: session.name.clone(),
                artifacts: serde_json::to_value(artifacts).expect("serialize artifacts"),
            });
            self.synthesizer_outputs
                .lock()
                .expect("synthesizer outputs")
                .pop_front()
                .expect("configured synthesizer output")
        }

        fn resume_named(
            &self,
            _repository_path: &str,
            session_name: &str,
            prompt: &str,
        ) -> Result<CopilotRunOutput, AppError> {
            self.calls.lock().expect("calls").push(Call::Resume {
                session_name: session_name.to_owned(),
                prompt: prompt.to_owned(),
            });
            self.resume_outputs
                .lock()
                .expect("resume outputs")
                .pop_front()
                .expect("configured resume output")
        }
    }

    struct Harness {
        _directory: TempDir,
        store: AppStore,
        repository_path: String,
        work_item_id: String,
    }

    fn harness(models: &[&str]) -> Harness {
        let directory = tempdir().expect("temp directory");
        let repository = directory.path().join("repository");
        let data = directory.path().join("data");
        fs::create_dir_all(&repository).expect("repository directory");
        let store = AppStore::open(&data).expect("store");
        let repository_path = repository.to_string_lossy().into_owned();
        let work_item_id = "work-item".to_owned();
        store
            .with_connection(|connection| {
                connection.execute(
                    "INSERT INTO repositories (
                        id, root_path, display_name, created_at, updated_at
                     ) VALUES ('repository', ?1, 'repository', 'now', 'now')",
                    [&repository_path],
                )?;
                connection.execute(
                    "INSERT INTO work_items (
                        id, repository_id, title, source_kind, source_metadata_json,
                        markdown_body, lifecycle_status, created_at, updated_at
                     ) VALUES (
                        ?1, 'repository', 'Feature', 'inline_markdown',
                        '{\"kind\":\"inline_markdown\"}', '# Requirements',
                        'open', 'now', 'now'
                     )",
                    [&work_item_id],
                )?;
                connection.execute("DELETE FROM model_assignments WHERE role = 'planner'", [])?;
                for (ordinal, model) in models.iter().enumerate() {
                    connection.execute(
                        "INSERT INTO model_assignments (role, position, model_id)
                         VALUES ('planner', ?1, ?2)",
                        rusqlite::params![ordinal, model],
                    )?;
                }
                Ok(())
            })
            .expect("seed");
        Harness {
            _directory: directory,
            store,
            repository_path,
            work_item_id,
        }
    }

    fn completed(markdown: &str, event_marker: &str) -> CopilotRunOutput {
        CopilotRunOutput {
            envelope: AgentEnvelope {
                version: 1,
                outcome: AgentOutcome::Completed,
                questions: Vec::new(),
                markdown: Some(markdown.to_owned()),
                error: None,
            },
            events: vec![
                CopilotEvent {
                    sequence: 0,
                    kind: Some("started".to_owned()),
                    payload: json!({"marker": event_marker, "step": 0}),
                },
                CopilotEvent {
                    sequence: 1,
                    kind: Some("finished".to_owned()),
                    payload: json!({"marker": event_marker, "step": 1}),
                },
            ],
        }
    }

    fn needs_input(id: &str, prompt: &str) -> CopilotRunOutput {
        CopilotRunOutput {
            envelope: AgentEnvelope {
                version: 1,
                outcome: AgentOutcome::NeedsInput,
                questions: vec![AgentQuestion {
                    id: id.to_owned(),
                    prompt: prompt.to_owned(),
                }],
                markdown: None,
                error: None,
            },
            events: vec![CopilotEvent {
                sequence: 0,
                kind: Some("question".to_owned()),
                payload: json!({"questionId": id}),
            }],
        }
    }

    fn blocked(message: &str) -> CopilotRunOutput {
        CopilotRunOutput {
            envelope: AgentEnvelope {
                version: 1,
                outcome: AgentOutcome::Blocked,
                questions: Vec::new(),
                markdown: None,
                error: Some(message.to_owned()),
            },
            events: vec![CopilotEvent {
                sequence: 0,
                kind: Some("blocked".to_owned()),
                payload: json!({"message": message}),
            }],
        }
    }

    fn start_request(work_item_id: &str, key: &str) -> StartPlanningRequest {
        StartPlanningRequest {
            work_item_id: work_item_id.to_owned(),
            idempotency_key: key.to_owned(),
        }
    }

    #[test]
    fn isolates_two_planners_then_synthesizes_and_rejects_duplicate_start() {
        let harness = harness(&["model-a", "model-b"]);
        let sentinel = format!("{}/sentinel.txt", harness.repository_path);
        fs::write(&sentinel, "unchanged").expect("sentinel");
        let executor = FakeExecutor::new(
            [
                (0, vec![Ok(completed("# Candidate A", "planner-a"))]),
                (1, vec![Ok(completed("# Candidate B", "planner-b"))]),
            ],
            vec![Ok(completed("# Durable Plan", "synthesizer"))],
            Vec::new(),
        );
        let service = PlanningService::with_executor(&harness.store, &executor);

        let state = service
            .start(&start_request(&harness.work_item_id, "start-once"))
            .expect("planning succeeds");
        assert_eq!(state.run.status, "succeeded");
        assert_eq!(state.agents.len(), 3);
        assert_eq!(
            state
                .artifacts
                .iter()
                .filter(|artifact| artifact.artifact_kind == "planner_output")
                .count(),
            2
        );
        assert_eq!(state.events.len(), 6);
        assert_eq!(
            state.plan.as_ref().map(|plan| plan.markdown_body.as_str()),
            Some("# Durable Plan")
        );
        assert_eq!(
            fs::read_to_string(&sentinel).expect("sentinel"),
            "unchanged"
        );

        let calls = executor.calls();
        let planners = calls
            .iter()
            .filter_map(|call| match call {
                Call::Planner {
                    ordinal,
                    model,
                    session_name,
                } => Some((*ordinal, model.as_str(), session_name.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(planners.len(), 2);
        assert!(
            planners.contains(&(0, "model-a", planners[0].2))
                || planners.contains(&(0, "model-a", planners[1].2))
        );
        let synthesis = calls
            .iter()
            .find_map(|call| match call {
                Call::Synthesizer {
                    session_name,
                    artifacts,
                } => Some((session_name, artifacts)),
                _ => None,
            })
            .expect("synthesis call");
        assert!(synthesis.0.contains("synthesizer"));
        assert_eq!(synthesis.1.as_array().expect("artifact array").len(), 2);
        assert!(synthesis.1.to_string().contains("# Candidate A"));
        assert!(synthesis.1.to_string().contains("# Candidate B"));

        let repeated = service
            .start(&start_request(&harness.work_item_id, "start-once"))
            .expect_err("duplicate start");
        assert_eq!(repeated.code, "conflict");
        assert_eq!(executor.calls().len(), calls.len());
    }

    #[test]
    fn optional_approval_plan_is_immediately_queue_eligible() {
        let harness = harness(&["model-a", "model-b"]);
        harness
            .store
            .with_connection(|connection| {
                connection.execute(
                    "UPDATE work_items SET require_plan_approval = 0 WHERE id = ?1",
                    [&harness.work_item_id],
                )?;
                Ok(())
            })
            .expect("make approval optional");
        let executor = FakeExecutor::new(
            [
                (0, vec![Ok(completed("# Candidate A", "planner-a"))]),
                (1, vec![Ok(completed("# Candidate B", "planner-b"))]),
            ],
            vec![Ok(completed("# Optional Plan", "synthesizer"))],
            Vec::new(),
        );
        let service = PlanningService::with_executor(&harness.store, &executor);

        let state = service
            .start(&start_request(&harness.work_item_id, "optional-approval"))
            .expect("planning succeeds");
        let detail = service.detail(&state.run.id).expect("planning detail");

        assert_eq!(
            detail
                .plan
                .as_ref()
                .map(|plan| plan.approval_policy.as_str()),
            Some("not_required")
        );
        assert_eq!(detail.current_phase, "ready");
        assert!(detail.queue.eligible);
        let edited = service
            .update_plan(&UpdateSynthesizedPlanRequest {
                planning_run_id: state.run.id.clone(),
                expected_plan_updated_at: detail.plan.expect("plan").updated_at,
                markdown_body: "# Edited Optional Plan".to_owned(),
            })
            .expect("edit optional plan");
        assert!(
            service
                .detail(&edited.run.id)
                .expect("edited detail")
                .queue
                .eligible
        );
        service
            .enqueue_plan(&EnqueuePlanRequest {
                planning_run_id: state.run.id,
            })
            .expect("enqueue without approval");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn persists_planner_and_synthesizer_question_rounds_and_resumes_exact_sessions() {
        let harness = harness(&["model-a", "model-b"]);
        let executor = FakeExecutor::new(
            [
                (0, vec![Ok(needs_input("scope", "Which scope?"))]),
                (1, vec![Ok(completed("# Candidate B", "planner-b"))]),
            ],
            vec![Ok(needs_input("tradeoff", "Which tradeoff?"))],
            vec![
                Ok(completed("# Candidate", "planner-resume")),
                Ok(completed("# Final", "synth-resume")),
            ],
        );
        let service = PlanningService::with_executor(&harness.store, &executor);
        let first = service
            .start(&start_request(&harness.work_item_id, "questions"))
            .expect("question state");
        assert_eq!(first.run.status, "waiting_for_answers");
        let planner = first
            .agents
            .iter()
            .find(|agent| agent.role == "planner")
            .expect("planner");
        let planner_question = &first.questions[0];

        let second = service
            .submit_answers(&SubmitPlanningAnswersRequest {
                planning_run_id: first.run.id.clone(),
                planning_agent_id: planner.id.clone(),
                expected_run_updated_at: first.run.updated_at.clone(),
                answers: vec![PlanningAnswerInput {
                    question_id: planner_question.id.clone(),
                    answer_markdown: "Backend only".to_owned(),
                }],
            })
            .expect("planner answer");
        assert_eq!(second.run.status, "waiting_for_answers");
        assert_eq!(second.questions[0].status, "answered");
        assert_eq!(
            second.questions[0].answer_markdown.as_deref(),
            Some("Backend only")
        );
        let synthesizer = second
            .agents
            .iter()
            .find(|agent| agent.role == "synthesizer")
            .expect("synthesizer");
        let synthesis_question = second
            .questions
            .iter()
            .find(|question| question.status == "open")
            .expect("synthesis question");

        let duplicate = service
            .submit_answers(&SubmitPlanningAnswersRequest {
                planning_run_id: first.run.id.clone(),
                planning_agent_id: planner.id.clone(),
                expected_run_updated_at: first.run.updated_at,
                answers: vec![PlanningAnswerInput {
                    question_id: planner_question.id.clone(),
                    answer_markdown: "Backend only".to_owned(),
                }],
            })
            .expect_err("stale duplicate conflicts");
        assert_eq!(duplicate.code, "conflict");

        let final_state = service
            .submit_answers(&SubmitPlanningAnswersRequest {
                planning_run_id: second.run.id.clone(),
                planning_agent_id: synthesizer.id.clone(),
                expected_run_updated_at: second.run.updated_at,
                answers: vec![PlanningAnswerInput {
                    question_id: synthesis_question.id.clone(),
                    answer_markdown: "Favor safety".to_owned(),
                }],
            })
            .expect("synthesis answer");
        assert_eq!(final_state.run.status, "succeeded");
        assert_eq!(
            final_state
                .plan
                .as_ref()
                .map(|plan| plan.markdown_body.as_str()),
            Some("# Final")
        );

        let calls = executor.calls();
        let planner_session = calls
            .iter()
            .find_map(|call| match call {
                Call::Planner { session_name, .. } => Some(session_name),
                _ => None,
            })
            .expect("planner call");
        let synthesis_session = calls
            .iter()
            .find_map(|call| match call {
                Call::Synthesizer { session_name, .. } => Some(session_name),
                _ => None,
            })
            .expect("synthesis call");
        let resumes = calls
            .iter()
            .filter_map(|call| match call {
                Call::Resume {
                    session_name,
                    prompt,
                } => Some((session_name, prompt)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(resumes[0].0, planner_session);
        assert!(resumes[0].1.contains("Backend only"));
        assert_eq!(resumes[1].0, synthesis_session);
        assert!(resumes[1].1.contains("Favor safety"));
    }

    #[test]
    fn records_nonzero_or_malformed_failure_and_explicit_retry_without_duplicate_plan() {
        let harness = harness(&["model-a", "model-b"]);
        let executor = FakeExecutor::new(
            [
                (
                    0,
                    vec![Err(AppError::external(
                        "Copilot returned malformed JSONL after exit status 7.",
                    ))],
                ),
                (1, vec![Ok(completed("# Candidate B", "planner-b"))]),
            ],
            vec![Ok(completed("# Final", "synth"))],
            vec![Ok(completed("# Recovered candidate", "retry"))],
        );
        let service = PlanningService::with_executor(&harness.store, &executor);
        let failed = service
            .start(&start_request(&harness.work_item_id, "retry"))
            .expect("failure is durable state");
        assert_eq!(failed.run.status, "failed");
        assert_eq!(failed.run.error_code.as_deref(), Some("external"));
        let planner = failed
            .agents
            .iter()
            .find(|agent| agent.role == "planner")
            .expect("planner");
        assert_eq!(planner.attempt, 1);

        let recovered = service
            .retry(&RetryPlanningRequest {
                planning_run_id: failed.run.id.clone(),
                planning_agent_id: Some(planner.id.clone()),
                expected_run_updated_at: failed.run.updated_at,
            })
            .expect("retry succeeds");
        assert_eq!(recovered.run.status, "succeeded");
        assert_eq!(
            recovered
                .agents
                .iter()
                .find(|agent| agent.id == planner.id)
                .map(|agent| agent.attempt),
            Some(2)
        );
        assert_eq!(
            recovered
                .artifacts
                .iter()
                .filter(|artifact| artifact.artifact_kind == "synthesized_plan")
                .count(),
            1
        );
        assert_eq!(
            recovered.plan.as_ref().map(|plan| plan.edit_revision),
            Some(1)
        );

        let repeated = service
            .retry(&RetryPlanningRequest {
                planning_run_id: recovered.run.id.clone(),
                planning_agent_id: Some(planner.id.clone()),
                expected_run_updated_at: recovered.run.updated_at,
            })
            .expect_err("successful agent is not retryable");
        assert_eq!(repeated.code, "conflict");
    }

    #[test]
    fn exposes_cohesive_detail_and_guards_plan_approval_and_idempotent_enqueue() {
        let harness = harness(&["model-a", "model-b"]);
        let executor = FakeExecutor::new(
            [
                (0, vec![Ok(completed("# Candidate A", "planner-a"))]),
                (1, vec![Ok(completed("# Candidate B", "planner-b"))]),
            ],
            vec![Ok(completed("# Synthesized", "synthesizer"))],
            Vec::new(),
        );
        let service = PlanningService::with_executor(&harness.store, &executor);
        let started = service
            .start(&start_request(&harness.work_item_id, "detail"))
            .expect("planning");
        let detail = service.detail(&started.run.id).expect("detail");
        assert_eq!(detail.source.kind, "inline_markdown");
        assert_eq!(detail.current_phase, "approval");
        assert_eq!(detail.status, "pending");
        assert_eq!(detail.queue.state, "awaiting_approval");
        assert!(detail
            .agents
            .iter()
            .all(|agent| agent.session_name.starts_with("quorum-")));
        assert_eq!(detail.recent_events.len(), 6);

        let original_plan = started.plan.expect("plan");
        let updated = service
            .update_plan(&UpdateSynthesizedPlanRequest {
                planning_run_id: started.run.id.clone(),
                expected_plan_updated_at: original_plan.updated_at.clone(),
                markdown_body: "# Edited plan".to_owned(),
            })
            .expect("update");
        let updated_plan = updated.plan.expect("updated plan");
        assert_eq!(updated_plan.edit_revision, 2);
        assert_eq!(updated_plan.approval_status, "pending");
        let stale = service
            .update_plan(&UpdateSynthesizedPlanRequest {
                planning_run_id: started.run.id.clone(),
                expected_plan_updated_at: original_plan.updated_at,
                markdown_body: "# Another edit".to_owned(),
            })
            .expect_err("stale mutation");
        assert_eq!(stale.code, "conflict");

        let rejected = service
            .reject_plan(&PlanApprovalRequest {
                planning_run_id: started.run.id.clone(),
                expected_plan_updated_at: updated_plan.updated_at,
            })
            .expect("reject");
        let rejected_plan = rejected.plan.expect("rejected plan");
        assert_eq!(rejected_plan.approval_status, "rejected");
        let duplicate_reject = service
            .reject_plan(&PlanApprovalRequest {
                planning_run_id: started.run.id.clone(),
                expected_plan_updated_at: rejected_plan.updated_at.clone(),
            })
            .expect_err("duplicate rejection");
        assert_eq!(duplicate_reject.code, "conflict");

        let approved = service
            .approve_plan(&PlanApprovalRequest {
                planning_run_id: started.run.id.clone(),
                expected_plan_updated_at: rejected_plan.updated_at,
            })
            .expect("approve");
        let approved_plan = approved.plan.expect("approved plan");
        assert_eq!(approved_plan.approval_status, "approved");
        let eligible = service.detail(&started.run.id).expect("eligible detail");
        assert_eq!(eligible.current_phase, "ready");
        assert!(eligible.queue.eligible);

        let enqueue_request = EnqueuePlanRequest {
            planning_run_id: started.run.id.clone(),
        };
        service.enqueue_plan(&enqueue_request).expect("enqueue");
        service
            .enqueue_plan(&enqueue_request)
            .expect("idempotent enqueue");
        let queued = service.detail(&started.run.id).expect("queued detail");
        assert_eq!(queued.current_phase, "queue");
        assert_eq!(queued.queue.state, "queued");
        let queue_count: i64 = harness
            .store
            .with_connection(|connection| {
                connection
                    .query_row("SELECT count(*) FROM queue_entries", [], |row| row.get(0))
                    .map_err(Into::into)
            })
            .expect("queue count");
        assert_eq!(queue_count, 1);
    }

    #[test]
    fn replans_rejected_work_and_preserves_approved_work() {
        let harness = harness(&["model-a", "model-b"]);
        let executor = FakeExecutor::new(
            [
                (
                    0,
                    vec![
                        Ok(completed("# First candidate A", "planner-first-a")),
                        Ok(completed("# Second candidate A", "planner-second-a")),
                    ],
                ),
                (
                    1,
                    vec![
                        Ok(completed("# First candidate B", "planner-first-b")),
                        Ok(completed("# Second candidate B", "planner-second-b")),
                    ],
                ),
            ],
            vec![
                Ok(completed("# First plan", "synthesizer-first")),
                Ok(completed("# Replacement plan", "synthesizer-second")),
            ],
            Vec::new(),
        );
        let service = PlanningService::with_executor(&harness.store, &executor);
        let first = service
            .start(&start_request(&harness.work_item_id, "initial-plan"))
            .expect("initial planning");
        let first_plan = first.plan.expect("initial plan");
        let rejected = service
            .reject_plan(&PlanApprovalRequest {
                planning_run_id: first.run.id.clone(),
                expected_plan_updated_at: first_plan.updated_at,
            })
            .expect("reject initial plan");
        let rejected_plan = rejected.plan.expect("rejected plan");

        let replacement = service
            .replan(&ReplanWorkItemRequest {
                planning_run_id: first.run.id.clone(),
                expected_plan_updated_at: rejected_plan.updated_at,
                idempotency_key: "replacement-plan".to_owned(),
            })
            .expect("re-plan rejected work");
        let replacement_plan = replacement.plan.expect("replacement plan");
        assert_ne!(replacement.run.id, first.run.id);
        assert_eq!(replacement_plan.markdown_body, "# Replacement plan");
        assert_eq!(replacement_plan.approval_status, "pending");
        assert_eq!(replacement_plan.edit_revision, 1);
        let old_run_count: i64 = harness
            .store
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT count(*) FROM planning_runs WHERE id = ?1",
                        [&first.run.id],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })
            .expect("old run count");
        assert_eq!(old_run_count, 0);

        let approved = service
            .approve_plan(&PlanApprovalRequest {
                planning_run_id: replacement.run.id.clone(),
                expected_plan_updated_at: replacement_plan.updated_at,
            })
            .expect("approve replacement");
        let approved_plan = approved.plan.expect("approved replacement");
        let error = service
            .replan(&ReplanWorkItemRequest {
                planning_run_id: replacement.run.id,
                expected_plan_updated_at: approved_plan.updated_at,
                idempotency_key: "forbidden-replan".to_owned(),
            })
            .expect_err("approved plan must be preserved");
        assert_eq!(error.code, "conflict");
    }

    #[test]
    fn restart_recovery_blocks_unobservable_work_until_explicit_retry() {
        let harness = harness(&["model-a"]);
        let run_id = "persisted-run";
        let planner_id = Uuid::new_v4().to_string();
        let synthesizer_id = Uuid::new_v4().to_string();
        harness
            .store
            .with_connection(|connection| {
                connection.execute(
                    "INSERT INTO planning_runs (
                        id, work_item_id, status, idempotency_key, created_at, updated_at
                     ) VALUES (?1, ?2, 'running', 'restart', 'before', 'before')",
                    rusqlite::params![run_id, harness.work_item_id],
                )?;
                connection.execute(
                    "INSERT INTO planning_agents (
                        id, planning_run_id, role, ordinal, model_id, session_name,
                        status, attempt, started_at, created_at, updated_at
                     ) VALUES (?1, ?2, 'planner', 0, 'model-a', 'exact-planner-session',
                               'running', 1, 'before', 'before', 'before')",
                    rusqlite::params![planner_id, run_id],
                )?;
                connection.execute(
                    "INSERT INTO planning_agents (
                        id, planning_run_id, role, ordinal, model_id, session_name,
                        status, attempt, created_at, updated_at
                     ) VALUES (?1, ?2, 'synthesizer', 0, 'model-a', 'exact-synth-session',
                               'pending', 0, 'before', 'before')",
                    rusqlite::params![synthesizer_id, run_id],
                )?;
                connection.execute(
                    "INSERT INTO planning_questions (
                        id, planning_run_id, planning_agent_id, external_id, ordinal,
                        prompt_markdown, status, created_at, updated_at
                     ) VALUES (
                        'persisted-question', ?1, ?2, 'scope', 0,
                        'Which scope?', 'answered', 'before', 'before'
                     )",
                    rusqlite::params![run_id, planner_id],
                )?;
                connection.execute(
                    "INSERT INTO planning_answers (
                        id, question_id, answer_markdown, created_at, updated_at
                     ) VALUES (
                        'persisted-answer', 'persisted-question', 'Backend only',
                        'before', 'before'
                     )",
                    [],
                )?;
                Ok(())
            })
            .expect("seed interrupted run");
        let reopened = AppStore::open(
            harness
                .store
                .database_path()
                .parent()
                .expect("app data directory"),
        )
        .expect("reopen after interruption");
        let executor = FakeExecutor::new(
            [],
            vec![Ok(completed("# Final", "synth"))],
            vec![Ok(completed("# Candidate", "recovered"))],
        );
        let service = PlanningService::with_executor(&reopened, &executor);

        let recovered = service.get(run_id).expect("recover state");
        assert_eq!(recovered.run.status, "blocked");
        assert_eq!(recovered.run.error_code.as_deref(), Some("interrupted"));
        assert!(recovered.plan.is_none());
        let planner = recovered
            .agents
            .iter()
            .find(|agent| agent.id == planner_id)
            .expect("planner");
        assert_eq!(planner.status, "blocked");

        let completed = service
            .retry(&RetryPlanningRequest {
                planning_run_id: run_id.to_owned(),
                planning_agent_id: Some(planner_id),
                expected_run_updated_at: recovered.run.updated_at,
            })
            .expect("recover by retry");
        assert_eq!(completed.run.status, "succeeded");
        let calls = executor.calls();
        assert!(matches!(
            &calls[0],
            Call::Resume {
                session_name,
                prompt
            } if session_name == "exact-planner-session"
                && prompt.contains("Backend only")
        ));
    }

    #[test]
    fn restart_finalizes_a_persisted_synthesis_artifact_exactly_once() {
        let harness = harness(&["model-a"]);
        let planner_id = Uuid::new_v4().to_string();
        let synthesizer_id = Uuid::new_v4().to_string();
        harness
            .store
            .with_connection(|connection| {
                connection.execute_batch(&format!(
                    "INSERT INTO planning_runs (
                        id, work_item_id, status, idempotency_key, created_at, updated_at
                     ) VALUES ('post-synthesis', '{}', 'synthesizing', 'post-synthesis',
                               'before', 'before');
                     INSERT INTO planning_agents (
                        id, planning_run_id, role, ordinal, model_id, session_name,
                        status, attempt, created_at, updated_at, completed_at
                     ) VALUES (
                        '{planner_id}', 'post-synthesis', 'planner', 0, 'model-a',
                        'planner-session', 'succeeded', 1, 'before', 'before', 'before'
                     );
                     INSERT INTO planning_agents (
                        id, planning_run_id, role, ordinal, model_id, session_name,
                        status, attempt, created_at, updated_at, completed_at
                     ) VALUES (
                        '{synthesizer_id}', 'post-synthesis', 'synthesizer', 0, 'model-a',
                        'synth-session', 'succeeded', 1, 'before', 'before', 'before'
                     );
                     INSERT INTO planning_artifacts (
                        id, planning_run_id, planning_agent_id, artifact_kind,
                        markdown_body, attempt, sequence, created_at
                     ) VALUES (
                        'candidate', 'post-synthesis', '{planner_id}', 'planner_output',
                        '# Candidate', 1, 0, 'before'
                     );
                     INSERT INTO planning_artifacts (
                        id, planning_run_id, planning_agent_id, artifact_kind,
                        markdown_body, attempt, sequence, created_at
                     ) VALUES (
                        'synthesis', 'post-synthesis', '{synthesizer_id}', 'synthesized_plan',
                        '# Persisted Final', 1, 0, 'before'
                     );",
                    harness.work_item_id
                ))?;
                Ok(())
            })
            .expect("seed completed synthesis");
        let executor = FakeExecutor::new([], Vec::new(), Vec::new());
        let service = PlanningService::with_executor(&harness.store, &executor);

        let first = service.get("post-synthesis").expect("finalize plan");
        assert_eq!(first.run.status, "succeeded");
        assert_eq!(
            first.plan.as_ref().map(|plan| plan.markdown_body.as_str()),
            Some("# Persisted Final")
        );
        let second = service.get("post-synthesis").expect("idempotent finalize");
        assert_eq!(second.run.updated_at, first.run.updated_at);
        assert_eq!(second.plan.map(|plan| plan.edit_revision), Some(1));
        assert!(executor.calls().is_empty());
    }

    #[test]
    fn blocked_agent_is_persisted_and_retryable() {
        let harness = harness(&["model-a", "model-b"]);
        let executor = FakeExecutor::new(
            [
                (0, vec![Ok(blocked("Repository is unavailable"))]),
                (1, vec![Ok(completed("# Candidate B", "planner-b"))]),
            ],
            vec![Ok(completed("# Final", "synth"))],
            vec![Ok(completed("# Candidate", "retry"))],
        );
        let service = PlanningService::with_executor(&harness.store, &executor);
        let blocked_state = service
            .start(&start_request(&harness.work_item_id, "blocked"))
            .expect("blocked state");
        assert_eq!(blocked_state.run.status, "blocked");
        assert_eq!(
            blocked_state.run.error_message.as_deref(),
            Some("Repository is unavailable")
        );
        let planner = blocked_state
            .agents
            .iter()
            .find(|agent| agent.role == "planner")
            .expect("planner");
        let final_state = service
            .retry(&RetryPlanningRequest {
                planning_run_id: blocked_state.run.id.clone(),
                planning_agent_id: Some(planner.id.clone()),
                expected_run_updated_at: blocked_state.run.updated_at,
            })
            .expect("retry");
        assert_eq!(final_state.run.status, "succeeded");
    }
}
