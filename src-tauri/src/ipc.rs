use std::sync::Arc;

use tauri::State;

use crate::copilot::SystemProcessRunner;
use crate::error::AppError;
use crate::planning::{
    EnqueuePlanRequest, PlanApprovalRequest, PlanningDetailDto, PlanningService,
    ReplanWorkItemRequest, RetryPlanningRequest, StartPlanningRequest,
    SubmitPlanningAnswersRequest, SystemPlanningExecutor, UpdateSynthesizedPlanRequest,
};
use crate::repository::{
    CreateWorkItemRequest, IntakeGithubIssueRequest, IntakeLocalMarkdownRequest,
    RegisterRepositoryRequest, RepositoryDto, RepositoryService, WorkItemDto,
};
use crate::settings::{
    discover_copilot_models, SettingsDto, SettingsService, UpdateSettingsRequest,
};
use crate::state::AppStore;
use crate::terminal::{
    LaunchTerminalHandoffRequest, OpenCopilotSessionRequest, ResumeTerminalHandoffRequest,
    TerminalHandoffService, TerminalLauncher,
};
use crate::StartupState;

fn service(store: &Arc<AppStore>) -> RepositoryService<'_> {
    RepositoryService::new(store)
}

fn settings_service(store: &Arc<AppStore>) -> SettingsService<'_> {
    SettingsService::new(store)
}

async fn blocking<T: Send + 'static>(
    store: Arc<AppStore>,
    operation: impl FnOnce(Arc<AppStore>) -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    tauri::async_runtime::spawn_blocking(move || operation(store))
        .await
        .map_err(|error| {
            AppError::database(format!(
                "Quorum's background task stopped unexpectedly: {error}"
            ))
        })?
}

#[tauri::command(rename_all = "camelCase")]
pub async fn list_repositories(
    state: State<'_, StartupState>,
) -> Result<Vec<RepositoryDto>, AppError> {
    let store = state.store()?;
    blocking(store, |store| service(&store).list_active()).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn register_repository(
    state: State<'_, StartupState>,
    request: RegisterRepositoryRequest,
) -> Result<RepositoryDto, AppError> {
    blocking(state.store()?, move |store| {
        service(&store).register(&request)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn archive_repository(
    state: State<'_, StartupState>,
    repository_id: String,
) -> Result<(), AppError> {
    blocking(state.store()?, move |store| {
        service(&store).archive(&repository_id)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn list_work_items(
    state: State<'_, StartupState>,
    repository_id: String,
) -> Result<Vec<WorkItemDto>, AppError> {
    blocking(state.store()?, move |store| {
        service(&store).list_work_items(&repository_id)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn create_work_item(
    state: State<'_, StartupState>,
    request: CreateWorkItemRequest,
) -> Result<WorkItemDto, AppError> {
    blocking(state.store()?, move |store| {
        service(&store).create_work_item(request)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn intake_inline_markdown(
    state: State<'_, StartupState>,
    request: CreateWorkItemRequest,
) -> Result<WorkItemDto, AppError> {
    blocking(state.store()?, move |store| {
        service(&store).create_work_item(request)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn intake_local_markdown(
    state: State<'_, StartupState>,
    request: IntakeLocalMarkdownRequest,
) -> Result<WorkItemDto, AppError> {
    blocking(state.store()?, move |store| {
        service(&store).intake_local_markdown(request)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn intake_github_issue(
    state: State<'_, StartupState>,
    request: IntakeGithubIssueRequest,
) -> Result<WorkItemDto, AppError> {
    blocking(state.store()?, move |store| {
        service(&store).intake_github_issue(request)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_work_item(
    state: State<'_, StartupState>,
    work_item_id: String,
) -> Result<WorkItemDto, AppError> {
    blocking(state.store()?, move |store| {
        service(&store).get_work_item(&work_item_id)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_settings(state: State<'_, StartupState>) -> Result<SettingsDto, AppError> {
    let store = state.store()?;
    blocking(store, |store| settings_service(&store).get()).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn update_settings(
    state: State<'_, StartupState>,
    request: UpdateSettingsRequest,
) -> Result<SettingsDto, AppError> {
    blocking(state.store()?, move |store| {
        settings_service(&store).update(request)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn list_copilot_models() -> Result<Vec<String>, AppError> {
    tauri::async_runtime::spawn_blocking(discover_copilot_models)
        .await
        .map_err(|error| {
            AppError::external(format!(
                "Copilot CLI model discovery stopped unexpectedly: {error}"
            ))
        })?
}

#[tauri::command(rename_all = "camelCase")]
pub async fn start_planning(
    state: State<'_, StartupState>,
    request: StartPlanningRequest,
) -> Result<PlanningDetailDto, AppError> {
    blocking(state.store()?, move |store| {
        let executor = SystemPlanningExecutor::default();
        let planning = PlanningService::with_executor(&store, &executor);
        let state = planning.start(&request)?;
        planning.detail(&state.run.id)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn replan_work_item(
    state: State<'_, StartupState>,
    request: ReplanWorkItemRequest,
) -> Result<PlanningDetailDto, AppError> {
    blocking(state.store()?, move |store| {
        let executor = SystemPlanningExecutor::default();
        let planning = PlanningService::with_executor(&store, &executor);
        let state = planning.replan(&request)?;
        planning.detail(&state.run.id)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_planning(
    state: State<'_, StartupState>,
    work_item_id: String,
) -> Result<PlanningDetailDto, AppError> {
    blocking(state.store()?, move |store| {
        let executor = SystemPlanningExecutor::default();
        let planning = PlanningService::with_executor(&store, &executor);
        let state = planning.latest_for_work_item(&work_item_id)?;
        planning.detail(&state.run.id)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn submit_planning_answers(
    state: State<'_, StartupState>,
    request: SubmitPlanningAnswersRequest,
) -> Result<PlanningDetailDto, AppError> {
    blocking(state.store()?, move |store| {
        let executor = SystemPlanningExecutor::default();
        let planning = PlanningService::with_executor(&store, &executor);
        let state = planning.submit_answers(&request)?;
        planning.detail(&state.run.id)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn retry_planning_agent(
    state: State<'_, StartupState>,
    request: RetryPlanningRequest,
) -> Result<PlanningDetailDto, AppError> {
    blocking(state.store()?, move |store| {
        let executor = SystemPlanningExecutor::default();
        let planning = PlanningService::with_executor(&store, &executor);
        let state = planning.retry(&request)?;
        planning.detail(&state.run.id)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn open_planning_terminal(
    state: State<'_, StartupState>,
    request: LaunchTerminalHandoffRequest,
) -> Result<PlanningDetailDto, AppError> {
    blocking(state.store()?, move |store| {
        let executor = SystemPlanningExecutor::default();
        let launcher = TerminalLauncher::new(SystemProcessRunner);
        let handoff = TerminalHandoffService::with_dependencies(&store, &launcher, &executor)
            .launch(&request)?;
        PlanningService::with_executor(&store, &executor).detail(&handoff.planning_run_id)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn open_copilot_session(
    state: State<'_, StartupState>,
    request: OpenCopilotSessionRequest,
) -> Result<(), AppError> {
    blocking(state.store()?, move |store| {
        let executor = SystemPlanningExecutor::default();
        let launcher = TerminalLauncher::new(SystemProcessRunner);
        TerminalHandoffService::with_dependencies(&store, &launcher, &executor)
            .open_session(&request)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn reconcile_planning_terminal(
    state: State<'_, StartupState>,
    request: ResumeTerminalHandoffRequest,
) -> Result<PlanningDetailDto, AppError> {
    blocking(state.store()?, move |store| {
        let executor = SystemPlanningExecutor::default();
        let launcher = TerminalLauncher::new(SystemProcessRunner);
        let handoff = TerminalHandoffService::with_dependencies(&store, &launcher, &executor)
            .resume_and_reconcile(&request)?;
        PlanningService::with_executor(&store, &executor).detail(&handoff.planning_run_id)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn update_synthesized_plan(
    state: State<'_, StartupState>,
    request: UpdateSynthesizedPlanRequest,
) -> Result<PlanningDetailDto, AppError> {
    blocking(state.store()?, move |store| {
        let executor = SystemPlanningExecutor::default();
        let planning = PlanningService::with_executor(&store, &executor);
        let state = planning.update_plan(&request)?;
        planning.detail(&state.run.id)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn approve_plan(
    state: State<'_, StartupState>,
    request: PlanApprovalRequest,
) -> Result<PlanningDetailDto, AppError> {
    blocking(state.store()?, move |store| {
        let executor = SystemPlanningExecutor::default();
        let planning = PlanningService::with_executor(&store, &executor);
        let state = planning.approve_plan(&request)?;
        planning.detail(&state.run.id)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn reject_plan(
    state: State<'_, StartupState>,
    request: PlanApprovalRequest,
) -> Result<PlanningDetailDto, AppError> {
    blocking(state.store()?, move |store| {
        let executor = SystemPlanningExecutor::default();
        let planning = PlanningService::with_executor(&store, &executor);
        let state = planning.reject_plan(&request)?;
        planning.detail(&state.run.id)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn enqueue_plan(
    state: State<'_, StartupState>,
    request: EnqueuePlanRequest,
) -> Result<PlanningDetailDto, AppError> {
    blocking(state.store()?, move |store| {
        let executor = SystemPlanningExecutor::default();
        let planning = PlanningService::with_executor(&store, &executor);
        let state = planning.enqueue_plan(&request)?;
        planning.detail(&state.run.id)
    })
    .await
}

#[cfg(test)]
mod bindings_tests {
    use std::fs;

    use ts_rs::TS;

    use crate::error::AppError;
    use crate::planning::{
        EnqueuePlanRequest, PlanApprovalRequest, PlanRevisionDto, PlanningAgentDto,
        PlanningAnswerInput, PlanningDetailDto, PlanningEventDto, PlanningQuestionDto,
        PlanningQueueDto, PlanningRunDto, PlanningSourceDto, QueueEntryDto, ReplanWorkItemRequest,
        RetryPlanningRequest, StartPlanningRequest, SubmitPlanningAnswersRequest,
        TerminalHandoffSummaryDto, UpdateSynthesizedPlanRequest,
    };
    use crate::repository::{
        CreateWorkItemRequest, IntakeGithubIssueRequest, IntakeLocalMarkdownRequest,
        RegisterRepositoryRequest, RepositoryDto, WorkItemDto,
    };
    use crate::settings::{SettingsDto, UpdateSettingsRequest};
    use crate::terminal::{
        LaunchTerminalHandoffRequest, OpenCopilotSessionRequest, ResumeTerminalHandoffRequest,
    };

    #[test]
    #[allow(clippy::too_many_lines)]
    fn bindings_remain_current() {
        let bindings = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bindings");
        assert_binding(
            &bindings.join("AppError.ts"),
            &AppError::export_to_string().expect("AppError binding"),
        );
        assert_binding(
            &bindings.join("SettingsDto.ts"),
            &SettingsDto::export_to_string().expect("settings binding"),
        );
        assert_binding(
            &bindings.join("UpdateSettingsRequest.ts"),
            &UpdateSettingsRequest::export_to_string().expect("update settings binding"),
        );
        assert_binding(
            &bindings.join("RepositoryDto.ts"),
            &RepositoryDto::export_to_string().expect("RepositoryDto binding"),
        );
        assert_binding(
            &bindings.join("WorkItemDto.ts"),
            &WorkItemDto::export_to_string().expect("WorkItemDto binding"),
        );
        assert_binding(
            &bindings.join("RegisterRepositoryRequest.ts"),
            &RegisterRepositoryRequest::export_to_string().expect("register request binding"),
        );
        assert_binding(
            &bindings.join("CreateWorkItemRequest.ts"),
            &CreateWorkItemRequest::export_to_string().expect("work item request binding"),
        );
        assert_binding(
            &bindings.join("IntakeLocalMarkdownRequest.ts"),
            &IntakeLocalMarkdownRequest::export_to_string().expect("local intake binding"),
        );
        assert_binding(
            &bindings.join("IntakeGithubIssueRequest.ts"),
            &IntakeGithubIssueRequest::export_to_string().expect("GitHub intake binding"),
        );
        assert_binding(
            &bindings.join("StartPlanningRequest.ts"),
            &StartPlanningRequest::export_to_string().expect("start planning binding"),
        );
        assert_binding(
            &bindings.join("ReplanWorkItemRequest.ts"),
            &ReplanWorkItemRequest::export_to_string().expect("re-plan binding"),
        );
        assert_binding(
            &bindings.join("PlanningAnswerInput.ts"),
            &PlanningAnswerInput::export_to_string().expect("planning answer binding"),
        );
        assert_binding(
            &bindings.join("SubmitPlanningAnswersRequest.ts"),
            &SubmitPlanningAnswersRequest::export_to_string().expect("submit answers binding"),
        );
        assert_binding(
            &bindings.join("RetryPlanningRequest.ts"),
            &RetryPlanningRequest::export_to_string().expect("retry planning binding"),
        );
        assert_binding(
            &bindings.join("UpdateSynthesizedPlanRequest.ts"),
            &UpdateSynthesizedPlanRequest::export_to_string().expect("update plan binding"),
        );
        assert_binding(
            &bindings.join("PlanApprovalRequest.ts"),
            &PlanApprovalRequest::export_to_string().expect("approval binding"),
        );
        assert_binding(
            &bindings.join("EnqueuePlanRequest.ts"),
            &EnqueuePlanRequest::export_to_string().expect("enqueue binding"),
        );
        assert_binding(
            &bindings.join("LaunchTerminalHandoffRequest.ts"),
            &LaunchTerminalHandoffRequest::export_to_string().expect("terminal launch binding"),
        );
        assert_binding(
            &bindings.join("OpenCopilotSessionRequest.ts"),
            &OpenCopilotSessionRequest::export_to_string().expect("open session binding"),
        );
        assert_binding(
            &bindings.join("ResumeTerminalHandoffRequest.ts"),
            &ResumeTerminalHandoffRequest::export_to_string().expect("terminal resume binding"),
        );
        assert_binding(
            &bindings.join("PlanningRunDto.ts"),
            &PlanningRunDto::export_to_string().expect("planning run binding"),
        );
        assert_binding(
            &bindings.join("PlanningAgentDto.ts"),
            &PlanningAgentDto::export_to_string().expect("planning agent binding"),
        );
        assert_binding(
            &bindings.join("PlanningEventDto.ts"),
            &PlanningEventDto::export_to_string().expect("planning event binding"),
        );
        assert_binding(
            &bindings.join("PlanningQuestionDto.ts"),
            &PlanningQuestionDto::export_to_string().expect("planning question binding"),
        );
        assert_binding(
            &bindings.join("PlanRevisionDto.ts"),
            &PlanRevisionDto::export_to_string().expect("plan revision binding"),
        );
        assert_binding(
            &bindings.join("PlanningSourceDto.ts"),
            &PlanningSourceDto::export_to_string().expect("planning source binding"),
        );
        assert_binding(
            &bindings.join("QueueEntryDto.ts"),
            &QueueEntryDto::export_to_string().expect("queue entry binding"),
        );
        assert_binding(
            &bindings.join("PlanningQueueDto.ts"),
            &PlanningQueueDto::export_to_string().expect("planning queue binding"),
        );
        assert_binding(
            &bindings.join("TerminalHandoffSummaryDto.ts"),
            &TerminalHandoffSummaryDto::export_to_string().expect("terminal summary binding"),
        );
        assert_binding(
            &bindings.join("PlanningDetailDto.ts"),
            &PlanningDetailDto::export_to_string().expect("planning detail binding"),
        );
    }

    fn assert_binding(path: &std::path::Path, generated: &str) {
        if std::env::var_os("UPDATE_BINDINGS").is_some() {
            fs::write(path, generated)
                .unwrap_or_else(|error| panic!("could not update {}: {error}", path.display()));
        }
        let checked_in = fs::read_to_string(path).unwrap_or_else(|error| {
            panic!("missing checked-in binding {}: {error}", path.display())
        });
        assert_eq!(
            checked_in,
            generated,
            "{} is stale; regenerate it from its Rust DTO",
            path.display()
        );
    }
}
