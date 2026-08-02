mod error;
mod ipc;
mod repository;
mod state;

use std::sync::Arc;

use tauri::Manager;

use crate::state::AppStore;

pub struct StartupState {
    store: Result<Arc<AppStore>, error::AppError>,
}

impl StartupState {
    fn store(&self) -> Result<Arc<AppStore>, error::AppError> {
        self.store.clone()
    }
}

pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let store = app
                .path()
                .app_data_dir()
                .map_err(|error| {
                    error::AppError::path(format!(
                        "Unable to locate Quorum application data: {error}"
                    ))
                })
                .and_then(AppStore::open)
                .map(Arc::new);
            app.manage(StartupState { store });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::list_repositories,
            ipc::register_repository,
            ipc::archive_repository,
            ipc::list_work_items,
            ipc::create_work_item,
            ipc::get_work_item
        ]);

    if let Err(error) = builder.run(tauri::generate_context!()) {
        eprintln!("Quorum failed to start: {error}");
    }
}
