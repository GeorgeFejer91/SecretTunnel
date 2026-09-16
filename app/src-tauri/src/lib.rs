mod commands;
mod diag;
mod error;
mod process;
mod settings;

use process::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let paths = settings::app_paths().expect("failed to resolve app data paths");

    // Non-mutating preflight / profile-report mode: when
    // SECRET_TUNNEL_PROFILE_REPORT is set, write a JSON report describing the
    // resolved profile (proving SECRET_TUNNEL_CONFIG_DIR isolation works), then
    // exit before touching settings, services, or the UI. The smoke harness uses
    // this to verify it is about to run against an isolated, honoring binary.
    if let Some(report_path) = std::env::var_os("SECRET_TUNNEL_PROFILE_REPORT") {
        write_profile_report(&paths, &report_path);
        return;
    }

    let launch_environment = settings::apply_launch_environment_overrides(&paths)
        .ok()
        .flatten();
    let app_state = AppState::with_launch_environment(paths, launch_environment);
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

fn write_profile_report(paths: &settings::AppPaths, report_path: &std::ffi::OsStr) {
    use serde_json::json;

    let report = json!({
        "binary": "secret-tunnel",
        "identity": diag::build_identity(),
        "fingerprint": diag::executable_fingerprint(),
        "pid": std::process::id(),
        "profileDir": paths.config_dir,
        "settingsPath": paths.settings_path,
        "managedConfigPath": paths.managed_config_path,
        "diagnosticsPath": paths.diagnostics_path,
        "isolatedProfile": std::env::var_os("SECRET_TUNNEL_CONFIG_DIR").is_some(),
    });

    if let Some(parent) = std::path::Path::new(report_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        report_path,
        serde_json::to_vec_pretty(&report).unwrap_or_default(),
    );
}

fn launched_in_background() -> bool {
    std::env::args().any(|arg| arg == "--background")
}