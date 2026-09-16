use crate::error::AppError;
use crate::settings::{
    gpt_repo_mcp_exists, load_or_create_settings, mcp_url, validate_zrok_name,
    write_managed_mcp_config, AccessMode, AppPaths, Settings,
};
use serde::Serialize;
use std::env;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

const LOCAL_PORT: &str = "8787";
const MAX_LOG_LINES: usize = 160;

#[derive(Clone)]
pub struct AppState {
    pub paths: AppPaths,
    runtime: Arc<Mutex<RuntimeState>>,
}

impl AppState {
    pub fn new(paths: AppPaths) -> Self {
        Self {
            paths,
            runtime: Arc::new(Mutex::new(RuntimeState::default())),
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
            zrok_installed: command_exists("zrok2"),
            gpt_repo_mcp_found: gpt_repo_mcp_exists(&settings),
            logs: runtime.logs.clone(),
        })
    }

    pub fn start(&self) -> Result<(), AppError> {
        let settings = load_or_create_settings(&self.paths)?;
        validate_zrok_name(&settings.zrok_name)?;
        if !gpt_repo_mcp_exists(&settings) {
            return Err(AppError::new(
                "missing_gpt_repo_mcp",
                "Could not find gpt-repo-mcp. Clone and build it in your GitHub folder first.",
            ));
        }
        if !command_exists("zrok2") {
            return Err(AppError::new(
                "missing_zrok",
                "zrok2 is not on PATH. Install zrok2 and enable your zrok account.",
            ));
        }

        write_managed_mcp_config(&self.paths, &settings)?;

        {
            let mut runtime = self
                .runtime
                .lock()
                .map_err(|_| AppError::new("runtime_lock", "Runtime state is unavailable."))?;
            runtime.cleanup_exited();
            if runtime.is_running() {
                return Err(AppError::new(
                    "already_running",
                    "The MCP tunnel is already running.",
                ));
            }
            runtime.push_log("app", "Starting MCP server and zrok share.");
        }

        ensure_zrok_name(&settings)?;

        let mcp = self.spawn_mcp(&settings)?;
        let zrok = match self.spawn_zrok(&settings) {
            Ok(child) => child,
            Err(error) => {
                stop_child(mcp);
                return Err(error);
            }
        };

        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| AppError::new("runtime_lock", "Runtime state is unavailable."))?;
        runtime.mcp = Some(mcp);
        runtime.zrok = Some(zrok);
        runtime.push_log("app", format!("MCP URL: {}", mcp_url(&settings)));
        Ok(())
    }

    pub fn stop(&self) -> Result<(), AppError> {
        self.stop_all();
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| AppError::new("runtime_lock", "Runtime state is unavailable."))?;
        runtime.push_log("app", "Stopped local processes.");
        Ok(())
    }

    pub fn stop_all(&self) {
        let (mcp, zrok) = match self.runtime.lock() {
            Ok(mut runtime) => (runtime.mcp.take(), runtime.zrok.take()),
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
        let mut command = Command::new(npm_command());
        command
            .arg("run")
            .arg("dev")
            .current_dir(&settings.gpt_repo_mcp_path)
            .env("GPT_REPO_CONFIG", &self.paths.managed_config_path)
            .env("REPO_READER_CONFIG", &self.paths.managed_config_path)
            .env("GPT_REPO_HOST", "127.0.0.1")
            .env("PORT", LOCAL_PORT)
            .env("GPT_REPO_PUBLIC_PATH_TOKEN", &settings.public_path_token)
            .env("REPO_READER_PUBLIC_PATH_TOKEN", &settings.public_path_token)
            .env("GPT_REPO_LOG_FORMAT", "pretty")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if settings.access_mode == AccessMode::Read {
            command.env("GPT_REPO_READ_ONLY_SURFACE", "1");
        } else {
            command.env_remove("GPT_REPO_READ_ONLY_SURFACE");
        }
        spawn_tracked(command, "mcp", self.runtime.clone())
    }

    fn spawn_zrok(&self, settings: &Settings) -> Result<Child, AppError> {
        let share_name = format!("public:{}", settings.zrok_name);
        let mut command = Command::new(zrok_command());
        command
            .arg("share")
            .arg("public")
            .arg(format!("http://127.0.0.1:{LOCAL_PORT}"))
            .arg("-n")
            .arg(share_name)
            .arg("--headless")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        spawn_tracked(command, "zrok", self.runtime.clone())
    }
}

#[derive(Default)]
struct RuntimeState {
    mcp: Option<Child>,
    zrok: Option<Child>,
    logs: Vec<LogLine>,
}

impl RuntimeState {
    fn is_running(&self) -> bool {
        self.mcp.is_some() || self.zrok.is_some()
    }

    fn cleanup_exited(&mut self) {
        if child_exited(&mut self.mcp) {
            self.push_log("mcp", "Process exited.");
        }
        if child_exited(&mut self.zrok) {
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
    pub gpt_repo_mcp_found: bool,
    pub logs: Vec<LogLine>,
}

fn spawn_tracked(
    mut command: Command,
    label: &'static str,
    runtime: Arc<Mutex<RuntimeState>>,
) -> Result<Child, AppError> {
    suppress_console_window(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| AppError::new("spawn_failed", format!("{label}: {error}")))?;
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
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn ensure_zrok_name(settings: &Settings) -> Result<(), AppError> {
    let mut command = Command::new(zrok_command());
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
    )
    .to_ascii_lowercase();
    if combined.contains("exist") || combined.contains("already") {
        return Ok(());
    }
    Err(AppError::new(
        "zrok_create_name",
        "Could not create or verify the zrok reserved name. Is zrok2 enabled?",
    ))
}

fn command_exists(command: &str) -> bool {
    find_executable(command).is_some()
}

fn zrok_command() -> OsString {
    find_executable("zrok2")
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| OsString::from("zrok2"))
}

fn find_executable(command: &str) -> Option<PathBuf> {
    let command_path = Path::new(command);
    if command_path.is_absolute() && command_path.is_file() {
        return Some(command_path.to_path_buf());
    }

    for extra in extra_search_dirs() {
        if let Some(path) = find_executable_in_dir(&extra, command) {
            return Some(path);
        }
    }

    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        if let Some(path) = find_executable_in_dir(&dir, command) {
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

fn extra_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local_app_data).join("Programs").join("zrok"));
        }
    }
    dirs
}

fn npm_command() -> &'static str {
    if cfg!(windows) {
        "npm.cmd"
    } else {
        "npm"
    }
}

#[cfg(windows)]
fn suppress_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn suppress_console_window(_command: &mut Command) {}
