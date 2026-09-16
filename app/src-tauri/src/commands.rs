use crate::error::AppError;
use crate::process::{AppState, StatusDto};
use crate::settings::{
    load_or_create_settings, save_settings, validate_workspace_path, AccessMode,
};
use tauri::{AppHandle, State};

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
use tauri_plugin_autostart::ManagerExt;

#[tauri::command]
pub fn get_status(app: AppHandle, state: State<'_, AppState>) -> Result<StatusDto, AppError> {
    state.request_autostart_if_configured()?;
    state.snapshot(autostart_enabled(&app))
}

#[tauri::command]
pub fn choose_workspace_folder(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<StatusDto>, AppError> {
    let Some(path) = rfd::FileDialog::new().pick_folder() else {
        return Ok(None);
    };
    let canonical = validate_workspace_path(&path.to_string_lossy())?;
    let mut settings = load_or_create_settings(&state.paths)?;
    settings.workspace_path = Some(canonical);
    save_settings(&state.paths, &settings)?;
    sync_services_after_settings_change(&state)?;
    Ok(Some(state.snapshot(autostart_enabled(&app))?))
}

#[tauri::command]
pub fn open_url(url: String) -> Result<(), AppError> {
    crate::process::open_in_browser(&url)
}

#[tauri::command]
pub fn set_workspace_path(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<StatusDto, AppError> {
    let canonical = validate_workspace_path(&path)?;
    let mut settings = load_or_create_settings(&state.paths)?;
    settings.workspace_path = Some(canonical);
    save_settings(&state.paths, &settings)?;
    sync_services_after_settings_change(&state)?;
    state.snapshot(autostart_enabled(&app))
}

#[tauri::command]
pub fn set_access_mode(
    app: AppHandle,
    state: State<'_, AppState>,
    mode: String,
) -> Result<StatusDto, AppError> {
    let access_mode = AccessMode::parse(&mode)?;
    let mut settings = load_or_create_settings(&state.paths)?;
    settings.access_mode = access_mode;
    save_settings(&state.paths, &settings)?;
    sync_services_after_settings_change(&state)?;
    state.snapshot(autostart_enabled(&app))
}

#[tauri::command]
pub fn set_autostart(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<StatusDto, AppError> {
    set_autostart_enabled(&app, enabled)?;
    if enabled {
        let _ = state.start_if_configured()?;
    }
    state.snapshot(autostart_enabled(&app))
}

#[tauri::command]
pub fn regenerate_mcp_url(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<StatusDto, AppError> {
    state.regenerate_mcp_url()?;
    state.snapshot(autostart_enabled(&app))
}

#[tauri::command]
pub fn enable_zrok(
    app: AppHandle,
    state: State<'_, AppState>,
    token: String,
) -> Result<StatusDto, AppError> {
    state.enable_zrok(token)?;
    let _ = state.start_if_configured()?;
    state.snapshot(autostart_enabled(&app))
}

#[tauri::command]
pub fn start_services(app: AppHandle, state: State<'_, AppState>) -> Result<StatusDto, AppError> {
    state.start()?;
    state.snapshot(autostart_enabled(&app))
}

#[tauri::command]
pub fn stop_services(app: AppHandle, state: State<'_, AppState>) -> Result<StatusDto, AppError> {
    state.stop()?;
    state.snapshot(autostart_enabled(&app))
}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
fn autostart_enabled(app: &AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
fn autostart_enabled(_app: &AppHandle) -> bool {
    false
}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
fn set_autostart_enabled(app: &AppHandle, enabled: bool) -> Result<(), AppError> {
    let manager = app.autolaunch();
    if enabled {
        manager
            .enable()
            .map_err(|error| AppError::new("autostart", error.to_string()))
    } else {
        manager
            .disable()
            .map_err(|error| AppError::new("autostart", error.to_string()))
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
fn set_autostart_enabled(_app: &AppHandle, _enabled: bool) -> Result<(), AppError> {
    Err(AppError::new(
        "autostart_unavailable",
        "Autostart is unavailable on this platform.",
    ))
}

fn sync_services_after_settings_change(state: &AppState) -> Result<(), AppError> {
    if state.is_running()? {
        let _ = state.restart_if_configured()?;
    } else {
        let _ = state.start_if_configured()?;
    }
    Ok(())
}
