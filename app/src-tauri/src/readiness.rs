use crate::lifecycle::{Attempt, ReadinessController};
use crate::settings::Settings;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A single readiness gate. `kind` names the gate for status/report output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProbeStatus {
    Pending,
    Verifying,
    Verified,
    Failed,
    Unsupported,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeEntry {
    pub kind: &'static str,
    pub status: ProbeStatus,
    pub checked_at: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
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
    /// Readiness sample this snapshot was captured for.
    pub verified_revision: Option<u64>,
    /// True when any gate is currently Failed.
    pub degraded: bool,
    pub issues: Vec<String>,
}

impl ReadinessSnapshot {
    pub fn pending() -> Self {
        let gate = |kind: &'static str| ProbeEntry {
            kind,
            status: ProbeStatus::Pending,
            checked_at: None,
            detail: None,
        };
        Self {
            local_process: gate("localProcess"),
            local_endpoint: gate("localEndpoint"),
            local_protocol: gate("localProtocol"),
            public_tunnel: gate("publicTunnel"),
            public_protocol: gate("publicProtocol"),
            verified_revision: None,
            degraded: false,
            issues: Vec::new(),
        }
    }

    pub fn is_ready(&self) -> bool {
        !self.degraded
            && self.local_process.status != ProbeStatus::Pending
            && self.local_process.status != ProbeStatus::Failed
            && self.local_endpoint.status == ProbeStatus::Verified
            && self.local_protocol.status == ProbeStatus::Verified
    }
}

fn now_label() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Everything a probe needs to observe the running services. Provided by the
/// process module, kept generic here so readiness is unit-testable.
#[derive(Clone)]
pub struct ProbeContext {
    pub known_mcp_pids: Vec<u32>,
    pub local_url: Option<String>,
    pub public_url: Option<String>,
    pub probe_script_path: PathBuf,
    pub bundled_node: PathBuf,
    pub bundled_resource_dir: PathBuf,
    pub settings_path: PathBuf,
}

/// Returns the next full snapshot for the given context, annotating each gate.
pub type ProbeFn = Arc<dyn Fn(&ProbeContext, &ReadinessSnapshot) -> ReadinessSnapshot + Send + Sync>;

struct SchedulerState {
    snapshot: ReadinessSnapshot,
    context: Option<ProbeContext>,
    settings: Option<Settings>,
    probe: Option<ProbeFn>,
}

/// Probe scheduler attached to the coordinator. `schedule_for` records the
/// attempt generation and starts a background probe loop owned by that
/// generation; `invalidate` (called on stop/reconfigure) halts it so a stale
/// generation can never resurrect readiness.
pub struct ReadinessScheduler {
    running: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    state: Arc<Mutex<SchedulerState>>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl ReadinessScheduler {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
            state: Arc::new(Mutex::new(SchedulerState {
                snapshot: ReadinessSnapshot::pending(),
                context: None,
                settings: None,
                probe: None,
            })),
            thread: Mutex::new(None),
        }
    }

    pub fn install_probe(&self, context: ProbeContext, settings: Settings, probe: ProbeFn) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.context = Some(context);
        state.settings = Some(settings);
        state.probe = Some(probe);
    }
}

impl ReadinessController for ReadinessScheduler {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn schedule_for(&self, attempt: &Attempt) {
        let generation = attempt.generation;
        self.generation
            .store(generation, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);
        let mut thread_slot = match self.thread.lock() {
            Ok(thread) => thread,
            Err(poisoned) => poisoned.into_inner(),
        };
        if thread_slot.is_some() {
            return;
        }
        let running = self.running.clone();
        let generation_handle = self.generation.clone();
        let state = self.state.clone();
        *thread_slot = Some(thread::spawn(move || {
            // First sample immediately; subsequent samples are spaced.
            let mut first = true;
            while running.load(Ordering::SeqCst) {
                let tick_generation = generation_handle.load(Ordering::SeqCst);
                if tick_generation == 0 {
                    break;
                }
                let (context, settings, probe, prior) = {
                    let guard = match state.lock() {
                        Ok(state) => state,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    (
                        guard.context.clone(),
                        guard.settings.clone(),
                        guard.probe.clone(),
                        guard.snapshot.clone(),
                    )
                };
                let Some(context) = context else {
                    break;
                };
                let Some(probe) = probe else {
                    break;
                };
                let _ = settings;
                let mut snapshot = probe(&context, &prior);
                snapshot.verified_revision = Some(generation);
                snapshot.degraded = [
                    &snapshot.local_process,
                    &snapshot.local_endpoint,
                    &snapshot.local_protocol,
                    &snapshot.public_tunnel,
                    &snapshot.public_protocol,
                ]
                .iter()
                .any(|entry| entry.status == ProbeStatus::Failed);
                snapshot.issues = [
                    &snapshot.local_process,
                    &snapshot.local_endpoint,
                    &snapshot.local_protocol,
                    &snapshot.public_tunnel,
                    &snapshot.public_protocol,
                ]
                .iter()
                .filter_map(|entry| {
                    if entry.status == ProbeStatus::Failed {
                        Some(format!("{}: {}", entry.kind, entry.detail.as_deref().unwrap_or("failed")))
                    } else {
                        None
                    }
                })
                .collect();
                {
                    let mut guard = match state.lock() {
                        Ok(state) => state,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    guard.snapshot = snapshot;
                }
                // Stop when superseded by a newer generation or invalidated.
                if generation_handle.load(Ordering::SeqCst) != tick_generation
                    || !running.load(Ordering::SeqCst)
                {
                    break;
                }
                if first {
                    first = false;
                }
                thread::sleep(Duration::from_millis(2_000));
            }
        }));
    }

    fn invalidate(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.generation.store(0, Ordering::SeqCst);
        match self.state.lock() {
            Ok(mut state) => state.snapshot = ReadinessSnapshot::pending(),
            Err(poisoned) => poisoned.into_inner().snapshot = ReadinessSnapshot::pending(),
        }
        if let Ok(mut thread_slot) = self.thread.lock() {
            if let Some(handle) = thread_slot.take() {
                let _ = handle.join();
            }
        }
    }

    fn current(&self) -> ReadinessSnapshot {
        match self.state.lock() {
            Ok(state) => state.snapshot.clone(),
            Err(poisoned) => poisoned.into_inner().snapshot.clone(),
        }
    }
}

impl Default for ReadinessScheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn pending_snapshot_is_never_ready() {
        let snapshot = ReadinessSnapshot::pending();
        assert!(!snapshot.is_ready());
        assert!(!snapshot.degraded);
        assert_eq!(snapshot.local_process.status, ProbeStatus::Pending);
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
        let context = ProbeContext {
            known_mcp_pids: vec![42],
            local_url: Some("http://127.0.0.1:8787".to_string()),
            public_url: Some("https://name.shares.zrok.io".to_string()),
            probe_script_path: PathBuf::from("probe.mjs"),
            bundled_node: PathBuf::from("node"),
            bundled_resource_dir: PathBuf::from("resources"),
            settings_path: PathBuf::from("settings.json"),
        };
        let probe_fn = {
            let local_url = context.local_url.clone();
            Arc::new(move |_: &ProbeContext, prior: &ReadinessSnapshot| {
                let mut snapshot = prior.clone();
                if let Some(url) = &local_url {
                    snapshot.local_endpoint = ProbeEntry {
                        kind: "localEndpoint",
                        status: ProbeStatus::Verified,
                        checked_at: Some(url.clone()),
                        detail: Some("connect ok".to_string()),
                    };
                }
                snapshot
            })
        };
        scheduler.install_probe(context, Settings::default(), probe_fn);
        let attempt = crate::lifecycle::Attempt::new(7, Settings::default(), 1);
        scheduler.schedule_for(&attempt);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if scheduler.current().local_endpoint.status == ProbeStatus::Verified {
                break;
            }
            assert!(Instant::now() < deadline, "probe never captured");
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            scheduler.current().verified_revision,
            Some(7),
            "snapshot must be tagged with its generation"
        );
        scheduler.invalidate();
    }
}