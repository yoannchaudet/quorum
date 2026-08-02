use std::sync::Arc;

use tauri::State;

use crate::error::AppError;
use crate::repository::{
    CreateWorkItemRequest, RegisterRepositoryRequest, RepositoryDto, RepositoryService, WorkItemDto,
};
use crate::settings::{
    discover_copilot_models, SettingsDto, SettingsService, UpdateSettingsRequest,
};
use crate::state::AppStore;
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

#[cfg(test)]
mod bindings_tests {
    use std::fs;

    use ts_rs::TS;

    use crate::error::AppError;
    use crate::repository::{
        CreateWorkItemRequest, RegisterRepositoryRequest, RepositoryDto, WorkItemDto,
    };
    use crate::settings::{SettingsDto, UpdateSettingsRequest};

    #[test]
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
    }

    fn assert_binding(path: &std::path::Path, generated: &str) {
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
