use crate::lifecycle::{Attempt, ReadinessController};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Bound for one probe cycle, including the bundled-node child process.
/// `invalidate` never joins the probe thread, so shutdown is never blocked
/// by a hung probe: the thread simply stops publishing after this deadline.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(20);

/// Delay between probe samples while a generation is running.
const PROBE_CADENCE: Duration = Duration::from_secs(2);

/// A single readiness gate. `kind` names the gate for status/report output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProbeStatus {
    Pending,
    Verifying,
    Verified,
    Failed,
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeEntry {
    pub kind: String,
    pub status: ProbeStatus,
    pub checked_at: Option<String>,
    pub detail: Option<String>,
}

fn gate(kind: &'static str) -> ProbeEntry {
    ProbeEntry {
        kind: kind.to_string(),
        status: ProbeStatus::Pending,
        checked_at: None,
        detail: None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessSnapshot {
    /// MCP server process (local) alive, spawned by this attempt.
    pub local_process: ProbeEntry,
    /// Local HTTP MCP endpoint responds.
    pub local_endpoint: ProbeEntry,
    /// MCP protocol handshake + tool surface on the local endpoint.
    pub local_protocol: ProbeEntry,
    /// zrok share endpoints reachable through the public tunnel.
    pub public_tunnel: ProbeEntry,
    /// Public MCP protocol handshake against the tunnel.
    pub public_protocol: ProbeEntry,
    /// Lifecycle generation this snapshot belongs to. `publish` only writes
    /// snapshots whose generation matches the currently scheduled one, so a
    /// superseded shard can never resurrect readiness.
    pub generation: u64,
    /// Settings revision the sample was probed against. Distinct from the
    /// generation: reconfiguration bumps the revision and the generation.
    pub verified_revision: Option<u64>,
    /// True when any gate is currently Failed.
    pub degraded: bool,
    pub issues: Vec<String>,
}

impl ReadinessSnapshot {
    pub fn pending() -> Self {
        Self {
            local_process: gate("localProcess"),
            local_endpoint: gate("localEndpoint"),
            local_protocol: gate("localProtocol"),
            public_tunnel: gate("publicTunnel"),
            public_protocol: gate("publicProtocol"),
            generation: 0,
            verified_revision: None,
            degraded: false,
            issues: Vec::new(),
        }
    }

    fn for_generation(generation: u64) -> Self {
        Self {
            generation,
            ..Self::pending()
        }
    }

    fn gates(&self) -> [&ProbeEntry; 5] {
        [
            &self.local_process,
            &self.local_endpoint,
            &self.local_protocol,
            &self.public_tunnel,
            &self.public_protocol,
        ]
    }

    /// Ready for local MCP use: the server process is alive and both the HTTP
    /// endpoint and the MCP protocol handshake verify on the loopback URL.
    pub fn is_local_ready(&self) -> bool {
        self.local_process.status != ProbeStatus::Pending
            && self.local_process.status != ProbeStatus::Failed
            && self.local_endpoint.status == ProbeStatus::Verified
            && self.local_protocol.status == ProbeStatus::Verified
    }

    /// Ready through the public tunnel: both the tunnel reachability and the
    /// public MCP handshake must verify.
    pub fn is_public_ready(&self) -> bool {
        self.public_tunnel.status == ProbeStatus::Verified
            && self.public_protocol.status == ProbeStatus::Verified
    }

    /// Fully ready: local and public gates all verified. A readiness claim for
    /// ChatGPT requires the public path; local-only verification is not enough.
    pub fn is_ready(&self) -> bool {
        !self.degraded && self.is_local_ready() && self.is_public_ready()
    }

    fn recompute(&mut self) {
        self.degraded = self
            .gates()
            .iter()
            .any(|entry| entry.status == ProbeStatus::Failed);
        self.issues = self
            .gates()
            .iter()
            .filter_map(|entry| {
                if entry.status == ProbeStatus::Failed {
                    Some(format!(
                        "{}: {}",
                        entry.kind,
                        entry.detail.as_deref().unwrap_or("failed")
                    ))
                } else {
                    None
                }
            })
            .collect();
    }
}

/// Everything a probe needs to observe the running services. Provided by the
/// process module, kept generic here so readiness is unit-testable.
#[derive(Clone)]
pub struct ProbeContext {
    /// PIDs of the MCP processes committed to the current generation.
    pub known_mcp_pids: Vec<u32>,
    /// Local health base, e.g. `http://127.0.0.1:8787`.
    pub local_url: Option<String>,
    /// Local MCP protocol endpoint, e.g. `http://127.0.0.1:8787/t/{token}/mcp`.
    pub local_mcp_url: Option<String>,
    /// Public tunnel base, e.g. `https://gptmcp-xxxx.shares.zrok.io`.
    pub public_url: Option<String>,
    /// Public MCP protocol endpoint.
    pub public_mcp_url: Option<String>,
    pub probe_script_path: PathBuf,
    pub bundled_node: PathBuf,
}

/// Supplies a fresh context each probe tick so pids/URLs that only exist after
/// spawn are observed without reinstalling the probe.
pub type ContextProvider = Arc<dyn Fn() -> ProbeContext + Send + Sync>;

/// Returns the next full snapshot for the given context, annotating each gate.
pub type ProbeFn =
    Arc<dyn Fn(&ProbeContext, &ReadinessSnapshot) -> ReadinessSnapshot + Send + Sync>;

struct SchedulerState {
    /// Latest published snapshot. Written only when the writer's generation
    /// matches `generation` under the same lock, making publication atomic
    /// with the generation check.
    snapshot: ReadinessSnapshot,
    /// Generation that currently owns probing. 0 means idle/invalidated.
    generation: u64,
    /// Settings revision the current generation was scheduled for.
    revision: u64,
    running: bool,
    context_provider: Option<ContextProvider>,
    probe: Option<ProbeFn>,
}

/// Probe scheduler attached to the coordinator. `schedule_for` records the
/// attempt generation/revision and starts a probe loop; `invalidate` (called
/// on stop/reconfigure) atomically resets state. `invalidate` never joins the
/// loop, so a hung probe (up to `PROBE_DEADLINE`) cannot block shutdown.
pub struct ReadinessScheduler {
    /// Read-only mirror of the state generation for cheap `generation()`.
    generation_handle: AtomicU64,
    state: Arc<Mutex<SchedulerState>>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl ReadinessScheduler {
    pub fn new() -> Self {
        Self {
            generation_handle: AtomicU64::new(0),
            state: Arc::new(Mutex::new(SchedulerState {
                snapshot: ReadinessSnapshot::pending(),
                generation: 0,
                revision: 0,
                running: false,
                context_provider: None,
                probe: None,
            })),
            thread: Mutex::new(None),
        }
    }

    pub fn install_probe(&self, context_provider: ContextProvider, probe: ProbeFn) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.context_provider = Some(context_provider);
        state.probe = Some(probe);
    }

    /// Spawn (or replace) the probe loop for the scheduled generation. Always
    /// starts a fresh loop so re-scheduling after an invalidate restarts
    /// probing; any lingering thread exits on its own when it observes the
    /// generation mismatch or `running == false`.
    fn spawn_loop(&self, generation: u64, revision: u64) {
        let running;
        {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            running = state.running;
            state.generation = generation;
            state.revision = revision;
            state.snapshot = ReadinessSnapshot::for_generation(generation);
        }
        if !running {
            return;
        }
        self.generation_handle.store(generation, Ordering::Release);
        let state = self.state.clone();
        let mut thread_slot = match self.thread.lock() {
            Ok(thread) => thread,
            Err(poisoned) => poisoned.into_inner(),
        };
        *thread_slot = Some(thread::spawn(move || loop {
            // Each tick reads the generation/running gate under the same lock
            // that later guards the snapshot write.
            let (tick_generation, revision) = {
                let guard = match state.lock() {
                    Ok(state) => state,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if !guard.running || guard.generation == 0 {
                    break;
                }
                (guard.generation, guard.revision)
            };
            let (context, probe, prior) = {
                let guard = match state.lock() {
                    Ok(state) => state,
                    Err(poisoned) => poisoned.into_inner(),
                };
                let Some(provider) = guard.context_provider.clone() else {
                    thread::sleep(PROBE_CADENCE);
                    continue;
                };
                let Some(probe) = guard.probe.clone() else {
                    thread::sleep(PROBE_CADENCE);
                    continue;
                };
                (provider(), probe, guard.snapshot.clone())
            };
            let mut snapshot = probe(&context, &prior);
            snapshot.verified_revision = Some(revision);
            snapshot.generation = tick_generation;
            snapshot.recompute();
            {
                let mut guard = match state.lock() {
                    Ok(state) => state,
                    Err(poisoned) => poisoned.into_inner(),
                };
                // Atomic publication gate: only the generation that was still
                // scheduled at write time may publish. A superseded or
                // invalidated generation writes nothing and exits.
                if !guard.running || guard.generation != tick_generation {
                    break;
                }
                guard.snapshot = snapshot;
            }
            thread::sleep(PROBE_CADENCE);
        }));
    }
}

impl Default for ReadinessScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadinessController for ReadinessScheduler {
    fn generation(&self) -> u64 {
        self.generation_handle.load(Ordering::Acquire)
    }

    fn schedule_for(&self, attempt: &Attempt) {
        {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.running = true;
            state.generation = attempt.generation;
            state.revision = attempt.revision;
            state.snapshot = ReadinessSnapshot::for_generation(attempt.generation);
        }
        self.generation_handle
            .store(attempt.generation, Ordering::Release);
        self.spawn_loop(attempt.generation, attempt.revision);
    }

    fn invalidate(&self) {
        self.generation_handle.store(0, Ordering::Release);
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.running = false;
        state.generation = 0;
        state.revision = 0;
        state.snapshot = ReadinessSnapshot::pending();
        // Deliberately do NOT join the probe thread: a probe mid-flight is
        // bounded by PROBE_DEADLINE and will exit at its next write gate.
    }

    fn current(&self) -> ReadinessSnapshot {
        match self.state.lock() {
            Ok(state) => state.snapshot.clone(),
            Err(poisoned) => poisoned.into_inner().snapshot.clone(),
        }
    }
}

/// Mapping from the bundled-node probe's JSON gate report to internal status.
fn map_probe_status(raw: &str) -> ProbeStatus {
    match raw {
        "verified" => ProbeStatus::Verified,
        "failed" => ProbeStatus::Failed,
        "unsupported" => ProbeStatus::Unsupported,
        "verifying" => ProbeStatus::Verifying,
        _ => ProbeStatus::Pending,
    }
}

#[derive(Deserialize)]
struct GatewayReport {
    #[serde(default)]
    status: String,
    #[serde(default)]
    detail: Option<String>,
}

#[derive(Deserialize)]
struct ProbeReport {
    #[serde(default)]
    localProcess: Option<GatewayReport>,
    #[serde(default)]
    localEndpoint: Option<GatewayReport>,
    #[serde(default)]
    localProtocol: Option<GatewayReport>,
    #[serde(default)]
    publicTunnel: Option<GatewayReport>,
    #[serde(default)]
    publicProtocol: Option<GatewayReport>,
}

/// The production probe: runs the bundled `readiness-probe.mjs` through the
/// bundled Node, bounded by `PROBE_DEADLINE`, and maps each gate report.
/// Missing gates become `Pending`; a crashed/timeout probe marks every gate
/// `Failed` so readiness cannot be claimed on an unmeasurable state.
pub fn real_probe_fn(probe_script_path: PathBuf, bundled_node: PathBuf) -> ProbeFn {
    Arc::new(
        move |context: &ProbeContext, _prior: &ReadinessSnapshot| -> ReadinessSnapshot {
            let mut snapshot = ReadinessSnapshot::pending();
            let mut command = Command::new(&bundled_node);
            command
                .arg(&probe_script_path)
                .arg("--local-url")
                .arg(context.local_url.clone().unwrap_or_default())
                .arg("--local-mcp-url")
                .arg(context.local_mcp_url.clone().unwrap_or_default())
                .arg("--public-url")
                .arg(context.public_url.clone().unwrap_or_default())
                .arg("--public-mcp-url")
                .arg(context.public_mcp_url.clone().unwrap_or_default())
                .arg("--pids")
                .arg(
                    context
                        .known_mcp_pids
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                )
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                command.creation_flags(CREATE_NO_WINDOW);
            }
            let result = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    return failed_everywhere(
                        snapshot,
                        format!("readiness probe could not start: {error}"),
                    );
                }
            };
            let deadline = Instant::now() + PROBE_DEADLINE;
            let mut child = result;
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let stdout = child
                            .stdout
                            .take()
                            .map(|mut pipe| {
                                use std::io::Read as _;
                                let mut buffer = String::new();
                                let _ = pipe.read_to_string(&mut buffer);
                                buffer
                            })
                            .unwrap_or_default();
                        let stderr = child
                            .stderr
                            .take()
                            .map(|mut pipe| {
                                use std::io::Read as _;
                                let mut buffer = String::new();
                                let _ = pipe.read_to_string(&mut buffer);
                                buffer
                            })
                            .unwrap_or_default();
                        if !status.success() {
                            let detail = if stderr.trim().is_empty() {
                                format!(
                                    "readiness probe exited with {}",
                                    status.code().unwrap_or(-1)
                                )
                            } else {
                                format!(
                                    "readiness probe exited with {}: {}",
                                    status.code().unwrap_or(-1),
                                    stderr.trim()
                                )
                            };
                            return failed_everywhere(snapshot, detail);
                        }
                        return apply_report(snapshot, &stdout);
                    }
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            return failed_everywhere(
                                snapshot,
                                "readiness probe timed out".to_string(),
                            );
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                    Err(error) => {
                        return failed_everywhere(
                            snapshot,
                            format!("readiness probe failed to wait: {error}"),
                        );
                    }
                }
            }
        },
    )
}

fn failed_everywhere(mut snapshot: ReadinessSnapshot, detail: String) -> ReadinessSnapshot {
    for entry in [
        &mut snapshot.local_process,
        &mut snapshot.local_endpoint,
        &mut snapshot.local_protocol,
        &mut snapshot.public_tunnel,
        &mut snapshot.public_protocol,
    ] {
        entry.status = ProbeStatus::Failed;
        entry.detail = Some(detail.clone());
    }
    snapshot.recompute();
    snapshot
}

fn apply_report(mut snapshot: ReadinessSnapshot, stdout: &str) -> ReadinessSnapshot {
    let report: ProbeReport = match serde_json::from_str(stdout.trim()) {
        Ok(report) => report,
        Err(error) => {
            return failed_everywhere(
                snapshot,
                format!("readiness probe output was not valid JSON: {error}"),
            );
        }
    };
    let gates: [(&mut ProbeEntry, Option<GatewayReport>); 5] = [
        (&mut snapshot.local_process, report.localProcess),
        (&mut snapshot.local_endpoint, report.localEndpoint),
        (&mut snapshot.local_protocol, report.localProtocol),
        (&mut snapshot.public_tunnel, report.publicTunnel),
        (&mut snapshot.public_protocol, report.publicProtocol),
    ];
    for (entry, report) in gates {
        if let Some(report) = report {
            entry.status = map_probe_status(&report.status);
            entry.checked_at = Some(now_label());
            entry.detail = report.detail.or(entry.detail.clone());
        }
    }
    snapshot.recompute();
    snapshot
}

fn now_label() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::Attempt;
    use crate::settings::Settings;
    use std::sync::atomic::{AtomicBool, AtomicU32};

    fn test_context() -> ProbeContext {
        ProbeContext {
            known_mcp_pids: vec![42],
            local_url: Some("http://127.0.0.1:8787".to_string()),
            local_mcp_url: Some("http://127.0.0.1:8787/t/token1/mcp".to_string()),
            public_url: Some("https://gptmcp-test.shares.zrok.io".to_string()),
            public_mcp_url: Some("https://gptmcp-test.shares.zrok.io/t/token1/mcp".to_string()),
            probe_script_path: PathBuf::from("probe.mjs"),
            bundled_node: PathBuf::from("node"),
        }
    }

    #[test]
    fn pending_snapshot_is_never_ready() {
        let snapshot = ReadinessSnapshot::pending();
        assert!(!snapshot.is_ready());
        assert!(!snapshot.is_local_ready());
        assert!(!snapshot.is_public_ready());
        assert!(!snapshot.degraded);
        assert_eq!(snapshot.local_process.status, ProbeStatus::Pending);
    }

    #[test]
    fn local_gates_alone_are_not_publicly_ready() {
        let mut snapshot = ReadinessSnapshot::pending();
        snapshot.local_process.status = ProbeStatus::Verified;
        snapshot.local_endpoint.status = ProbeStatus::Verified;
        snapshot.local_protocol.status = ProbeStatus::Verified;
        snapshot.public_tunnel.status = ProbeStatus::Unsupported;
        snapshot.public_protocol.status = ProbeStatus::Unsupported;
        snapshot.recompute();
        assert!(snapshot.is_local_ready());
        assert!(!snapshot.is_public_ready());
        assert!(
            !snapshot.is_ready(),
            "public readiness requires public protocol"
        );
    }

    #[test]
    fn unsupported_gates_do_not_degrade() {
        let mut snapshot = ReadinessSnapshot::pending();
        snapshot.public_tunnel.status = ProbeStatus::Unsupported;
        snapshot.recompute();
        assert!(!snapshot.degraded);
    }

    #[test]
    fn invalidate_resets_to_pending() {
        let scheduler = ReadinessScheduler::new();
        scheduler.invalidate();
        let snapshot = scheduler.current();
        assert_eq!(snapshot.local_process.status, ProbeStatus::Pending);
        assert_eq!(scheduler.generation(), 0);
    }

    #[test]
    fn scheduler_captures_probe_snapshot_for_its_generation() {
        let scheduler = ReadinessScheduler::new();
        let context = test_context();
        let provider: ContextProvider = {
            let context = context.clone();
            Arc::new(move || context.clone())
        };
        let local_url = context.local_url.clone();
        let probe_fn: ProbeFn = Arc::new(move |_: &ProbeContext, prior: &ReadinessSnapshot| {
            let mut snapshot = prior.clone();
            if let Some(url) = &local_url {
                snapshot.local_endpoint = ProbeEntry {
                    kind: "localEndpoint".to_string(),
                    status: ProbeStatus::Verified,
                    checked_at: Some(url.clone()),
                    detail: Some("connect ok".to_string()),
                };
            }
            snapshot
        });
        scheduler.install_probe(provider, probe_fn);
        let attempt = Attempt::new(7, Settings::default(), 11);
        scheduler.schedule_for(&attempt);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if scheduler.current().local_endpoint.status == ProbeStatus::Verified {
                break;
            }
            assert!(Instant::now() < deadline, "probe never captured");
            thread::sleep(Duration::from_millis(20));
        }
        let snapshot = scheduler.current();
        assert_eq!(
            snapshot.generation, 7,
            "snapshot must be tagged with its generation"
        );
        assert_eq!(
            snapshot.verified_revision,
            Some(11),
            "verified_revision must hold the settings revision, not the generation"
        );
        scheduler.invalidate();
    }

    #[test]
    fn invalidate_does_not_wait_for_a_slow_probe() {
        let scheduler = ReadinessScheduler::new();
        let context = test_context();
        let provider: ContextProvider = {
            let context = context.clone();
            Arc::new(move || context.clone())
        };
        let slow: ProbeFn = Arc::new(|_: &ProbeContext, prior: &ReadinessSnapshot| {
            thread::sleep(Duration::from_millis(600));
            let mut snapshot = prior.clone();
            snapshot.local_endpoint.status = ProbeStatus::Verified;
            snapshot
        });
        scheduler.install_probe(provider, slow);
        let attempt = Attempt::new(3, Settings::default(), 4);
        scheduler.schedule_for(&attempt);
        thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        scheduler.invalidate();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(400),
            "invalidate blocked on the probe loop: {elapsed:?}"
        );
    }

    #[test]
    fn probe_result_after_stop_cannot_republish_verified() {
        let scheduler = ReadinessScheduler::new();
        let context = test_context();
        let provider: ContextProvider = {
            let context = context.clone();
            Arc::new(move || context.clone())
        };
        let slow = Arc::new(AtomicBool::new(false));
        let publishes = Arc::new(AtomicU32::new(0));
        let slow_probe: ProbeFn = {
            let slow = slow.clone();
            let publishes = publishes.clone();
            Arc::new(move |_: &ProbeContext, prior: &ReadinessSnapshot| {
                thread::sleep(Duration::from_millis(120));
                let mut snapshot = prior.clone();
                snapshot.local_endpoint.status = ProbeStatus::Verified;
                if slow.load(Ordering::SeqCst) {
                    publishes.fetch_add(1, Ordering::SeqCst);
                }
                snapshot
            })
        };
        scheduler.install_probe(provider, slow_probe);
        let attempt = Attempt::new(5, Settings::default(), 6);
        scheduler.schedule_for(&attempt);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if scheduler.current().local_endpoint.status == ProbeStatus::Verified {
                break;
            }
            assert!(Instant::now() < deadline, "initial probe never captured");
            thread::sleep(Duration::from_millis(20));
        }
        // Stop invalidates: a late probe result must not republish verified.
        slow.store(true, Ordering::SeqCst);
        scheduler.invalidate();
        thread::sleep(Duration::from_secs(1));
        let snapshot = scheduler.current();
        assert!(
            snapshot.local_endpoint.status != ProbeStatus::Verified,
            "stale probe republished verified after stop"
        );
        assert_eq!(
            snapshot.generation, 0,
            "published snapshot must belong to the invalidated/zero generation"
        );
        assert_eq!(
            scheduler.generation(),
            0,
            "generation must reset after stop"
        );
    }

    #[test]
    fn scheduler_restarts_after_invalidate() {
        let scheduler = ReadinessScheduler::new();
        let context = test_context();
        let provider: ContextProvider = {
            let context = context.clone();
            Arc::new(move || context.clone())
        };
        let probe_fn: ProbeFn = Arc::new(|_: &ProbeContext, prior: &ReadinessSnapshot| {
            let mut snapshot = prior.clone();
            snapshot.local_protocol.status = ProbeStatus::Verified;
            snapshot
        });
        scheduler.install_probe(provider, probe_fn);
        let attempt = Attempt::new(1, Settings::default(), 1);
        scheduler.schedule_for(&attempt);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if scheduler.current().local_protocol.status == ProbeStatus::Verified {
                break;
            }
            assert!(Instant::now() < deadline, "first loop never captured");
            thread::sleep(Duration::from_millis(20));
        }
        scheduler.invalidate();
        assert_eq!(scheduler.current().generation, 0);
        let attempt = Attempt::new(2, Settings::default(), 2);
        scheduler.schedule_for(&attempt);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = scheduler.current();
            if snapshot.generation == 2
                && snapshot.verified_revision == Some(2)
                && snapshot.local_protocol.status == ProbeStatus::Verified
            {
                break;
            }
            assert!(Instant::now() < deadline, "restarted loop never captured");
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(scheduler.generation(), 2);
        scheduler.invalidate();
    }

    #[test]
    fn report_mapping_tags_checked_at() {
        let snapshot = ReadinessSnapshot::pending();
        let applied = apply_report(
            snapshot,
            r#"{"localProcess":{"status":"verified","detail":"MCP process alive (42)"},"localEndpoint":{"status":"failed","detail":"local endpoint: ECONNREFUSED"}}"#,
        );
        assert_eq!(applied.local_process.status, ProbeStatus::Verified);
        assert_eq!(applied.local_endpoint.status, ProbeStatus::Failed);
        assert_eq!(
            applied.local_endpoint.detail.as_deref(),
            Some("local endpoint: ECONNREFUSED")
        );
        assert!(applied.local_process.checked_at.is_some());
        assert!(applied.local_protocol.status == ProbeStatus::Pending);
        assert!(applied.degraded);
        assert_eq!(applied.issues.len(), 1);
        assert!(applied.gates()[0].kind == "localProcess");
    }
}
