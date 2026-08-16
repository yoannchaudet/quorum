//! Tauri command surface for the Quorum UX.
//!
//! Per `docs/frontend.md` this layer holds no business logic: every command is a thin
//! passthrough to `quorum-core`, which owns reading, validating, and writing
//! `~/.quorum/config.yaml` as well as enumerating the models the local `copilot` CLI
//! can run.

use quorum_core::config::Config;
use tauri::async_runtime::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Emitter, Runtime};

/// Menu id and frontend event name for opening the settings pane.
const SETTINGS_ID: &str = "settings";
const SETTINGS_EVENT: &str = "open-settings";

/// The model list costs a `copilot` CLI subprocess, so enumerate it once per run and
/// serve every later Settings open from memory. The lock is held across the lookup so
/// concurrent callers wait for one subprocess rather than each spawning their own.
/// Failures are not cached: the CLI may simply not have been installed yet.
static MODELS_CACHE: Mutex<Option<Vec<String>>> = Mutex::const_new(None);

// Commands are `async` so Tauri runs them off the main thread; a synchronous command
// would block the webview — and with it the whole UI — for the duration of the call.
#[tauri::command]
async fn read_config() -> Result<Config, String> {
    Config::load_default().map_err(|error| error.to_string())
}

#[tauri::command]
async fn write_config(config: Config) -> Result<(), String> {
    config.save_default().map_err(|error| error.to_string())
}

#[tauri::command]
async fn available_models() -> Result<Vec<String>, String> {
    let mut cache = MODELS_CACHE.lock().await;
    if let Some(cached) = cache.as_ref() {
        return Ok(cached.clone());
    }
    let models = tauri::async_runtime::spawn_blocking(quorum_core::available_models)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    *cache = Some(models.clone());
    Ok(models)
}

/// Add a Settings item bound to `Cmd/Ctrl+,` to the platform's conventional menu:
/// the application submenu on macOS, the Edit submenu elsewhere (where `Menu::default`
/// puts no application submenu at all).
fn install_settings_item<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let settings = MenuItem::with_id(app, SETTINGS_ID, "Settings…", true, Some("CmdOrCtrl+,"))?;
    let menu = Menu::default(app)?;
    let items = menu.items()?;
    let submenus = || items.iter().filter_map(|item| item.as_submenu());

    #[cfg(target_os = "macos")]
    // The application submenu comes first and opens with About; Settings sits under it.
    let target: Option<(&Submenu<R>, Option<usize>)> = submenus().next().map(|menu| (menu, Some(1)));

    #[cfg(not(target_os = "macos"))]
    let target: Option<(&Submenu<R>, Option<usize>)> = submenus()
        .find(|menu| menu.text().map(|text| text == "Edit").unwrap_or(false))
        .map(|menu| (menu, None));

    match target {
        Some((submenu, Some(index))) => submenu.insert(&settings, index)?,
        Some((submenu, None)) => {
            submenu.append(&PredefinedMenuItem::separator(app)?)?;
            submenu.append(&settings)?;
        }
        // Never expected, but a missing submenu must not cost the user the item.
        None => menu.append(&settings)?,
    }

    app.set_menu(menu)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            install_settings_item(app.handle())?;
            Ok(())
        })
        .on_menu_event(|app, event| {
            if event.id() == SETTINGS_ID {
                let _ = app.emit(SETTINGS_EVENT, ());
            }
        })
        .invoke_handler(tauri::generate_handler![
            read_config,
            write_config,
            available_models
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
