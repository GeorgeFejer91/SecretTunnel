use crate::error::AppError;
use crate::settings::{
    fresh_public_path_token, fresh_zrok_name, load_or_create_settings, mcp_url,
    normalize_windows_verbatim_prefix, save_settings, validate_zrok_name, write_managed_mcp_config,
    AccessMode, AppPaths, Settings,
};
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LOCAL_PORT: &str = "8787";
const MAX_LOG_LINES: usize = 160;
const AUTO_START_RETRY_AFTER: Duration = Duration::from_secs(20);

const INSTANCE_ID_ENV: &str = "SECRET_TUNNEL_INSTANCE_ID";
const ASSET_CLASS_ENV: &str = "SECRET_TUNNEL_ASSET_CLASS";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetClass {
    McpNode,
    ZrokShare,
}

impl AssetClass {
    fn tag(self) -> &'static str {
        match self {
            AssetClass::McpNode => "mcp-node",
            AssetClass::ZrokShare => "zrok-share",
        }
    }
}

fn fresh_instance_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let pid = std::process::id();
    format!("{pid:x}-{timestamp:x}")
}

#[derive(Clone)]
pub struct AppState {
    pub paths: AppPaths,
    runtime: Arc<Mutex<RuntimeState>>,
    status_probe_path: Option<PathBuf>,
    instance_id: String,
}

impl AppState {
    pub fn new(paths: AppPaths) -> Self {
        Self {
            paths,
            runtime: Arc::new(Mutex::new(RuntimeState::default())),
            status_probe_path: status_probe_path_from_environment(),
            instance_id: fresh_instance_id(),
        }
    }

    pub fn snapshot(&self, autostart_enabled: bool) -> Result<StatusDto, AppError> {
        let settings = load_or_create_settings(&self.paths)?;
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| AppError::new("runtime_lock", "Runtime state is unavailable."))?;
        runtime.cleanup_exited();
        Ok(StatusDto {
            settings: SettingsDto::from_settings(&settings, &self.paths),
            mcp_url: mcp_url(&settings),
            running: runtime.is_running(),
            autostart_enabled,
            zrok_installed: bundled_executable("zrok2").is_some(),
            zrok_enabled: zrok_environment_enabled(),
            gpt_repo_mcp_found: mcp_runtime_exists(&settings),
            logs: runtime.logs.clone(),
        })
    }

    pub fn start(&self) -> Result<(), AppError> {
        let settings = load_or_create_settings(&self.paths)?;
        self.start_with_settings(settings)
    }

    pub fn start_if_configured(&self) -> Result<bool, AppError> {
        let settings = load_or_create_settings(&self.paths)?;
        if settings.workspace_path.is_none() {
            return Ok(false);
        }
        self.start_with_settings(settings)?;
        Ok(true)
    }

    pub fn request_autostart_if_configured(&self) -> Result<(), AppError> {
        let settings = load_or_create_settings(&self.paths)?;
        if settings.workspace_path.is_none() {
            return Ok(());
        }

        let now = Instant::now();
        {
            let mut runtime = self
                .runtime
                .lock()
                .map_err(|_| AppError::new("runtime_lock", "Runtime state is unavailable."))?;
            runtime.cleanup_exited();
            if runtime.is_running() || runtime.starting {
                return Ok(());
            }
            if runtime
                .last_autostart_attempt
                .is_some_and(|last| now.duration_since(last) < AUTO_START_RETRY_AFTER)
            {
                return Ok(());
            }
            runtime.last_autostart_attempt = Some(now);
        }

        let state = self.clone();
        thread::spawn(move || {
            if let Err(error) = state.start_with_settings(settings) {
                if let Ok(mut runtime) = state.runtime.lock() {
                    runtime.push_log("app", format!("Auto-start failed: {}", error.message));
                }
            }
        });

        Ok(())
    }

    pub fn restart_if_configured(&self) -> Result<bool, AppError> {
        let settings = load_or_create_settings(&self.paths)?;
        if settings.workspace_path.is_none() {
            return Ok(false);
        }
        self.stop_all();
        self.start_with_settings(settings)?;
        Ok(true)
    }

    pub fn is_running(&self) -> Result<bool, AppError> {
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| AppError::new("runtime_lock", "Runtime state is unavailable."))?;
        runtime.cleanup_exited();
        Ok(runtime.is_running())
    }

    pub fn regenerate_mcp_url(&self) -> Result<(), AppError> {
        self.stop_all();
        let mut settings = load_or_create_settings(&self.paths)?;
        let mut created = false;
        for _ in 0..8 {
            let candidate = Settings {
                zrok_name: fresh_zrok_name(),
                public_path_token: fresh_public_path_token(),
                ..settings.clone()
            };
            if ensure_zrok_name(&candidate).is_ok() {
                settings = candidate;
                created = true;
                break;
            }
        }
        if !created {
            return Err(AppError::new(
                "zrok_create_name",
                "Could not create a new zrok reserved name. Is zrok2 enabled?",
            ));
        }
        save_settings(&self.paths, &settings)?;
        if settings.workspace_path.is_some() {
            self.start_with_settings(settings)?;
        }
        Ok(())
    }

    fn start_with_settings(&self, settings: Settings) -> Result<(), AppError> {
        validate_zrok_name(&settings.zrok_name)?;
        write_managed_mcp_config(&self.paths, &settings)?;
        if !mcp_runtime_exists(&settings) {
            let error = AppError::new(
                "missing_gpt_repo_mcp",
                "Bundled gpt-repo-mcp runtime is missing. Rebuild Secret Tunnel so MCP is included.",
            );
            return Err(self.record_start_failure(&settings, error));
        }
        if bundled_executable("node").is_none() {
            let error = AppError::new(
                "missing_node",
                "Bundled Node runtime is missing. Rebuild Secret Tunnel so Node is included.",
            );
            return Err(self.record_start_failure(&settings, error));
        }
        if bundled_executable("zrok2").is_none() {
            let error = AppError::new(
                "missing_zrok",
                "Bundled zrok2 is missing. Rebuild Secret Tunnel so zrok is included.",
            );
            return Err(self.record_start_failure(&settings, error));
        }
        if !zrok_environment_enabled() {
            let error = AppError::new("zrok_not_enabled", "zrok needs enable");
            return Err(self.record_start_failure(&settings, error));
        }

        let stale_children = {
            let mut runtime = self
                .runtime
                .lock()
                .map_err(|_| AppError::new("runtime_lock", "Runtime state is unavailable."))?;
            runtime.cleanup_exited();
            if runtime.is_running() || runtime.starting {
                return Ok(());
            }
            let stale_children = if runtime.mcp.is_some() || runtime.zrok.is_some() {
                runtime.push_log("app", "Cleaning up partial process state.");
                let stale = (
                    runtime.mcp.take().map(|child| (child.id(), child)),
                    runtime.zrok.take().map(|child| (child.id(), child)),
                );
                if let Some((pid, _)) = &stale.0 {
                    runtime.untrack_child(*pid, AssetClass::McpNode);
                }
                if let Some((pid, _)) = &stale.1 {
                    runtime.untrack_child(*pid, AssetClass::ZrokShare);
                }
                stale
            } else {
                (None, None)
            };
            runtime.starting = true;
            runtime.push_log("app", "Starting MCP server and zrok share.");
            stale_children
        };

        if let Some((_, child)) = stale_children.1 {
            stop_child(child);
        }
        if let Some((_, child)) = stale_children.0 {
            stop_child(child);
        }

        let start_result = (|| {
            {
                let Ok(mut runtime) = self.runtime.lock() else {
                    return Err(AppError::new("runtime_lock", "Runtime state is unavailable."));
                };
                // A new run owns a fresh set of shares; previous run's registered
                // shares are stale for this run and must be cleared first.
                runtime.clear_owned_shares_for_host();
            }
            clear_stale_local_port(self.runtime.clone());
            ensure_zrok_name(&settings)?;
            self.clear_stale_zrok_shares(&settings.zrok_name);

            let mcp = self.spawn_mcp(&settings)?;
            let zrok = match self.spawn_zrok(&settings) {
                Ok(child) => child,
                Err(error) => {
                    let pid = mcp.id();
                    stop_child(mcp);
                    if let Ok(mut runtime) = self.runtime.lock() {
                        runtime.untrack_child(pid, AssetClass::McpNode);
                    }
                    return Err(error);
                }
            };

            let mut runtime = self
                .runtime
                .lock()
                .map_err(|_| AppError::new("runtime_lock", "Runtime state is unavailable."))?;
            runtime.mcp = Some(mcp);
            runtime.zrok = Some(zrok);
            runtime.starting = false;
            runtime.push_log("app", format!("MCP URL: {}", mcp_url(&settings)));
            self.write_status_probe("running", &settings, &runtime, None);
            // Remember whatever is registered under our stable host right now so
            // later cleanup does not delete the share we just brought up.
            drop(runtime);
            self.takeover_or_keep_owned_shares(&settings.zrok_name);
            Ok(())
        })();

        if start_result.is_err() {
            if let Ok(mut runtime) = self.runtime.lock() {
                runtime.starting = false;
                self.write_status_probe("start-failed", &settings, &runtime, None);
            }
        }

        start_result
    }

    pub fn stop(&self) -> Result<(), AppError> {
        self.stop_all();
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| AppError::new("runtime_lock", "Runtime state is unavailable."))?;
        runtime.push_log("app", "Stopped local processes.");
        let settings = load_or_create_settings(&self.paths)?;
        self.write_status_probe("stopped", &settings, &runtime, None);
        Ok(())
    }

    pub fn enable_zrok(&self, token: String) -> Result<(), AppError> {
        enable_zrok_environment(&token)?;
        self.push_app_log("zrok environment enabled.");
        Ok(())
    }

    pub fn enable_zrok_from_environment_if_present(&self) -> Result<bool, AppError> {
        if zrok_environment_enabled() {
            return Ok(false);
        }

        let Some(token) = zrok_enable_token_from_environment() else {
            return Ok(false);
        };

        enable_zrok_environment(&token)?;
        clear_zrok_enable_token_environment();
        self.push_app_log("zrok environment enabled from launch environment.");
        Ok(true)
    }

    pub fn push_app_log(&self, line: impl Into<String>) {
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.push_log("app", line.into());
        }
    }

    pub fn stop_all(&self) {
        let (mcp, zrok) = match self.runtime.lock() {
            Ok(mut runtime) => {
                let children = (runtime.mcp.take(), runtime.zrok.take());
                runtime.owned_share_tokens.clear();
                runtime.known_mcp_pids.clear();
                runtime.known_zrok_pids.clear();
                if let Ok(settings) = load_or_create_settings(&self.paths) {
                    self.write_status_probe("stopped", &settings, &runtime, None);
                }
                children
            }
            Err(_) => return,
        };
        if let Some(child) = zrok {
            stop_child(child);
        }
        if let Some(child) = mcp {
            stop_child(child);
        }
    }

    fn spawn_mcp(&self, settings: &Settings) -> Result<Child, AppError> {
        let runtime_dir = bundled_mcp_runtime_dir().ok_or_else(|| {
            AppError::new(
                "missing_gpt_repo_mcp",
                "Bundled gpt-repo-mcp runtime is missing. Rebuild Secret Tunnel so MCP is included.",
            )
        })?;
        let node = bundled_executable("node").ok_or_else(|| {
            AppError::new(
                "missing_node",
                "Bundled Node runtime is missing. Rebuild Secret Tunnel so Node is included.",
            )
        })?;
        let mut command = Command::new(node);
        let normalized_runtime_dir =
            PathBuf::from(normalize_windows_verbatim_prefix(&runtime_dir.to_string_lossy()));
        let script_path = normalized_runtime_dir.join("dist").join("server.js");
        command
            .current_dir(&normalized_runtime_dir)
            .arg(&script_path);
        configure_mcp_environment(&mut command, &self.paths, settings);
        if settings.access_mode == AccessMode::Read {
            command.env("GPT_REPO_READ_ONLY_SURFACE", "1");
        } else {
            command.env_remove("GPT_REPO_READ_ONLY_SURFACE");
        }
        command
            .env(INSTANCE_ID_ENV, &self.instance_id)
            .env(ASSET_CLASS_ENV, AssetClass::McpNode.tag());
        spawn_tracked(command, "mcp", AssetClass::McpNode, self.runtime.clone())
    }

    fn spawn_zrok(&self, settings: &Settings) -> Result<Child, AppError> {
        let share_name = format!("public:{}", settings.zrok_name);
        let mut command = Command::new(bundled_zrok_command()?);
        command
            .arg("share")
            .arg("public")
            .arg(format!("http://127.0.0.1:{LOCAL_PORT}"))
            .arg("-n")
            .arg(share_name)
            .arg("--headless")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env(INSTANCE_ID_ENV, &self.instance_id)
            .env(ASSET_CLASS_ENV, AssetClass::ZrokShare.tag());
        spawn_tracked(command, "zrok", AssetClass::ZrokShare, self.runtime.clone())
    }

    fn record_start_failure(&self, settings: &Settings, error: AppError) -> AppError {
        if let Ok(runtime) = self.runtime.lock() {
            self.write_status_probe("start-failed", settings, &runtime, Some(error.code));
        }
        error
    }

    fn write_status_probe(
        &self,
        event: &str,
        settings: &Settings,
        runtime: &RuntimeState,
        failure_code: Option<&str>,
    ) {
        let Some(path) = &self.status_probe_path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let document = json!({
            "event": event,
            "timestampMs": timestamp_ms,
            "running": runtime.is_running(),
            "starting": runtime.starting,
            "workspaceConfigured": settings.workspace_path.is_some(),
            "accessMode": settings.access_mode.as_str(),
            "zrokEnabled": zrok_environment_enabled(),
            "zrokInstalled": bundled_executable("zrok2").is_some(),
            "mcpRuntimeFound": mcp_runtime_exists(settings),
            "failureCode": failure_code,
            "mcpPids": runtime.known_mcp_pids.iter().copied().collect::<Vec<_>>(),
            "zrokPids": runtime.known_zrok_pids.iter().copied().collect::<Vec<_>>(),
            "ownedShareTokens": runtime.owned_share_tokens.iter().cloned().collect::<Vec<_>>(),
            "logs": runtime.logs.iter().map(|log| format!("{}: {}", log.source, log.line)).collect::<Vec<_>>()
        });
        let _ = fs::write(
            path,
            serde_json::to_vec_pretty(&document).unwrap_or_default(),
        );
    }
}

#[derive(Default)]
struct RuntimeState {
    mcp: Option<Child>,
    zrok: Option<Child>,
    starting: bool,
    last_autostart_attempt: Option<Instant>,
    logs: Vec<LogLine>,
    owned_share_tokens: HashSet<String>,
    known_mcp_pids: HashSet<u32>,
    known_zrok_pids: HashSet<u32>,
}

impl RuntimeState {
    fn is_running(&self) -> bool {
        self.mcp.is_some() && self.zrok.is_some()
    }

    fn record_owned_share(&mut self, token: &str) {
        if !token.is_empty() {
            self.owned_share_tokens.insert(token.to_string());
        }
    }

    fn clear_owned_shares_for_host(&mut self) {
        self.owned_share_tokens.clear();
    }

    fn track_mcp_pid(&mut self, pid: u32) {
        self.known_mcp_pids.insert(pid);
    }

    fn track_zrok_pid(&mut self, pid: u32) {
        self.known_zrok_pids.insert(pid);
    }

    fn untrack_child(&mut self, pid: u32, class: AssetClass) {
        match class {
            AssetClass::McpNode => {
                self.known_mcp_pids.remove(&pid);
            }
            AssetClass::ZrokShare => {
                self.known_zrok_pids.remove(&pid);
            }
        }
    }

    fn cleanup_exited(&mut self) {
        let mcp_pid = self.mcp.as_ref().map(|child| child.id());
        if child_exited(&mut self.mcp) {
            if let Some(pid) = mcp_pid {
                self.untrack_child(pid, AssetClass::McpNode);
            }
            self.push_log("mcp", "Process exited.");
        }
        let zrok_pid = self.zrok.as_ref().map(|child| child.id());
        if child_exited(&mut self.zrok) {
            if let Some(pid) = zrok_pid {
                self.untrack_child(pid, AssetClass::ZrokShare);
            }
            self.push_log("zrok", "Process exited.");
        }
    }

    fn push_log(&mut self, source: impl Into<String>, line: impl Into<String>) {
        self.logs.push(LogLine {
            source: source.into(),
            line: line.into(),
        });
        if self.logs.len() > MAX_LOG_LINES {
            let extra = self.logs.len() - MAX_LOG_LINES;
            self.logs.drain(0..extra);
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub source: String,
    pub line: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDto {
    pub workspace_path: Option<String>,
    pub access_mode: String,
    pub zrok_name: String,
    pub public_path_token: String,
    pub gpt_repo_mcp_path: String,
    pub managed_config_path: String,
}

impl SettingsDto {
    fn from_settings(settings: &Settings, paths: &AppPaths) -> Self {
        Self {
            workspace_path: settings.workspace_path.clone(),
            access_mode: settings.access_mode.as_str().to_string(),
            zrok_name: settings.zrok_name.clone(),
            public_path_token: settings.public_path_token.clone(),
            gpt_repo_mcp_path: settings.gpt_repo_mcp_path.clone(),
            managed_config_path: paths.managed_config_path.to_string_lossy().to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusDto {
    pub settings: SettingsDto,
    pub mcp_url: String,
    pub running: bool,
    pub autostart_enabled: bool,
    pub zrok_installed: bool,
    pub zrok_enabled: bool,
    pub gpt_repo_mcp_found: bool,
    pub logs: Vec<LogLine>,
}

fn spawn_tracked(
    mut command: Command,
    label: &'static str,
    class: AssetClass,
    runtime: Arc<Mutex<RuntimeState>>,
) -> Result<Child, AppError> {
    suppress_console_window(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| AppError::new("spawn_failed", format!("{label}: {error}")))?;
    let pid = child.id();
    if let Ok(mut runtime) = runtime.lock() {
        match class {
            AssetClass::McpNode => runtime.track_mcp_pid(pid),
            AssetClass::ZrokShare => runtime.track_zrok_pid(pid),
        }
    }
    if let Some(stdout) = child.stdout.take() {
        spawn_log_reader(label, stdout, runtime.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_reader(label, stderr, runtime);
    }
    Ok(child)
}

fn spawn_log_reader<R>(label: &'static str, reader: R, runtime: Arc<Mutex<RuntimeState>>)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines().map_while(Result::ok) {
            if let Ok(mut runtime) = runtime.lock() {
                runtime.push_log(label, line);
            }
        }
    });
}

fn child_exited(slot: &mut Option<Child>) -> bool {
    let Some(child) = slot.as_mut() else {
        return false;
    };
    match child.try_wait() {
        Ok(None) => false,
        Ok(Some(_)) | Err(_) => {
            *slot = None;
            true
        }
    }
}

fn stop_child(mut child: Child) {
    if matches!(child.try_wait(), Ok(None)) {
        kill_process_tree(child.id());
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(windows)]
fn kill_process_tree(pid: u32) {
    let mut command = Command::new("taskkill");
    command
        .arg("/PID")
        .arg(pid.to_string())
        .arg("/T")
        .arg("/F")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    suppress_console_window(&mut command);
    let _ = command.status();
}

#[cfg(not(windows))]
fn kill_process_tree(_pid: u32) {}

#[cfg(windows)]
fn clear_stale_local_port(runtime: Arc<Mutex<RuntimeState>>) {
    let mut command = Command::new("netstat");
    command
        .arg("-ano")
        .arg("-p")
        .arg("tcp")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    suppress_console_window(&mut command);
    let Ok(output) = command.output() else {
        return;
    };

    let (listeners, owned_mcp_pids) = {
        let runtime = match runtime.lock() {
            Ok(runtime) => runtime,
            Err(_) => return,
        };
        (
            listening_pids_on_local_port(&String::from_utf8_lossy(&output.stdout)),
            runtime.known_mcp_pids.clone(),
        )
    };
    // Reap leftover MCP node processes on the port. We reap both this
    // instance's tracked children and orphaned children of a previous
    // instance (for example after a crash), but only when the process is
    // genuinely one of our asset class (a bundled node running our bundled
    // gpt-repo-mcp server). Never touch unrelated listeners on the port.
    for pid in listeners {
        let is_owned = owned_mcp_pids.contains(&pid);
        let is_mcp_asset = is_mcp_asset_process(pid);
        if !is_owned && !is_mcp_asset {
            continue;
        }
        kill_process_tree(pid);
        if let Ok(mut runtime) = runtime.lock() {
            runtime.untrack_child(pid, AssetClass::McpNode);
            runtime.push_log(
                "app",
                format!("Reaped leftover MCP process {pid} on port {LOCAL_PORT}."),
            );
        }
    }
}

#[cfg(windows)]
fn is_mcp_asset_process(pid: u32) -> bool {
    let mut command = Command::new("wmic");
    command
        .arg("process")
        .arg("where")
        .arg(format!("ProcessId={pid}"))
        .arg("get")
        .arg("CommandLine")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    suppress_console_window(&mut command);
    let Ok(output) = command.output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(node_path) = bundled_executable("node") else {
        return false;
    };
    let node_fingerprint =
        normalize_windows_verbatim_prefix(&node_path.to_string_lossy());
    text.contains(&node_fingerprint) && text.contains("server.js")
}

#[cfg(not(windows))]
fn clear_stale_local_port(_runtime: Arc<Mutex<RuntimeState>>) {}

#[cfg(windows)]
fn listening_pids_on_local_port(output: &str) -> Vec<u32> {
    let mut pids = Vec::new();
    for line in output.lines() {
        let parts = line.split_whitespace().collect::<Vec<_>>();
        if parts.len() < 5 {
            continue;
        }
        if !parts[0].eq_ignore_ascii_case("tcp") || !parts[3].eq_ignore_ascii_case("listening") {
            continue;
        }
        if !parts[1].ends_with(&format!(":{LOCAL_PORT}")) {
            continue;
        }
        if let Ok(pid) = parts[4].parse::<u32>() {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

pub fn open_in_browser(url: &str) -> Result<(), AppError> {
    let url = validate_external_url(url)?;
    let mut command = platform_open_command(&url);
    suppress_console_window(&mut command);
    command
        .status()
        .map_err(|error| AppError::new("open_url", error.to_string()))?;
    Ok(())
}

fn validate_external_url(url: &str) -> Result<String, AppError> {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(AppError::new(
            "invalid_url",
            "Only http(s) links can be opened.",
        ));
    }
    let has_unsafe_bytes = url.bytes().any(|byte| {
        byte.is_ascii_whitespace()
            || matches!(
                byte,
                b'"' | b'&' | b';' | b'|' | b'<' | b'>' | b'^' | b'`' | b'\\'
            )
    });
    if url.len() > 2048 || has_unsafe_bytes {
        return Err(AppError::new(
            "invalid_url",
            "The link is not a valid web address.",
        ));
    }
    Ok(url.to_string())
}

#[cfg(target_os = "windows")]
fn platform_open_command(url: &str) -> Command {
    let mut command = Command::new("cmd");
    command.args(["/C", "start", "", url]);
    command
}

#[cfg(target_os = "macos")]
fn platform_open_command(url: &str) -> Command {
    Command::new("open").arg(url)
}

#[cfg(target_os = "linux")]
fn platform_open_command(url: &str) -> Command {
    Command::new("xdg-open").arg(url)
}

fn configure_mcp_environment(command: &mut Command, paths: &AppPaths, settings: &Settings) {
    command
        .env("GPT_REPO_CONFIG", &paths.managed_config_path)
        .env("REPO_READER_CONFIG", &paths.managed_config_path)
        .env("GPT_REPO_HOST", "127.0.0.1")
        .env("PORT", LOCAL_PORT)
        .env("GPT_REPO_PUBLIC_PATH_TOKEN", &settings.public_path_token)
        .env("REPO_READER_PUBLIC_PATH_TOKEN", &settings.public_path_token)
        .env("GPT_REPO_LOG_FORMAT", "pretty")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
}

fn mcp_runtime_exists(settings: &Settings) -> bool {
    let _ = settings;
    bundled_mcp_runtime_dir().is_some()
}

fn bundled_mcp_runtime_dir() -> Option<PathBuf> {
    for root in resource_search_dirs() {
        for candidate in [
            root.join("resources").join("gpt-repo-mcp"),
            root.join("gpt-repo-mcp"),
        ] {
            if candidate.join("dist").join("server.js").is_file()
                && candidate.join("node_modules").is_dir()
            {
                return Some(candidate);
            }
        }
    }
    None
}

impl AppState {
    fn clear_stale_zrok_shares(&self, zrok_name: &str) {
        let owned_tokens: HashSet<String> = match self.runtime.lock() {
            Ok(runtime) => runtime.owned_share_tokens.iter().cloned().collect(),
            Err(_) => return,
        };
        let tokens = stale_zrok_share_tokens(zrok_name);
        if tokens.is_empty() {
            return;
        }
        let Ok(zrok) = bundled_zrok_command() else {
            return;
        };
        for token in tokens {
            if owned_tokens.contains(&token) {
                continue;
            }
            let mut command = Command::new(&zrok);
            command
                .arg("delete")
                .arg("share")
                .arg(&token)
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            suppress_console_window(&mut command);
            let cleared = command
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            self.push_app_log(if cleared {
                format!("Cleared stale zrok share {token} for a stable URL.")
            } else {
                format!("Could not clear stale zrok share {token}.")
            });
        }
    }

    fn takeover_or_keep_owned_shares(&self, zrok_name: &str) {
        // Poll a few moments: the share we just registered may take a moment to
        // appear in `list shares`, and we want it marked owned before any future
        // cleanup compares against the owned set.
        for _ in 0..10 {
            let live_tokens = stale_zrok_share_tokens(zrok_name);
            if !live_tokens.is_empty() {
                if let Ok(mut runtime) = self.runtime.lock() {
                    for token in live_tokens {
                        runtime.record_owned_share(&token);
                    }
                }
                return;
            }
            thread::sleep(Duration::from_millis(300));
        }
    }
}

fn stale_zrok_share_tokens(zrok_name: &str) -> Vec<String> {
    let Ok(zrok) = bundled_zrok_command() else {
        return Vec::new();
    };
    let mut command = Command::new(zrok);
    command
        .arg("list")
        .arg("shares")
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    suppress_console_window(&mut command);
    let Ok(output) = command.output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    stale_zrok_share_tokens_from_json(&String::from_utf8_lossy(&output.stdout), zrok_name)
}

fn stale_zrok_share_tokens_from_json(output: &str, zrok_name: &str) -> Vec<String> {
    let expected_host = format!("{zrok_name}.shares.zrok.io");
    let Ok(document) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    let Some(shares) = document.get("shares").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut tokens = Vec::new();
    for share in shares {
        let Some(endpoints) = share
            .get("frontendEndpoints")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        if endpoints
            .iter()
            .any(|endpoint| endpoint.as_str() == Some(&expected_host))
        {
            if let Some(token) = share
                .get("shareToken")
                .and_then(serde_json::Value::as_str)
            {
                tokens.push(token.to_string());
            }
        }
    }
    tokens
}

fn ensure_zrok_name(settings: &Settings) -> Result<(), AppError> {
    let mut command = Command::new(bundled_zrok_command()?);
    command
        .arg("create")
        .arg("name")
        .arg("-n")
        .arg("public")
        .arg(&settings.zrok_name);
    suppress_console_window(&mut command);
    let output = command
        .output()
        .map_err(|error| AppError::new("zrok_create_name", error.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if zrok_name_already_reserved(&combined) {
        return Ok(());
    }
    Err(AppError::new(
        "zrok_create_name",
        "Could not create or verify the zrok reserved name. Is zrok2 enabled?",
    ))
}

fn zrok_name_already_reserved(output: &str) -> bool {
    let combined = output.to_ascii_lowercase();
    ["exist", "already", "conflict", "taken"]
        .iter()
        .any(|needle| combined.contains(needle))
}

fn enable_zrok_environment(token: &str) -> Result<(), AppError> {
    let token = validate_zrok_token(token)?;
    let mut command = Command::new(bundled_zrok_command()?);
    command
        .arg("enable")
        .arg(token)
        .arg("--headless")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    suppress_console_window(&mut command);
    let output = command
        .output()
        .map_err(|error| AppError::new("zrok_enable", error.to_string()))?;
    if output.status.success() && zrok_environment_enabled() {
        return Ok(());
    }

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    if combined.contains("already") && zrok_environment_enabled() {
        return Ok(());
    }
    if combined.contains("invalid")
        || combined.contains("unauthorized")
        || combined.contains("token")
    {
        return Err(AppError::new(
            "zrok_enable",
            "zrok rejected that token. Check the token and try again.",
        ));
    }
    Err(AppError::new(
        "zrok_enable",
        "Could not enable zrok. Check the token and your network connection.",
    ))
}

fn validate_zrok_token(token: &str) -> Result<&str, AppError> {
    let token = token.trim();
    if token.len() < 8 || token.chars().any(char::is_whitespace) {
        return Err(AppError::new(
            "invalid_zrok_token",
            "Paste a valid zrok enable token.",
        ));
    }
    Ok(token)
}

fn zrok_enable_token_from_environment() -> Option<String> {
    ["SECRET_TUNNEL_ZROK_ENABLE_TOKEN", "ZROK_ENABLE_TOKEN"]
        .into_iter()
        .filter_map(env::var_os)
        .map(|token| token.to_string_lossy().trim().to_string())
        .find(|token| !token.is_empty())
}

fn clear_zrok_enable_token_environment() {
    for variable in ["SECRET_TUNNEL_ZROK_ENABLE_TOKEN", "ZROK_ENABLE_TOKEN"] {
        env::remove_var(variable);
    }
}

fn status_probe_path_from_environment() -> Option<PathBuf> {
    env::var_os("SECRET_TUNNEL_STATUS_FILE")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

fn zrok_environment_enabled() -> bool {
    let Ok(zrok) = bundled_zrok_command() else {
        return false;
    };
    let mut command = Command::new(zrok);
    command
        .arg("status")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    suppress_console_window(&mut command);
    let Ok(output) = command.output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    zrok_status_indicates_enabled(&format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn zrok_status_indicates_enabled(output: &str) -> bool {
    let normalized = output.to_ascii_lowercase();
    if normalized.trim().is_empty() {
        return false;
    }
    !(normalized.contains("zrok2 enable") || normalized.contains("not enabled"))
}

fn bundled_zrok_command() -> Result<OsString, AppError> {
    bundled_executable("zrok2")
        .map(PathBuf::into_os_string)
        .ok_or_else(|| {
            AppError::new(
                "missing_zrok",
                "Bundled zrok2 is missing. Rebuild Secret Tunnel so zrok is included.",
            )
        })
}

fn bundled_executable(command: &str) -> Option<PathBuf> {
    let command_path = Path::new(command);
    if command_path.is_absolute() && command_path.is_file() {
        return Some(command_path.to_path_buf());
    }

    for extra in bundled_executable_search_dirs() {
        if let Some(path) = find_executable_in_dir(&extra, command) {
            return Some(path);
        }
    }
    None
}

fn find_executable_in_dir(dir: &Path, command: &str) -> Option<PathBuf> {
    for candidate in executable_names(command) {
        let path = dir.join(candidate);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn executable_names(command: &str) -> Vec<String> {
    let path = Path::new(command);
    if path.extension().is_some() {
        return vec![command.to_string()];
    }

    if cfg!(windows) {
        let mut names = Vec::new();
        if let Some(path_ext) = env::var_os("PATHEXT") {
            for ext in path_ext.to_string_lossy().split(';') {
                if !ext.trim().is_empty() {
                    names.push(format!("{command}{}", ext.to_ascii_lowercase()));
                    names.push(format!("{command}{}", ext.to_ascii_uppercase()));
                }
            }
        }
        names.extend([
            format!("{command}.exe"),
            format!("{command}.cmd"),
            format!("{command}.bat"),
        ]);
        names.sort();
        names.dedup();
        names
    } else {
        vec![command.to_string()]
    }
}

fn bundled_executable_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for root in resource_search_dirs() {
        dirs.push(root.join("binaries"));
        dirs.push(root);
    }
    dirs.dedup();
    dirs
}

fn resource_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(resource_dir) = env::var_os("SECRET_TUNNEL_RESOURCE_DIR") {
        dirs.push(PathBuf::from(resource_dir));
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            dirs.push(exe_dir.to_path_buf());
        }
    }
    if let Ok(current_dir) = env::current_dir() {
        dirs.push(current_dir.clone());
        dirs.push(current_dir.join("src-tauri"));
    }
    dirs.dedup();
    dirs
}

#[cfg(windows)]
fn suppress_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn suppress_console_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::{
        bundled_executable, bundled_mcp_runtime_dir, clear_zrok_enable_token_environment,
        stale_zrok_share_tokens_from_json, status_probe_path_from_environment,
        validate_zrok_token, zrok_name_already_reserved, zrok_enable_token_from_environment,
        zrok_status_indicates_enabled,
    };
    use std::sync::Mutex;
    use std::{env, fs};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(windows)]
    #[test]
    fn finds_listening_pid_on_local_port() {
        use super::listening_pids_on_local_port;

        let output = r#"
  TCP    127.0.0.1:8787         0.0.0.0:0              LISTENING       44648
  TCP    127.0.0.1:1420         0.0.0.0:0              LISTENING       51980
  TCP    [::1]:8787             [::]:0                 LISTENING       44648
"#;

        assert_eq!(listening_pids_on_local_port(output), vec![44648]);
    }

    #[test]
    fn tolerates_existing_zrok_name_conflicts() {
        assert!(zrok_name_already_reserved("createShareNameConflict"));
        assert!(zrok_name_already_reserved("name already exists"));
        assert!(zrok_name_already_reserved("resource already taken"));
        assert!(!zrok_name_already_reserved("unauthorized"));
        assert!(!zrok_name_already_reserved(""));
    }

    #[test]
    fn extracts_stale_zrok_shares_for_the_stable_host() {
        let output = r#"{
  "shares": [
    {
      "shareToken": "reallystale1",
      "shareMode": "public",
      "backendMode": "proxy",
      "frontendEndpoints": ["gptmcpexamplename1.shares.zrok.io"],
      "target": "http://127.0.0.1:8787"
    },
    {
      "shareToken": "othername2",
      "shareMode": "public",
      "frontendEndpoints": ["somethingelse.shares.zrok.io"]
    },
    {
      "shareToken": "keepme3",
      "shareMode": "public",
      "frontendEndpoints": [
        "gptmcpexamplename1.shares.zrok.io",
        "gptmcpexamplename1.shares.zrok.io"
      ]
    }
  ]
}"#;
        assert_eq!(
            stale_zrok_share_tokens_from_json(output, "gptmcpexamplename1"),
            vec!["reallystale1".to_string(), "keepme3".to_string()]
        );
    }

    #[test]
    fn stale_zrok_share_parsing_is_graceful() {
        assert!(stale_zrok_share_tokens_from_json("not json", "name").is_empty());
        assert!(stale_zrok_share_tokens_from_json("", "name").is_empty());
        assert!(
            stale_zrok_share_tokens_from_json(r#"{"shares":[]}"#, "name").is_empty()
        );
        assert!(
            stale_zrok_share_tokens_from_json(
                r#"{"shares":[{"shareToken":"token"}]}"#,
                "name"
            )
            .is_empty()
        );
    }

    #[test]
    fn detects_disabled_zrok_status() {
        let output = r#"
Config:

To create a local environment use the zrok2 enable command.
"#;

        assert!(!zrok_status_indicates_enabled(output));
    }

    #[test]
    fn validates_zrok_enable_tokens() {
        assert_eq!(validate_zrok_token("  abcdefgh  ").unwrap(), "abcdefgh");
        assert!(validate_zrok_token("").is_err());
        assert!(validate_zrok_token("abc").is_err());
        assert!(validate_zrok_token("abc defgh").is_err());
    }

    #[test]
    fn detects_bundled_mcp_runtime_from_resource_dir() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = env::temp_dir().join(format!(
            "secret-tunnel-mcp-runtime-test-{}",
            std::process::id()
        ));
        let runtime = root.join("resources").join("gpt-repo-mcp");
        fs::create_dir_all(runtime.join("dist")).unwrap();
        fs::create_dir_all(runtime.join("node_modules")).unwrap();
        fs::write(runtime.join("dist").join("server.js"), "").unwrap();

        let previous = env::var_os("SECRET_TUNNEL_RESOURCE_DIR");
        env::set_var("SECRET_TUNNEL_RESOURCE_DIR", &root);
        assert_eq!(bundled_mcp_runtime_dir(), Some(runtime));
        if let Some(previous) = previous {
            env::set_var("SECRET_TUNNEL_RESOURCE_DIR", previous);
        } else {
            env::remove_var("SECRET_TUNNEL_RESOURCE_DIR");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bundled_executable_lookup_does_not_use_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = env::temp_dir().join(format!("secret-tunnel-path-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("secret-tunnel-fake-tool.exe"), "").unwrap();

        let previous_path = env::var_os("PATH");
        env::set_var("PATH", &root);
        assert_eq!(bundled_executable("secret-tunnel-fake-tool"), None);
        if let Some(previous_path) = previous_path {
            env::set_var("PATH", previous_path);
        } else {
            env::remove_var("PATH");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_zrok_enable_token_from_launch_environment() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_zrok_enable_token_environment();
        assert_eq!(zrok_enable_token_from_environment(), None);

        env::set_var("ZROK_ENABLE_TOKEN", " fallback-token ");
        assert_eq!(
            zrok_enable_token_from_environment(),
            Some("fallback-token".to_string())
        );

        env::set_var("SECRET_TUNNEL_ZROK_ENABLE_TOKEN", " primary-token ");
        assert_eq!(
            zrok_enable_token_from_environment(),
            Some("primary-token".to_string())
        );

        clear_zrok_enable_token_environment();
        assert_eq!(zrok_enable_token_from_environment(), None);
    }

    #[test]
    fn reads_status_probe_path_from_launch_environment() {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = env::var_os("SECRET_TUNNEL_STATUS_FILE");
        env::remove_var("SECRET_TUNNEL_STATUS_FILE");
        assert_eq!(status_probe_path_from_environment(), None);

        let path = env::temp_dir().join("secret-tunnel-status-probe.json");
        env::set_var("SECRET_TUNNEL_STATUS_FILE", &path);
        assert_eq!(status_probe_path_from_environment(), Some(path));

        if let Some(previous) = previous {
            env::set_var("SECRET_TUNNEL_STATUS_FILE", previous);
        } else {
            env::remove_var("SECRET_TUNNEL_STATUS_FILE");
        }
    }
}
