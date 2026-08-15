//! Tauri command surface for the Quorum UX.
//!
//! Per `docs/frontend.md` this layer holds no business logic: every command is a thin
//! passthrough to `quorum-core`, which owns reading, validating, and writing
//! `~/.quorum/config.yaml` as well as enumerating the models the local `copilot` CLI
//! can run.

use quorum_core::config::Config;

#[tauri::command]
fn read_config() -> Result<Config, String> {
    Config::load_default().map_err(|error| error.to_string())
}

#[tauri::command]
fn write_config(config: Config) -> Result<(), String> {
    config.save_default().map_err(|error| error.to_string())
}

#[tauri::command]
fn available_models() -> Result<Vec<String>, String> {
    quorum_core::available_models().map_err(|error| error.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            read_config,
            write_config,
            available_models
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
