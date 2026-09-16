mod commands;
mod error;
mod process;
mod settings;

use process::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let paths = settings::app_paths().expect("failed to resolve app data paths");
    let app_state = AppState::new(paths);

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::choose_workspace_folder,
            commands::set_workspace_path,
            commands::set_access_mode,
            commands::set_autostart,
            commands::start_services,
            commands::stop_services
        ])
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                let state = window.state::<AppState>();
                state.stop_all();
            }
        });

    #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
    let builder = builder.plugin(tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        Some(vec!["--background"]),
    ));

    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
