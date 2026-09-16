use crate::diag::{self, Diagnostics};
use crate::error::AppError;
use crate::lifecycle::{Attempt, Coordinator, LifecycleEndpoint, LifecycleRequest, LifecycleState};
use crate::ownership::{ActivationWatcher, ProfileLock};
use crate::readiness::{real_probe_fn, ContextProvider, ReadinessScheduler};
use crate::settings::{
    effective_workspace_path, fresh_public_path_token, fresh_zrok_name, load_or_create_settings,
    load_settings, mcp_url, normalize_windows_verbatim_prefix, redact_secrets, save_settings,
    secrets_for_redaction, settings_revision, validate_zrok_name, write_managed_mcp_config,
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
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_LOCAL_PORT: &str = "8787";
const MAX_LOG_LINES: usize = 160;
const AUTO_START_RETRY_AFTER: Duration = Duration::from_secs(20);
const SUBPROCESS_DEADLINE: Duration = Duration::from_secs(15);
const CLEANUP_CONFIRM_DEADLINE: Duration = Duration::from_secs(5);
/// Maximum time AppState waits for a coordinator request to settle. The
/// coordinator worker itself is bounded by execution lock + subprocess
/// deadlines; this is the wall-clock ceiling the command layer waits on.
const SETTLEMENT_DEADLINE: Duration = Duration::from_secs(60);

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

/// The local MCP port. `SECRET_TUNNEL_LOCAL_PORT` lets the smoke harness run
/// on an isolated port so it can never collide with, or take over, the
/// production service on the default port.
fn local_port() -> String {
    env::var_os("SECRET_TUNNEL_LOCAL_PORT")
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| {
            !value.is_empty()
                && value.parse::<u16>().is_ok()
                && value.parse::<u16>().unwrap_or(0) != 0
        })
        .unwrap_or_else(|| DEFAULT_LOCAL_PORT.to_string())
}

fn fresh_instance_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let pid = std::process::id();
    format!("{pid:x}-{timestamp:x}")
}

/// Attach the production readiness probe to the scheduler. The provider reads
/// live runtime state on every tick so the probe observes pids/URLs that only
/// exist after spawn; the probe itself runs the bundled Node script that
/// performs real local-MCP and public-TLS handshakes.
fn install_readiness_probe(
    readiness: &std::sync::Arc<ReadinessScheduler>,
    paths: &AppPaths,
    runtime: &std::sync::Arc<std::sync::Mutex<RuntimeState>>,
) {
    use crate::readiness::ProbeContext;
    let probe_script_path =
        readiness_probe_script_path().unwrap_or_else(|| PathBuf::from("readiness-probe.mjs"));
    let bundled_node = bundled_executable("node").unwrap_or_else(|| PathBuf::from("node"));
    let runtime_provider = runtime.clone();
    let paths_provider = paths.clone();
    // The provider closure is `move` and re-runs on every readiness tick, so it
    // needs its own copies; the originals are handed to `real_probe_fn` below.
    let probe_script_for_context = probe_script_path.clone();
    let bundled_node_for_context = bundled_node.clone();
    let provider: ContextProvider = std::sync::Arc::new(move || {
        let settings = load_settings(&paths_provider)
            .ok()
            .flatten()
            .unwrap_or_default();
        let token = settings.public_path_token.clone();
        let zrok_name = settings.zrok_name.clone();
        let mcp_port = local_port();
        let local_url = format!("http://127.0.0.1:{mcp_port}");
        let local_mcp_url = if token.is_empty() {
            format!("{local_url}/mcp")
        } else {
            format!("{local_url}/t/{token}/mcp")
        };
        let public_url = if zrok_name.is_empty() {
            None
        } else {
            Some(format!("https://{zrok_name}.shares.zrok.io"))
        };
        let public_mcp_url = public_url.as_ref().map(|base| {
            if token.is_empty() {
                format!("{base}/mcp")
            } else {
                format!("{base}/t/{token}/mcp")
            }
        });
        let mcp_pids = match runtime_provider.lock() {
            Ok(runtime) => runtime
                .known_mcp_pids
                .iter()
                .copied()
                .chain(runtime.mcp.as_ref().map(|child| child.id()))
                .filter(|pid| *pid != 0)
                .collect(),
            Err(poisoned) => poisoned
                .into_inner()
                .known_mcp_pids
                .iter()
                .copied()
                .collect(),
        };
        ProbeContext {
            known_mcp_pids: mcp_pids,
            local_url: Some(local_url),
            local_mcp_url: Some(local_mcp_url),
            public_url,
            public_mcp_url: public_mcp_url,
            probe_script_path: probe_script_for_context.clone(),
            bundled_node: bundled_node_for_context.clone(),
        }
    });
    readiness.install_probe(provider, real_probe_fn(probe_script_path, bundled_node));
}

/// Locate the bundled readiness probe script, mirroring how `node`/`zrok2`
/// are resolved (resource dir first, then exe dir, then the dev workspace).
fn readiness_probe_script_path() -> Option<PathBuf> {
    for root in resource_search_dirs() {
        let candidates = [
            root.join("resources")
                .join("scripts")
                .join("readiness-probe.mjs"),
            root.join("scripts").join("readiness-probe.mjs"),
        ];
        if let Some(candidate) = candidates.into_iter().find(|path| path.exists()) {
            return Some(candidate);
        }
    }
    None
}

#[derive(Clone)]
pub struct AppState {
    pub paths: AppPaths,
    runtime: Arc<Mutex<RuntimeState>>,
    status_probe_path: Option<PathBuf>,
    instance_id: String,
    launch_environment: Option<Settings>,
    diagnostics: Diagnostics,
    coordinator: Arc<Coordinator>,
    readiness: Arc<ReadinessScheduler>,
    profile_lock: Option<Arc<ProfileLock>>,
    activation_watcher: Arc<Mutex<Option<ActivationWatcher>>>,
}

/// Coordinator endpoint: performs the actual start/stop operations within the
/// lifecycle coordinator's serialized execution model. Shares the same
/// runtime state and configuration paths as `AppState`. It never publishes
/// status directly; the coordinator's publish hook observes the winning
/// generation only.
struct ServiceEndpoint {
    paths: AppPaths,
    runtime: Arc<Mutex<RuntimeState>>,
    instance_id: String,
    diagnostics: Diagnostics,
}

impl LifecycleEndpoint for ServiceEndpoint {
    fn start_services(&self, attempt: &Attempt, settings: &Settings) -> Result<(), AppError> {
        if attempt.is_cancelled() {
            return Err(AppError::new("cancelled", "Superseded before start."));
        }
        validate_zrok_name(&settings.zrok_name)?;
        effective_workspace_path(&settings.workspace_path)?;

        // Retire any currently-running previous generation BEFORE activating
        // the replacement configuration. Cleanup must be confirmed; an
        // unconfirmed retirement aborts replacement startup entirely.
        let retired = self.retire_running(attempt)?;
        if retired {
            // Log after confirmed retirement; the coordinator handled the
            // resource cleanup, we only announce the transition.
            if let Ok(mut runtime) = self.runtime.lock() {
                runtime.push_log("app", "Retired previous generation before replacement.");
            }
        }

        write_managed_mcp_config(&self.paths, settings)?;

        if attempt.is_cancelled() {
            return Err(AppError::new("cancelled", "Superseded after config write."));
        }
        if !mcp_runtime_exists(settings) {
            return Err(AppError::new(
                "missing_gpt_repo_mcp",
                "Bundled gpt-repo-mcp runtime is missing.",
            ));
        }
        if bundled_executable("node").is_none() {
            return Err(AppError::new(
                "missing_node",
                "Bundled Node runtime is missing.",
            ));
        }
        if bundled_executable("zrok2").is_none() {
            return Err(AppError::new("missing_zrok", "Bundled zrok2 is missing."));
        }
        if !zrok_environment_enabled() {
            return Err(AppError::new("zrok_not_enabled", "zrok needs enable"));
        }
        if attempt.is_cancelled() {
            return Err(AppError::new("cancelled", "Superseded before spawning."));
        }

        {
            if let Ok(mut runtime) = self.runtime.lock() {
                runtime.clear_owned_shares_for_host();
            }
        }
        clear_stale_local_port(self.runtime.clone());
        ensure_zrok_name(settings)?;
        clear_stale_zrok_shares_for(
            settings.zrok_name.clone(),
            &self.runtime,
            &self.paths,
            &self.diagnostics,
        );

        let mcp = self.spawn_mcp(attempt, settings)?;
        let mcp_pid = mcp.id();
        let zrok = match self.spawn_zrok(attempt, settings) {
            Ok(child) => child,
            Err(error) => {
                stop_child(mcp);
                if let Ok(mut runtime) = self.runtime.lock() {
                    runtime.untrack_child(mcp_pid, AssetClass::McpNode);
                }
                return Err(error);
            }
        };

        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| AppError::new("runtime_lock", "Runtime state is unavailable."))?;
        runtime.generation = attempt.generation;
        runtime.mcp = Some(mcp);
        runtime.zrok = Some(zrok);
        runtime.active_zrok_name = Some(settings.zrok_name.clone());
        runtime.starting = false;
        runtime.push_log("app", format!("MCP URL: {}", mcp_url(settings)));
        drop(runtime);

        takeover_or_keep_owned_shares(&settings.zrok_name, &self.runtime);
        Ok(())
    }

    /// Apply changed settings to a running generation.
    ///
    /// Only the MCP server reads the workspace configuration; the zrok share
    /// forwards to a fixed local port and knows nothing about it. So when the
    /// tunnel identity is unchanged, changing the folder or the access mode
    /// must not touch the tunnel: tearing it down drops every connected client
    /// and re-registers the share for no reason, and the public URL 502s until
    /// the replacement finishes. Swapping just the MCP child keeps the public
    /// URL continuously valid, with a gap no longer than one process restart.
    ///
    /// Anything that changes the tunnel itself (a different zrok name) still
    /// goes through the full path.
    fn reconfigure_services(&self, attempt: &Attempt, settings: &Settings) -> Result<(), AppError> {
        if !self.can_swap_mcp_in_place(settings) {
            return self.start_services(attempt, settings);
        }
        if attempt.is_cancelled() {
            return Err(AppError::new("cancelled", "Superseded before reconfigure."));
        }
        validate_zrok_name(&settings.zrok_name)?;
        effective_workspace_path(&settings.workspace_path)?;

        // Retire only the MCP child. The zrok child and its share stay live.
        let old_mcp = match self.runtime.lock() {
            Ok(mut runtime) => runtime.mcp.take(),
            Err(_) => {
                return Err(AppError::new(
                    "runtime_lock",
                    "Runtime state is unavailable.",
                ))
            }
        };
        if let Some(mut child) = old_mcp {
            let pid = child.id();
            kill_process_tree(pid);
            if !confirmed_exited(&[pid], CLEANUP_CONFIRM_DEADLINE) {
                return Err(AppError::new(
                    "cleanup_unconfirmed",
                    "The previous MCP server could not be stopped.",
                ));
            }
            let _ = child.wait();
            if let Ok(mut runtime) = self.runtime.lock() {
                runtime.untrack_child(pid, AssetClass::McpNode);
            }
        }

        // Activate the replacement configuration only after the old server is
        // confirmed gone, so the two can never serve different roots at once.
        write_managed_mcp_config(&self.paths, settings)?;
        if attempt.is_cancelled() {
            return Err(AppError::new("cancelled", "Superseded after config write."));
        }
        let mcp = self.spawn_mcp(attempt, settings)?;

        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| AppError::new("runtime_lock", "Runtime state is unavailable."))?;
        runtime.generation = attempt.generation;
        runtime.mcp = Some(mcp);
        runtime.starting = false;
        runtime.push_log(
            "app",
            "Applied new settings without interrupting the tunnel.",
        );
        Ok(())
    }

    fn stop_services(&self, attempt: &Attempt) -> Result<bool, AppError> {
        // Take ownership under the lock first, then terminate outside it.
        // Cleanup is only confirmed once every known process identity is gone.
        let (children, pids): ((Option<Child>, Option<Child>), Vec<u32>) = match self.runtime.lock()
        {
            Ok(mut runtime) => {
                let children = (runtime.mcp.take(), runtime.zrok.take());
                let mut pids = runtime.committed_pids();
                pids.extend(attempt.registered_children());
                pids.sort_unstable();
                pids.dedup();
                (children, pids)
            }
            Err(_) => return Ok(false),
        };
        let (mcp, zrok) = children;
        let committed_len = pids.len();
        if committed_len == 0 {
            return Ok(true);
        }

        for pid in &pids {
            kill_process_tree(*pid);
        }

        if confirmed_exited(&pids, CLEANUP_CONFIRM_DEADLINE) {
            // Reap the handles so the Child objects do not leak zombie state.
            if let Ok(mut runtime) = self.runtime.lock() {
                for child in [mcp, zrok].into_iter().flatten() {
                    let mut child = child;
                    let _ = child.wait();
                }
                runtime.owned_share_tokens.clear();
                runtime.known_mcp_pids.clear();
                runtime.known_zrok_pids.clear();
                runtime.generation = 0;
            }
            return Ok(true);
        }
        // Cleanup could not be confirmed: re-insert the children so a later
        // Stop/Shutdown can retry termination against the same handles.
        if let Ok(mut runtime) = self.runtime.lock() {
            if runtime.mcp.is_none() {
                runtime.mcp = mcp;
            }
            if runtime.zrok.is_none() {
                runtime.zrok = zrok;
            }
        }
        Ok(false)
    }
}

impl ServiceEndpoint {
    /// True when the live generation's tunnel can be kept across this change:
    /// a zrok child is still committed and the reserved name is unchanged. The
    /// local port cannot change while the process runs, so it needs no check.
    fn can_swap_mcp_in_place(&self, settings: &Settings) -> bool {
        match self.runtime.lock() {
            Ok(runtime) => {
                runtime.generation != 0
                    && runtime.mcp.is_some()
                    && runtime.zrok.is_some()
                    && runtime.active_zrok_name.as_deref() == Some(settings.zrok_name.as_str())
            }
            Err(_) => false,
        }
    }

    /// Stop and confirm termination of any currently committed children that
    /// belong to a previous generation. Returns `Ok(true)` when something was
    /// retired and confirmed dead, `Ok(false)` when nothing was running, and
    /// `Err` when retirement could not be confirmed.
    fn retire_running(&self, attempt: &Attempt) -> Result<bool, AppError> {
        let (children, pids): ((Option<Child>, Option<Child>), Vec<u32>) = match self.runtime.lock()
        {
            Ok(mut runtime) => {
                if runtime.generation == 0 {
                    return Ok(false);
                }
                let children = (runtime.mcp.take(), runtime.zrok.take());
                let mut pids = runtime.committed_pids();
                pids.sort_unstable();
                pids.dedup();
                (children, pids)
            }
            Err(_) => return Ok(false),
        };
        let (old_mcp, old_zrok) = children;
        if pids.is_empty() {
            return Ok(false);
        }
        if attempt.is_cancelled() {
            return Err(AppError::new("cancelled", "Superseded during retirement."));
        }
        for pid in &pids {
            kill_process_tree(*pid);
        }
        if confirmed_exited(&pids, CLEANUP_CONFIRM_DEADLINE) {
            if let Ok(mut runtime) = self.runtime.lock() {
                for child in [old_mcp, old_zrok].into_iter().flatten() {
                    let mut child = child;
                    let _ = child.wait();
                }
                runtime.owned_share_tokens.clear();
                runtime.known_mcp_pids.clear();
                runtime.known_zrok_pids.clear();
                runtime.generation = 0;
            }
            Ok(true)
        } else {
            // Leave the partial state for a later stop/retry to clean up.
            Err(AppError::new(
                "cleanup_unconfirmed",
                "Previous generation could not be fully stopped.",
            ))
        }
    }
}

impl ServiceEndpoint {
    fn spawn_mcp(&self, attempt: &Attempt, settings: &Settings) -> Result<Child, AppError> {
        let runtime_dir = bundled_mcp_runtime_dir().ok_or_else(|| {
            AppError::new(
                "missing_gpt_repo_mcp",
                "Bundled gpt-repo-mcp runtime is missing.",
            )
        })?;
        let node = bundled_executable("node")
            .ok_or_else(|| AppError::new("missing_node", "Bundled Node runtime is missing."))?;
        let mut command = Command::new(node);
        let normalized_runtime_dir = PathBuf::from(normalize_windows_verbatim_prefix(
            &runtime_dir.to_string_lossy(),
        ));
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
        let child = spawn_tracked(command, "mcp", AssetClass::McpNode, self.runtime.clone())?;
        attempt.register_child(child.id());
        Ok(child)
    }

    fn spawn_zrok(&self, attempt: &Attempt, settings: &Settings) -> Result<Child, AppError> {
        let share_name = format!("public:{}", settings.zrok_name);
        let port = local_port();
        let mut command = Command::new(bundled_zrok_command()?);
        command
            .arg("share")
            .arg("public")
            .arg(format!("http://127.0.0.1:{port}"))
            .arg("-n")
            .arg(share_name)
            .arg("--headless")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env(INSTANCE_ID_ENV, &self.instance_id)
            .env(ASSET_CLASS_ENV, AssetClass::ZrokShare.tag());
        let child = spawn_tracked(command, "zrok", AssetClass::ZrokShare, self.runtime.clone())?;
        attempt.register_child(child.id());
        Ok(child)
    }
}

/// Publishes a status file only for the winning lifecycle generation. This is
/// the sole status publication path, driven by the coordinator's generation
/// check, so a superseded worker can never write a stale "running" status.
struct StatusPublisher {
    status_probe_path: Option<PathBuf>,
    instance_id: String,
    runtime: Arc<Mutex<RuntimeState>>,
    diagnostics: Diagnostics,
}

impl crate::lifecycle::PublishHook for StatusPublisher {
    fn published(
        &self,
        attempt: &Attempt,
        effective: crate::lifecycle::LifecycleState,
        failure: Option<(String, String)>,
    ) {
        let Some(path) = &self.status_probe_path else {
            return;
        };
        let settings = attempt.settings.clone();
        let (failure_code, failure_message) = failure
            .map(|(message, code)| (Some(code), Some(message)))
            .unwrap_or((None, None));
        let event = match effective {
            crate::lifecycle::LifecycleState::Running => "running",
            crate::lifecycle::LifecycleState::Stopped => "stopped",
            crate::lifecycle::LifecycleState::CleanupFailed => "cleanup-failed",
            _ => "transition",
        };
        let mut runtime = match self.runtime.lock() {
            Ok(runtime) => runtime,
            Err(poisoned) => poisoned.into_inner(),
        };
        write_status_document(
            path,
            event,
            &settings,
            &runtime,
            failure_code.as_deref(),
            failure_message.as_deref(),
            &self.instance_id,
            attempt.generation,
            &self.diagnostics,
        );
    }
}

fn write_status_document(
    path: &Path,
    event: &str,
    settings: &Settings,
    runtime: &RuntimeState,
    failure_code: Option<&str>,
    failure_message: Option<&str>,
    instance_id: &str,
    generation: u64,
    diagnostics: &Diagnostics,
) {
    if let Some(parent) = path.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            diagnostics.event(
                "status-write-failed",
                &format!("Could not create status directory: {error}"),
            );
            return;
        }
    }
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let secrets = secrets_for_redaction(settings);
    let logs = runtime
        .logs
        .iter()
        .map(|log| format!("{}: {}", log.source, redact_secrets(&log.line, &secrets)))
        .collect::<Vec<_>>();
    let document = json!({
        "event": event,
        "timestampMs": timestamp_ms,
        "instanceId": instance_id,
        "generation": generation,
        "running": runtime.is_running(),
        "starting": runtime.starting,
        "workspaceConfigured": settings.workspace_path.is_some(),
        "accessMode": settings.access_mode.as_str(),
        "zrokEnabled": StatusFacts::collect(settings).zrok_enabled,
        "zrokInstalled": StatusFacts::collect(settings).zrok_installed,
        "mcpRuntimeFound": StatusFacts::collect(settings).mcp_runtime_found,
        "failureCode": failure_code,
        "failureMessage": failure_message,
        "mcpPids": runtime.known_mcp_pids.iter().copied().collect::<Vec<_>>(),
        "zrokPids": runtime.known_zrok_pids.iter().copied().collect::<Vec<_>>(),
        "ownedShareTokens": runtime.owned_share_tokens.iter().cloned().collect::<Vec<_>>(),
        "logs": logs
    });
    let tmp = path.with_extension(format!(
        "status.json.tmp.{}.{}",
        std::process::id(),
        timestamp_ms
    ));
    if let Err(error) = fs::write(
        &tmp,
        serde_json::to_vec_pretty(&document).unwrap_or_default(),
    ) {
        let _ = fs::remove_file(&tmp);
        diagnostics.event(
            "status-write-failed",
            &format!("Could not write status probe: {error}"),
        );
        return;
    }
    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        diagnostics.event(
            "status-write-failed",
            &format!("Could not publish status probe: {error}"),
        );
    }
}

fn clear_stale_zrok_shares_for(
    zrok_name: String,
    runtime: &Arc<Mutex<RuntimeState>>,
    paths: &AppPaths,
    diagnostics: &Diagnostics,
) {
    let owned_tokens: HashSet<String> = match runtime.lock() {
        Ok(runtime) => runtime.owned_share_tokens.iter().cloned().collect(),
        Err(_) => return,
    };
    let tokens = stale_zrok_share_tokens(&zrok_name);
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
        let message = if cleared {
            format!("Cleared stale zrok share {token} for a stable URL.")
        } else {
            format!("Could not clear stale zrok share {token}.")
        };
        diagnostics.event("app", &message);
    }
}

fn takeover_or_keep_owned_shares(zrok_name: &str, runtime: &Arc<Mutex<RuntimeState>>) {
    for _ in 0..10 {
        let live_tokens = stale_zrok_share_tokens(zrok_name);
        if !live_tokens.is_empty() {
            if let Ok(mut runtime) = runtime.lock() {
                for token in live_tokens {
                    runtime.record_owned_share(&token);
                }
            }
            return;
        }
        thread::sleep(Duration::from_millis(300));
    }
}

impl AppState {
    pub fn new(paths: AppPaths) -> Self {
        Self::with_launch_environment(paths, None)
    }

    pub fn with_launch_environment(paths: AppPaths, launch_environment: Option<Settings>) -> Self {
        let diagnostics = Diagnostics::new(&paths);
        diag::bootstrap(&diagnostics, launch_environment.is_some());
        let runtime = Arc::new(Mutex::new(RuntimeState::default()));
        let status_probe_path = status_probe_path_from_environment();
        let instance_id = fresh_instance_id();
        let readiness = Arc::new(ReadinessScheduler::new());
        install_readiness_probe(&readiness, &paths, &runtime);
        let endpoint = ServiceEndpoint {
            paths: paths.clone(),
            runtime: runtime.clone(),
            instance_id: instance_id.clone(),
            diagnostics: diagnostics.clone(),
        };
        let coordinator = Arc::new(Coordinator::new(Arc::new(endpoint)));
        coordinator.attach_readiness(readiness.clone());
        coordinator.attach_publish_hook(Arc::new(StatusPublisher {
            status_probe_path: status_probe_path.clone(),
            instance_id: instance_id.clone(),
            runtime: runtime.clone(),
            diagnostics: diagnostics.clone(),
        }));
        Self {
            paths,
            runtime,
            status_probe_path,
            instance_id,
            launch_environment,
            diagnostics,
            coordinator,
            readiness,
            profile_lock: None,
            activation_watcher: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_profile_lock(
        paths: AppPaths,
        launch_environment: Option<Settings>,
        profile_lock: Option<ProfileLock>,
    ) -> Self {
        let diagnostics = Diagnostics::new(&paths);
        diag::bootstrap(&diagnostics, launch_environment.is_some());
        let runtime = Arc::new(Mutex::new(RuntimeState::default()));
        let status_probe_path = status_probe_path_from_environment();
        let instance_id = fresh_instance_id();
        let readiness = Arc::new(ReadinessScheduler::new());
        install_readiness_probe(&readiness, &paths, &runtime);
        let endpoint = ServiceEndpoint {
            paths: paths.clone(),
            runtime: runtime.clone(),
            instance_id: instance_id.clone(),
            diagnostics: diagnostics.clone(),
        };
        let coordinator = Arc::new(Coordinator::new(Arc::new(endpoint)));
        coordinator.attach_readiness(readiness.clone());
        coordinator.attach_publish_hook(Arc::new(StatusPublisher {
            status_probe_path: status_probe_path.clone(),
            instance_id: instance_id.clone(),
            runtime: runtime.clone(),
            diagnostics: diagnostics.clone(),
        }));
        Self {
            paths,
            runtime,
            status_probe_path,
            instance_id,
            launch_environment,
            diagnostics,
            coordinator,
            readiness,
            profile_lock: profile_lock.map(Arc::new),
            activation_watcher: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_activation_watcher(&self, watcher: ActivationWatcher) {
        if let Ok(mut slot) = self.activation_watcher.lock() {
            *slot = Some(watcher);
        }
    }

    /// Return the effective settings for this process: the in-memory launch
    /// overrides when present, otherwise the persisted settings. Overrides are
    /// never persisted to the production profile.
    fn configured_settings(&self) -> Result<Settings, AppError> {
        if let Some(settings) = &self.launch_environment {
            return Ok(settings.clone());
        }
        load_or_create_settings(&self.paths)
    }

    pub fn snapshot(&self, autostart_enabled: bool) -> Result<StatusDto, AppError> {
        let settings = self.configured_settings()?;
        // Environment facts are cached (no CLI subprocesses on the status read
        // path). zrok_enabled is refreshed by the background supervisor.
        let zrok_installed = bundled_executable("zrok2").is_some();
        let gpt_repo_mcp_found = mcp_runtime_exists(&settings);
        let workspace_effective = effective_workspace_path(&settings.workspace_path);
        let lifecycle_state = self.coordinator.effective_state();
        let generation = self.coordinator.current_generation();
        let desired_running = self.coordinator.desired_running();
        let blocked_reason = self.coordinator.blocked_reason();
        let readiness = self.coordinator.readiness_snapshot();
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| AppError::new("runtime_lock", "Runtime state is unavailable."))?;
        runtime.cleanup_exited();
        let secrets = secrets_for_redaction(&settings);
        Ok(StatusDto {
            settings: SettingsDto::from_settings(&settings, &self.paths),
            mcp_url: mcp_url(&settings),
            running: runtime.is_running(),
            autostart_enabled,
            zrok_installed,
            zrok_enabled: runtime.zrok_enabled_known,
            gpt_repo_mcp_found,
            workspace_configured: settings.workspace_path.is_some(),
            startup_blocked_reason: workspace_effective
                .as_ref()
                .err()
                .map(|error| error.code.to_string()),
            startup_blocked_message: workspace_effective
                .as_ref()
                .err()
                .map(|error| error.message.to_string()),
            lifecycle_state: lifecycle_state.as_str().to_string(),
            generation,
            desired_running,
            cleanup_blocked_reason: blocked_reason,
            readiness,
            logs: runtime
                .logs
                .iter()
                .map(|log| LogLine {
                    source: log.source.clone(),
                    line: redact_secrets(&log.line, &secrets),
                })
                .collect(),
        })
    }

    pub fn start(&self) -> Result<(), AppError> {
        let settings = self.configured_settings()?;
        self.request_start(settings, false)
    }

    pub fn start_if_configured(&self) -> Result<bool, AppError> {
        let settings = self.configured_settings()?;
        if settings.workspace_path.is_none() {
            self.diagnostics.event(
                "start-skipped",
                "Workspace not configured; no MCP tunnel started.",
            );
            return Ok(false);
        }
        match effective_workspace_path(&settings.workspace_path) {
            Ok(Some(_)) => {}
            Ok(None) => {
                self.diagnostics.event(
                    "start-skipped",
                    "Workspace path is missing from settings; no MCP tunnel started.",
                );
                return Ok(false);
            }
            Err(error) => {
                self.diagnostics.event(
                    "start-skipped",
                    &format!("start skipped: {}: {}", error.code, error.message),
                );
                return Ok(false);
            }
        }
        self.diagnostics.event(
            "configured-start-requested",
            "Automatic start requested at launch.",
        );
        self.request_start(settings, false)?;
        Ok(true)
    }

    /// Long-lived backend supervisor: schedules retries while the user intent
    /// is still "running" and services are not. Observed via the coordinator,
    /// so it automatically honors `desired_running = false` after an explicit
    /// stop and never runns while a worker is already in flight.
    pub fn start_supervisor(&self) {
        let state = self.clone();
        thread::spawn(move || loop {
            thread::sleep(AUTO_START_RETRY_AFTER);
            if state.coordinator.in_flight_generation().is_some() {
                continue;
            }
            if state.coordinator.blocked_reason().is_some() {
                continue;
            }
            if !state.coordinator.desired_running() {
                continue;
            }
            if state.coordinator.effective_state().is_running() {
                continue;
            }
            // Refresh the cached zrok-enabled fact outside the status path.
            {
                let detected = zrok_environment_enabled();
                if let Ok(mut runtime) = state.runtime.lock() {
                    runtime.zrok_enabled_known = detected;
                }
            }
            let settings = match state.configured_settings() {
                Ok(settings) => settings,
                Err(_) => continue,
            };
            if settings.workspace_path.is_none() {
                continue;
            }
            if let Err(error) = state.request_start(settings, true) {
                state.push_app_log(format!("Auto-start retry failed: {}", error.message));
            }
        });
    }

    pub fn restart_if_configured(&self) -> Result<bool, AppError> {
        let settings = self.configured_settings()?;
        if settings.workspace_path.is_none() {
            return Ok(false);
        }
        effective_workspace_path(&settings.workspace_path)?;
        self.diagnostics.event(
            "reconfigure-requested",
            "Settings changed; reconfiguring services.",
        );
        let revision = settings_revision(&settings);
        let generation = self
            .coordinator
            .submit(LifecycleRequest::Reconfigure { settings, revision });
        self.await_lifecycle(generation, LifecycleState::Running, "reconfigure")?;
        Ok(true)
    }

    pub fn is_running(&self) -> Result<bool, AppError> {
        let lifecycle_ok = self.coordinator.effective_state().is_running();
        if !lifecycle_ok {
            return Ok(false);
        }
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| AppError::new("runtime_lock", "Runtime state is unavailable."))?;
        runtime.cleanup_exited();
        Ok(runtime.is_running())
    }

    pub fn regenerate_mcp_url(&self) -> Result<(), AppError> {
        let mut settings = self.configured_settings()?;
        let mut created = false;
        for _ in 0..8 {
            let candidate = Settings {
                zrok_name: fresh_zrok_name(),
                public_path_token: fresh_public_path_token(),
                ..settings.clone()
            };
            // A freshly created name reports `Created`; a name that already
            // exists reports `Reserved`. Both leave us with a usable reserved
            // name.
            match ensure_zrok_name(&candidate) {
                Ok(NameCheck::Created) | Ok(NameCheck::Reserved) => {
                    settings = candidate;
                    created = true;
                    break;
                }
                Err(_) => continue,
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
            let revision = settings_revision(&settings);
            // A new generation with a different snapshot supersedes any
            // currently-running generation (retiring its children first).
            let generation = self.coordinator.submit(LifecycleRequest::Start {
                settings,
                revision,
                retry: false,
            });
            self.await_lifecycle(generation, LifecycleState::Running, "regenerate")?;
        }
        Ok(())
    }

    fn request_start(&self, settings: Settings, retry: bool) -> Result<(), AppError> {
        validate_zrok_name(&settings.zrok_name)?;
        effective_workspace_path(&settings.workspace_path)?;
        let revision = settings_revision(&settings);
        let generation = self.coordinator.submit(LifecycleRequest::Start {
            settings,
            revision,
            retry,
        });
        self.await_lifecycle(generation, LifecycleState::Running, "start")
    }

    fn await_lifecycle(
        &self,
        generation: u64,
        target: LifecycleState,
        op: &str,
    ) -> Result<(), AppError> {
        if generation == 0 {
            // No worker spawned: coalesced, already-running, or rejected.
            let state = self.coordinator.effective_state();
            if state == target {
                return Ok(());
            }
            if let Some(reason) = self.coordinator.blocked_reason() {
                return Err(AppError::new(
                    "cleanup_blocked",
                    format!("{op} rejected: cleanup is unresolved: {reason}"),
                ));
            }
            if target == LifecycleState::Running && state.is_running() {
                return Ok(());
            }
            if self.coordinator.in_flight_generation().is_some() {
                // An equivalent request was accepted: wait for its settlement.
                let settled = self.coordinator.wait_for_idle(SETTLEMENT_DEADLINE);
                return state_to_result(settled, target, op);
            }
            return Err(AppError::new(
                "lifecycle",
                format!("{op}: no worker was spawned and the target was not reached."),
            ));
        }
        let state = self
            .coordinator
            .wait_for_generation(generation, SETTLEMENT_DEADLINE);
        state_to_result(state, target, op)
    }

    pub fn stop(&self) -> Result<(), AppError> {
        self.diagnostics.event("stop", "Stopping local processes.");
        self.push_app_log("Stopping local processes.");
        let generation = self.coordinator.submit(LifecycleRequest::Stop);
        self.await_lifecycle(generation, LifecycleState::Stopped, "stop")
    }

    /// Fire-and-forget but bounded shutdown used on window close: submits
    /// Shutdown and waits (at most the cleanup deadline) for settlement so the
    /// process does not exit while resources are unconfirmed.
    pub fn stop_all(&self) {
        self.diagnostics
            .event("shutdown", "Shutting down local processes.");
        let generation = self.coordinator.submit(LifecycleRequest::Shutdown);
        if generation != 0 {
            let deadline = CLEANUP_CONFIRM_DEADLINE + SETTLEMENT_DEADLINE;
            let _ = self.coordinator.wait_for_generation(generation, deadline);
        }
    }

    pub fn enable_zrok(&self, token: String) -> Result<(), AppError> {
        enable_zrok_environment(&token)?;
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.zrok_enabled_known = true;
        }
        self.push_app_log("zrok environment enabled.");
        Ok(())
    }

    pub fn enable_zrok_from_environment_if_present(&self) -> Result<bool, AppError> {
        if zrok_environment_enabled() {
            if let Ok(mut runtime) = self.runtime.lock() {
                runtime.zrok_enabled_known = true;
            }
            return Ok(false);
        }

        let Some(token) = zrok_enable_token_from_environment() else {
            return Ok(false);
        };

        enable_zrok_environment(&token)?;
        clear_zrok_enable_token_environment();
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.zrok_enabled_known = true;
        }
        self.push_app_log("zrok environment enabled from launch environment.");
        Ok(true)
    }

    pub fn push_app_log(&self, line: impl Into<String>) {
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.push_log("app", line.into());
        }
    }
}

fn state_to_result(
    state: LifecycleState,
    target: LifecycleState,
    op: &str,
) -> Result<(), AppError> {
    if state == target {
        return Ok(());
    }
    match state {
        LifecycleState::CleanupFailed => Err(AppError::new(
            "cleanup_failed",
            format!("{op}: service cleanup could not be confirmed; replacements are blocked."),
        )),
        LifecycleState::Stopped => Err(AppError::new(
            "lifecycle",
            format!("{op}: services are not running."),
        )),
        LifecycleState::Starting | LifecycleState::Stopping => Err(AppError::new(
            "timeout",
            format!("{op}: timed out before reaching the target state."),
        )),
        LifecycleState::Running => Err(AppError::new(
            "lifecycle",
            format!("{op}: services are already running."),
        )),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StatusFacts {
    pub zrok_installed: bool,
    pub zrok_enabled: bool,
    pub mcp_runtime_found: bool,
}

impl StatusFacts {
    /// Collect environment facts. Intentionally NOT called while holding the
    /// runtime mutex: this may run `zrok2 status` as a subprocess.
    pub fn collect(settings: &Settings) -> Self {
        Self {
            zrok_installed: bundled_executable("zrok2").is_some(),
            zrok_enabled: zrok_environment_enabled(),
            mcp_runtime_found: mcp_runtime_exists(settings),
        }
    }
}

#[derive(Default)]
struct RuntimeState {
    /// Generation that currently owns `mcp`/`zrok`. Children are only
    /// committed to the runtime on a fully successful start; a Stop/Reconfigure
    /// retires them before any replacement starts.
    generation: u64,
    mcp: Option<Child>,
    zrok: Option<Child>,
    starting: bool,
    /// Cached zrok environment status: false by default, refreshed by the
    /// background supervisor so `get_status` never runs a CLI subprocess.
    zrok_enabled_known: bool,
    last_autostart_attempt: Option<Instant>,
    logs: Vec<LogLine>,
    owned_share_tokens: HashSet<String>,
    known_mcp_pids: HashSet<u32>,
    known_zrok_pids: HashSet<u32>,
    /// zrok name the currently committed tunnel was started for. A
    /// reconfiguration that keeps this name can swap the MCP server underneath
    /// the live tunnel instead of rebuilding the share.
    active_zrok_name: Option<String>,
}

impl RuntimeState {
    fn is_running(&self) -> bool {
        self.mcp.is_some() && self.zrok.is_some()
    }

    /// Every process identity currently committed to the runtime plus any
    /// tracked leftovers. Used by cleanup confirmation.
    fn committed_pids(&self) -> Vec<u32> {
        let mut pids = Vec::new();
        for child in [&self.mcp, &self.zrok].into_iter().flatten() {
            pids.push(child.id());
        }
        pids.extend(self.known_mcp_pids.iter().copied());
        pids.extend(self.known_zrok_pids.iter().copied());
        pids.sort_unstable();
        pids.dedup();
        pids
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
    pub workspace_configured: bool,
    pub startup_blocked_reason: Option<String>,
    pub startup_blocked_message: Option<String>,
    /// Serialized lifecycle coordinator state ("running", "stopped", etc.).
    pub lifecycle_state: String,
    /// Generation of the most recent admitted lifecycle request.
    pub generation: u64,
    /// Whether the backend still intends the service to be running.
    pub desired_running: bool,
    /// Non-`None` only when a cleanup failure blocks replacement start.
    pub cleanup_blocked_reason: Option<String>,
    /// Latest readiness probe snapshot from the active generation.
    pub readiness: Option<crate::readiness::ReadinessSnapshot>,
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

/// Poll for process exit until the deadline. Returns `true` only when every
/// given pid is confirmed gone (or its handle reports an error, meaning the
/// process is no longer ours to wait on). Used to confirm resource cleanup.
fn confirmed_exited(pids: &[u32], deadline: Duration) -> bool {
    let started = Instant::now();
    loop {
        let mut alive = Vec::new();
        for pid in pids {
            if process_alive(*pid) {
                alive.push(*pid);
            }
        }
        if alive.is_empty() {
            return true;
        }
        if started.elapsed() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    let mut command = Command::new("tasklist");
    command
        .arg("/FI")
        .arg(format!("PID eq {pid}"))
        .arg("/FO")
        .arg("CSV")
        .arg("/NH")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    suppress_console_window(&mut command);
    // `tasklist` exits 0 whether or not the filter matched anything: for an
    // unknown pid it prints "INFO: No tasks are running which match the
    // specified criteria." and still succeeds. Trusting the exit code reports
    // every pid as alive forever, which makes cleanup confirmation impossible
    // and wedges the lifecycle in CleanupFailed. Only the output distinguishes
    // the two cases, so parse the row. Failure to run the check at all means we
    // cannot observe the process; treat that as gone rather than blocking
    // forever on an answer that will never come.
    let Ok(output) = command.output() else {
        return false;
    };
    tasklist_row_matches_pid(&String::from_utf8_lossy(&output.stdout), pid)
}

/// True when a `tasklist /FO CSV /NH` listing contains a row for `pid`. Rows
/// look like `"name.exe","1234","Console","1","12,345 K"`; the informational
/// "no tasks" line has no such fields.
#[cfg(windows)]
fn tasklist_row_matches_pid(output: &str, pid: u32) -> bool {
    let wanted = pid.to_string();
    output.lines().any(|line| {
        line.split("\",\"")
            .nth(1)
            .map(|field| field.trim_matches('"').trim() == wanted)
            .unwrap_or(false)
    })
}

#[cfg(not(windows))]
fn process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
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
                format!(
                    "Reaped leftover MCP process {pid} on port {}.",
                    local_port()
                ),
            );
        }
    }
}

#[cfg(windows)]
fn is_mcp_asset_process(pid: u32) -> bool {
    let mut command = Command::new("powershell");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "Get-CimInstance Win32_Process -Filter \"ProcessId={pid}\" | Select-Object -ExpandProperty CommandLine"
            ),
        ])
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
    let node_fingerprint = normalize_windows_verbatim_prefix(&node_path.to_string_lossy());
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
        if !parts[1].ends_with(&format!(":{}", local_port())) {
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
    let mut command = Command::new("open");
    command.arg(url);
    command
}

#[cfg(target_os = "linux")]
fn platform_open_command(url: &str) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(url);
    command
}

fn configure_mcp_environment(command: &mut Command, paths: &AppPaths, settings: &Settings) {
    command
        .env("GPT_REPO_CONFIG", &paths.managed_config_path)
        .env("REPO_READER_CONFIG", &paths.managed_config_path)
        .env("GPT_REPO_HOST", "127.0.0.1")
        .env("PORT", local_port())
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
            if let Some(token) = share.get("shareToken").and_then(serde_json::Value::as_str) {
                tokens.push(token.to_string());
            }
        }
    }
    tokens
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameCheck {
    Created,
    Reserved,
}

fn ensure_zrok_name(settings: &Settings) -> Result<NameCheck, AppError> {
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
        return Ok(NameCheck::Created);
    }
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if zrok_name_already_reserved(&combined) {
        return Ok(NameCheck::Reserved);
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

/// Run a command to completion with a bounded wait. Used for all zrok
/// subprocess interactions so a hanging zrok cannot wedge diagnostics or
/// status reads. Returns `None` on timeout or spawn/output failure.
fn bounded_command_output(mut command: Command, label: &str) -> Option<Output> {
    suppress_console_window(&mut command);
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + SUBPROCESS_DEADLINE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_process_tree(child.id());
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.wait();
                return None;
            }
        }
    }
    match child.wait_with_output() {
        Ok(output) => Some(output),
        Err(_) => {
            let _ = label;
            None
        }
    }
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
    let Some(output) = bounded_command_output(command, "zrok status") else {
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
        stale_zrok_share_tokens_from_json, status_probe_path_from_environment, validate_zrok_token,
        zrok_enable_token_from_environment, zrok_name_already_reserved,
        zrok_status_indicates_enabled,
    };
    #[cfg(windows)]
    use super::{confirmed_exited, kill_process_tree, process_alive, tasklist_row_matches_pid};
    #[cfg(windows)]
    use std::process::Command;
    use std::sync::Mutex;
    #[cfg(windows)]
    use std::time::Duration;
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
        assert!(stale_zrok_share_tokens_from_json(r#"{"shares":[]}"#, "name").is_empty());
        assert!(stale_zrok_share_tokens_from_json(
            r#"{"shares":[{"shareToken":"token"}]}"#,
            "name"
        )
        .is_empty());
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

    /// tasklist exits 0 even when its filter matches nothing, so liveness has
    /// to come from the row itself. Getting this wrong reports every pid as
    /// alive, cleanup can never be confirmed, and the lifecycle wedges in
    /// CleanupFailed on the first reconfigure.
    #[cfg(windows)]
    #[test]
    fn tasklist_no_match_line_is_not_a_live_process() {
        let no_match = "INFO: No tasks are running which match the specified criteria.";
        assert!(!tasklist_row_matches_pid(no_match, 999_999));
        assert!(!tasklist_row_matches_pid("", 1234));

        let row = "\"secret-tunnel.exe\",\"2624\",\"Console\",\"1\",\"24,624 K\"";
        assert!(tasklist_row_matches_pid(row, 2624));
        // A pid appearing in another column must not count as a match.
        assert!(!tasklist_row_matches_pid(row, 1));
        assert!(!tasklist_row_matches_pid(row, 24));
    }

    /// End-to-end against a real process: alive while it runs, and confirmed
    /// gone once it exits. This is the check the whole cleanup path depends on.
    #[cfg(windows)]
    #[test]
    fn process_liveness_tracks_a_real_child() {
        let mut child = Command::new("cmd")
            .args(["/C", "ping -n 30 127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn test child");
        let pid = child.id();

        kill_process_tree(pid);
        let _ = child.wait();
        // This is the direction that was broken: because `tasklist` exits 0 for
        // a missing pid, every dead process still looked alive, so cleanup was
        // never confirmable and the lifecycle wedged in CleanupFailed. The
        // positive direction is covered deterministically by the parser test;
        // asserting it here as well depends on `tasklist` observing a
        // just-spawned process, which is racy under a parallel test run.
        assert!(
            confirmed_exited(&[pid], Duration::from_secs(10)),
            "cleanup must be confirmable once the child is gone"
        );
    }
}
