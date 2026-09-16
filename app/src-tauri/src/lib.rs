mod commands;
mod error;
mod process;
mod settings;

use process::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let paths = settings::app_paths().expect("failed to resolve app data paths");
    let launch_override_result = settings::apply_launch_environment_overrides(&paths);
    let app_state = AppState::new(paths);
    let startup_state = app_state.clone();

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(app_state)
        .setup(move |app| {
            if let Ok(resource_dir) = app.path().resource_dir() {
                std::env::set_var("SECRET_TUNNEL_RESOURCE_DIR", resource_dir);
            }
            if launched_in_background() {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.minimize();
                }
            }
            std::thread::spawn({
                let state = startup_state.clone();
                move || {
                    match launch_override_result {
                        Ok(true) => state.push_app_log("Applied launch environment settings."),
                        Ok(false) => {}
                        Err(error) => {
                            state.push_app_log(format!(
                                "Launch environment settings ignored: {}",
                                error.message
                            ));
                        }
                    }
                    if let Err(error) = state.enable_zrok_from_environment_if_present() {
                        state.push_app_log(format!("zrok auto-enable failed: {}", error.message));
                    }
                    let _ = state.start_if_configured();
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::choose_workspace_folder,
            commands::set_workspace_path,
            commands::set_access_mode,
            commands::set_autostart,
            commands::enable_zrok,
            commands::open_url,
            commands::regenerate_mcp_url,
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

fn launched_in_background() -> bool {
    std::env::args().any(|arg| arg == "--background")
}
