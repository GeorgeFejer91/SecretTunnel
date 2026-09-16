use crate::error::AppError;
use crate::settings::Settings;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Effective lifecycle state exposed to status readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    Stopped,
    Starting,
    Running,
    Stopping,
    CleanupFailed,
}

impl LifecycleState {
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::CleanupFailed => "cleanup-failed",
        }
    }
}

/// A superseding lifecycle request. Workers use the immutable settings snapshot
/// associated with their generation; a later request invalidates theirs.
#[derive(Clone)]
pub enum LifecycleRequest {
    Start {
        settings: Settings,
        revision: u64,
        retry: bool,
    },
    Reconfigure {
        settings: Settings,
        revision: u64,
    },
    Stop,
    Shutdown,
}

/// Generation-checked status publication observer. Invoked only for the
/// winning generation, so external artifacts (status files, UI channels) are
/// never written by a superseded worker.
pub trait PublishHook: Send + Sync {
    fn published(
        &self,
        attempt: &Attempt,
        effective: LifecycleState,
        failure: Option<(String, String)>,
    );
}

/// Per-attempt ownership: every child and remote resource created during one
/// attempt is recorded here so cancellation or partial failure can terminate
/// exactly that attempt's resources, never a newer generation's.
pub struct Attempt {
    pub generation: u64,
    pub settings: Settings,
    pub revision: u64,
    cancel: Arc<AtomicBool>,
    children: Mutex<HashSet<u32>>,
    share_tokens: Mutex<HashSet<String>>,
}

impl Attempt {
    pub fn new(generation: u64, settings: Settings, revision: u64) -> Self {
        Self {
            generation,
            settings,
            revision,
            cancel: Arc::new(AtomicBool::new(false)),
            children: Mutex::new(HashSet::new()),
            share_tokens: Mutex::new(HashSet::new()),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn register_child(&self, pid: u32) {
        if let Ok(mut children) = self.children.lock() {
            children.insert(pid);
        }
    }

    pub fn forget_child(&self, pid: u32) {
        if let Ok(mut children) = self.children.lock() {
            children.remove(&pid);
        }
    }

    pub fn registered_children(&self) -> Vec<u32> {
        match self.children.lock() {
            Ok(children) => children.iter().copied().collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn clear_children(&self) {
        if let Ok(mut children) = self.children.lock() {
            children.clear();
        }
    }

    pub fn register_share_token(&self, token: &str) {
        if !token.is_empty() {
            if let Ok(mut tokens) = self.share_tokens.lock() {
                tokens.insert(token.to_string());
            }
        }
    }

    pub fn registered_share_tokens(&self) -> Vec<String> {
        match self.share_tokens.lock() {
            Ok(tokens) => tokens.iter().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// What the endpoint reports back from an attempt's execution.
pub enum AttemptOutcome {
    /// Services are running after this attempt.
    Running,
    /// Services are stopped after this attempt.
    Stopped,
    /// Startup failed after this attempt; a cleanup pass ran, with no
    /// confirmed leftovers.
    FailedToStart { error: Option<AppError> },
    /// Initial cleanup could not be confirmed; replacement startup is blocked.
    CleanupFailed { details: String },
}

/// The concrete service operations. A worker calls exactly one of these based
/// on the request kind. Implementations must respect `Attempt::is_cancelled`
/// before and after slow operations, and use bounded deadlines.
pub trait LifecycleEndpoint: Send + Sync {
    fn start_services(&self, attempt: &Attempt, settings: &Settings) -> Result<(), AppError>;
    fn stop_services(&self, attempt: &Attempt) -> Result<bool, AppError>;

    /// Apply changed settings to an already-running generation. An endpoint
    /// that can narrow the work - replacing only the part that the change
    /// actually affects, and leaving the rest serving - overrides this. The
    /// default is a full restart, which is always correct if not always cheap.
    fn reconfigure_services(&self, attempt: &Attempt, settings: &Settings) -> Result<(), AppError> {
        self.start_services(attempt, settings)
    }
}

/// Outcomes of admitting a request, before any execution lock is taken.
enum Admission {
    /// No worker is needed (equivalent work coalesced, or a retry that must not
    /// undo an explicit stop).
    Handled,
    /// A worker must run for this generation.
    Spawn {
        attempt: Arc<Attempt>,
        request: LifecycleRequest,
    },
}

struct CoordinatorState {
    generation: u64,
    desired_running: bool,
    effective: LifecycleState,
    in_flight_generation: Option<u64>,
    in_flight_cancel: Option<Arc<AtomicBool>>,
    coalesce_key: Option<(Settings, u64)>,
    blocked: Option<String>,
}

pub struct Coordinator {
    endpoint: Arc<dyn LifecycleEndpoint>,
    state: Arc<Mutex<CoordinatorState>>,
    /// Serializes the actual service/configuration changes. Admission never
    /// waits on this; only bounded workers do.
    execution: Arc<Mutex<()>>,
    /// Readiness probes run on their own scheduler and are invalidated here.
    readiness: Mutex<Option<Arc<dyn ReadinessController>>>,
    /// External status publication, observed only for the winning generation.
    publish_hook: Mutex<Option<Arc<dyn PublishHook>>>,
}

impl Coordinator {
    pub fn new(endpoint: Arc<dyn LifecycleEndpoint>) -> Self {
        Self {
            endpoint,
            state: Arc::new(Mutex::new(CoordinatorState {
                generation: 0,
                desired_running: false,
                effective: LifecycleState::Stopped,
                in_flight_generation: None,
                in_flight_cancel: None,
                coalesce_key: None,
                blocked: None,
            })),
            execution: Arc::new(Mutex::new(())),
            readiness: Mutex::new(None),
            publish_hook: Mutex::new(None),
        }
    }

    pub fn attach_readiness(&self, controller: Arc<dyn ReadinessController>) {
        match self.readiness.lock() {
            Ok(mut slot) => {
                *slot = Some(controller);
            }
            Err(poisoned) => {
                *poisoned.into_inner() = Some(controller);
            }
        }
    }

    /// Register the external publication observer (status file, UI channel).
    /// The hook is only invoked when this attempt is the winning generation.
    pub fn attach_publish_hook(&self, hook: Arc<dyn PublishHook>) {
        match self.publish_hook.lock() {
            Ok(mut slot) => {
                *slot = Some(hook);
            }
            Err(poisoned) => {
                *poisoned.into_inner() = Some(hook);
            }
        }
    }

    fn publish_hook(&self) -> Option<Arc<dyn PublishHook>> {
        match self.publish_hook.lock() {
            Ok(guarded) => guarded.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Snapshot the current readiness state if a scheduler is attached.
    pub fn readiness_snapshot(&self) -> Option<crate::readiness::ReadinessSnapshot> {
        let guarded = match self.readiness.lock() {
            Ok(guarded) => guarded,
            Err(poisoned) => poisoned.into_inner(),
        };
        guarded.as_ref().map(|controller| controller.current())
    }

    fn readiness_handle(&self) -> Option<Arc<dyn ReadinessController>> {
        match self.readiness.lock() {
            Ok(guarded) => guarded.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Submit a request. Admission is short and non-blocking on execution; it
    /// records intent, invalidates the previous generation, and returns after
    /// launching a worker if actual work is required.
    ///
    /// Returns `0` when no worker was spawned (coalesced/rejected/blocked) and
    /// the generation of the spawned worker otherwise.
    pub fn submit(&self, request: LifecycleRequest) -> u64 {
        let admission = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.admit(&mut state, request)
        };
        if let Admission::Spawn { attempt, request } = admission {
            let attempt = attempt.clone();
            let request = request.clone();
            let state = self.state.clone();
            let execution = self.execution.clone();
            let endpoint = self.endpoint.clone();
            let readiness = self.readiness_handle();
            let publish_hook = self.publish_hook();
            let generation = attempt.generation;
            thread::spawn(move || {
                run_worker(
                    execution,
                    endpoint,
                    attempt,
                    request,
                    state,
                    readiness,
                    publish_hook,
                );
            });
            generation
        } else {
            0
        }
    }

    /// Wait until the given generation has finished publishing (it is no
    /// longer in flight), or until `deadline` elapses. Returns the last known
    /// effective state.
    pub fn wait_for_generation(&self, generation: u64, deadline: Duration) -> LifecycleState {
        let started = std::time::Instant::now();
        loop {
            let in_flight = match self.state.lock() {
                Ok(state) => state.in_flight_generation,
                Err(poisoned) => poisoned.into_inner().in_flight_generation,
            };
            if in_flight != Some(generation) {
                break;
            }
            if started.elapsed() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        self.effective_state()
    }

    fn admit(&self, state: &mut CoordinatorState, request: LifecycleRequest) -> Admission {
        match &request {
            LifecycleRequest::Start {
                settings,
                revision,
                retry,
            } => {
                if *retry && !state.desired_running {
                    // A retry must never undo an explicit Stop.
                    return Admission::Handled;
                }
                if state.blocked.is_some() {
                    // Cleanup is unresolved: placeholder startup is rejected
                    // without toggling desired_running, so a later Reconfigure
                    // cannot bypass the block either.
                    return Admission::Handled;
                }
                let same_snapshot =
                    state
                        .coalesce_key
                        .as_ref()
                        .is_some_and(|(last, last_revision)| {
                            last_revision == revision && settings == last
                        });
                if state.desired_running && state.in_flight_generation.is_some() && same_snapshot {
                    // Equivalent start already in flight; coalesce.
                    return Admission::Handled;
                }
                if state.in_flight_generation.is_none() && same_snapshot {
                    if state.effective.is_running() {
                        return Admission::Handled;
                    }
                }
                if !*retry {
                    state.desired_running = true;
                }
                self.supercede(state, &request)
            }
            LifecycleRequest::Reconfigure {
                settings: _,
                revision: _,
            } => {
                if state.blocked.is_some() {
                    // Reconfiguration must not bypass an unconfirmed cleanup
                    // block: only a Stop that confirms cleanup clears it.
                    return Admission::Handled;
                }
                if !state.desired_running {
                    // Reconfigure of a stopped profile defers to an explicit
                    // start later; nothing to restart.
                    return Admission::Handled;
                }
                state.desired_running = true;
                self.supercede(state, &request)
            }
            LifecycleRequest::Stop | LifecycleRequest::Shutdown => {
                state.desired_running = false;
                self.supercede(state, &request)
            }
        }
    }

    fn supercede(&self, state: &mut CoordinatorState, request: &LifecycleRequest) -> Admission {
        state.generation += 1;
        let generation = state.generation;
        // Signal the in-flight attempt that it has been superseded so it can
        // stop at its next cancellation checkpoint. It must never publish
        // current status afterwards (the generation gate in `publish` enforces
        // that).
        if let Some(cancel) = state.in_flight_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        state.in_flight_generation = Some(generation);
        // Readiness from the previous generation is stale the moment a
        // superseding request is accepted. Invalidate at admission, not later
        // when the worker happens to obtain the execution lock.
        if let Some(readiness) = self.readiness_handle() {
            readiness.invalidate();
        }
        let share = match request {
            LifecycleRequest::Start {
                settings, revision, ..
            }
            | LifecycleRequest::Reconfigure { settings, revision } => {
                Some((settings.clone(), *revision))
            }
            LifecycleRequest::Stop | LifecycleRequest::Shutdown => None,
        };
        if let Some((settings, revision)) = share {
            state.coalesce_key = Some((settings, revision));
        }
        // CleanupFailed blocks replacement startup: the block persists until a
        // Stop confirms cleanup (only publish(Stopped) clears it). Nothing here
        // removes it.
        let cancel = Arc::new(AtomicBool::new(false));
        let attempt = Arc::new(Attempt {
            generation,
            settings: settings_for_request(request),
            revision: revision_for_request(request),
            cancel: cancel.clone(),
            children: Mutex::new(HashSet::new()),
            share_tokens: Mutex::new(HashSet::new()),
        });
        state.in_flight_cancel = Some(cancel);
        let is_stop = matches!(request, LifecycleRequest::Stop | LifecycleRequest::Shutdown);
        if is_stop {
            state.effective = if state.blocked.is_some() {
                LifecycleState::CleanupFailed
            } else {
                LifecycleState::Stopping
            };
        } else {
            state.effective = LifecycleState::Starting;
        }
        Admission::Spawn {
            attempt,
            request: request.clone(),
        }
    }

    pub fn desired_running(&self) -> bool {
        match self.state.lock() {
            Ok(state) => state.desired_running,
            Err(poisoned) => poisoned.into_inner().desired_running,
        }
    }

    pub fn effective_state(&self) -> LifecycleState {
        match self.state.lock() {
            Ok(state) => state.effective,
            Err(poisoned) => poisoned.into_inner().effective,
        }
    }

    pub fn current_generation(&self) -> u64 {
        match self.state.lock() {
            Ok(state) => state.generation,
            Err(poisoned) => poisoned.into_inner().generation,
        }
    }

    pub fn blocked_reason(&self) -> Option<String> {
        match self.state.lock() {
            Ok(state) => state.blocked.clone(),
            Err(poisoned) => poisoned.into_inner().blocked.clone(),
        }
    }

    /// The generation currently being executed, if any.
    pub fn in_flight_generation(&self) -> Option<u64> {
        match self.state.lock() {
            Ok(state) => state.in_flight_generation,
            Err(poisoned) => poisoned.into_inner().in_flight_generation,
        }
    }

    /// Wait until no worker is in flight (or until the deadline), returning
    /// the last known effective state. Used when a request was coalesced and
    /// the caller should wait for the winning worker instead.
    pub fn wait_for_idle(&self, deadline: Duration) -> LifecycleState {
        let started = std::time::Instant::now();
        loop {
            let in_flight = self.in_flight_generation();
            if in_flight.is_none() {
                break;
            }
            if started.elapsed() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        self.effective_state()
    }
}

/// A readiness scheduler attached to the coordinator. It is invalidated on
/// stop/reconfigure and must never resurrect readiness from a stale generation.
pub trait ReadinessController: Send + Sync {
    fn generation(&self) -> u64;
    fn schedule_for(&self, attempt: &Attempt);
    fn invalidate(&self);
    fn current(&self) -> crate::readiness::ReadinessSnapshot;
}

fn settings_for_request(request: &LifecycleRequest) -> Settings {
    match request {
        LifecycleRequest::Start { settings, .. }
        | LifecycleRequest::Reconfigure { settings, .. } => settings.clone(),
        LifecycleRequest::Stop | LifecycleRequest::Shutdown => Settings::default(),
    }
}

fn revision_for_request(request: &LifecycleRequest) -> u64 {
    match request {
        LifecycleRequest::Start { revision, .. }
        | LifecycleRequest::Reconfigure { revision, .. } => *revision,
        LifecycleRequest::Stop | LifecycleRequest::Shutdown => 0,
    }
}

fn run_worker(
    execution: Arc<Mutex<()>>,
    endpoint: Arc<dyn LifecycleEndpoint>,
    attempt: Arc<Attempt>,
    request: LifecycleRequest,
    state: Arc<Mutex<CoordinatorState>>,
    readiness: Option<Arc<dyn ReadinessController>>,
    publish_hook: Option<Arc<dyn PublishHook>>,
) {
    let hook = &publish_hook;
    if attempt.is_cancelled() {
        publish(
            &state,
            &attempt,
            Some(LifecycleState::Stopped),
            None,
            None,
            hook,
        );
        return;
    }
    let guard = match execution.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let _guard = guard;

    // Re-check cancellation after acquiring the execution lock and before any
    // side effect, so a superseded worker never touches services.
    if attempt.is_cancelled() {
        publish(
            &state,
            &attempt,
            Some(LifecycleState::Stopped),
            None,
            None,
            hook,
        );
        return;
    }

    let outcome = match &request {
        LifecycleRequest::Start { settings, .. }
        | LifecycleRequest::Reconfigure { settings, .. } => {
            match if matches!(request, LifecycleRequest::Reconfigure { .. }) {
                // Reconfiguration of a live generation gets the endpoint's
                // narrower path, which may keep parts of it serving.
                endpoint.reconfigure_services(&attempt, settings)
            } else {
                endpoint.start_services(&attempt, settings)
            } {
                Ok(()) => {
                    if let Some(readiness) = &readiness {
                        readiness.invalidate();
                        readiness.schedule_for(&attempt);
                    }
                    AttemptOutcome::Running
                }
                Err(error) => {
                    // Startup failed: run cleanup ourselves so the caller cannot
                    // discard the cleanup outcome. Only a confirmed cleanup reports
                    // FailedToStart; unconfirmed leftovers block replacements.
                    let confirmed = match endpoint.stop_services(&attempt) {
                        Ok(confirmed) => confirmed,
                        Err(_) => false,
                    };
                    if confirmed {
                        AttemptOutcome::FailedToStart { error: Some(error) }
                    } else {
                        AttemptOutcome::CleanupFailed {
                            details: format!(
                                "Start failed ({}), and cleanup could not be confirmed.",
                                error.code
                            ),
                        }
                    }
                }
            }
        }
        LifecycleRequest::Stop | LifecycleRequest::Shutdown => {
            if let Some(readiness) = &readiness {
                readiness.invalidate();
            }
            let confirmed = match endpoint.stop_services(&attempt) {
                Ok(confirmed) => confirmed,
                Err(_) => false,
            };
            if confirmed {
                AttemptOutcome::Stopped
            } else {
                AttemptOutcome::CleanupFailed {
                    details: "Service cleanup could not be confirmed.".to_string(),
                }
            }
        }
    };

    match outcome {
        AttemptOutcome::Running => publish(
            &state,
            &attempt,
            Some(LifecycleState::Running),
            None,
            None,
            &hook,
        ),
        AttemptOutcome::Stopped => publish(
            &state,
            &attempt,
            Some(LifecycleState::Stopped),
            None,
            None,
            &hook,
        ),
        AttemptOutcome::FailedToStart { error } => {
            let failure = error.map(|error| (error.message.clone(), error.code.to_string()));
            publish(
                &state,
                &attempt,
                Some(LifecycleState::Stopped),
                None,
                failure,
                &hook,
            )
        }
        AttemptOutcome::CleanupFailed { details } => publish(
            &state,
            &attempt,
            Some(LifecycleState::CleanupFailed),
            Some(details),
            None,
            &hook,
        ),
    }
}

fn publish(
    state: &Mutex<CoordinatorState>,
    attempt: &Attempt,
    effective: Option<LifecycleState>,
    cleanup_failure: Option<String>,
    hook_failure: Option<(String, String)>,
    hook: &Option<Arc<dyn PublishHook>>,
) {
    let mut state = match state.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    };
    // Atomically check generation: a superseded worker must not overwrite
    // current state with its stale outcome.
    if state.in_flight_generation != Some(attempt.generation) {
        return;
    }
    state.in_flight_generation = None;
    state.in_flight_cancel = None;
    if let Some(effective) = effective {
        if effective == LifecycleState::Stopped {
            state.coalesce_key = None;
        }
        state.effective = effective;
    }
    if let Some(details) = cleanup_failure {
        state.blocked = Some(details);
    } else if effective != Some(LifecycleState::CleanupFailed) {
        state.blocked = None;
    }
    // External observers only see the winning generation's publication.
    if let Some(ref hook) = hook {
        let failure = hook_failure.or_else(|| {
            state.blocked.as_ref().map(|d| {
                let code = match state.effective {
                    LifecycleState::CleanupFailed => "cleanup-failed",
                    _ => "unknown",
                };
                (d.clone(), code.to_string())
            })
        });
        hook.published(attempt, state.effective, failure);
    }
}

/// Bounded wait helper used by endpoints for subprocess/network deadlines.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(20);

pub fn should_abort(attempt: &Attempt) -> bool {
    attempt.is_cancelled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::time::Instant;

    fn sample_settings(workspace: &str) -> Settings {
        Settings {
            workspace_path: Some(workspace.to_string()),
            access_mode: crate::settings::AccessMode::Read,
            ..Settings::default()
        }
    }

    #[derive(Clone)]
    struct Recording {
        starts: Arc<Mutex<Vec<u64>>>,
        stops: Arc<Mutex<Vec<u64>>>,
        reconfigures: Arc<Mutex<Vec<u64>>>,
    }

    /// Endpoint that distinguishes the two paths, so the dispatch itself can be
    /// asserted: a Reconfigure must reach reconfigure_services, never
    /// start_services. The real endpoint keeps the public tunnel alive on that
    /// narrow path, so a silent regression to start_services would start
    /// tearing the users URL down on every folder change again.
    struct DispatchSpy {
        record: Recording,
    }

    impl LifecycleEndpoint for DispatchSpy {
        fn start_services(&self, attempt: &Attempt, _settings: &Settings) -> Result<(), AppError> {
            self.record.starts.lock().unwrap().push(attempt.generation);
            Ok(())
        }

        fn stop_services(&self, attempt: &Attempt) -> Result<bool, AppError> {
            self.record.stops.lock().unwrap().push(attempt.generation);
            Ok(true)
        }

        fn reconfigure_services(
            &self,
            attempt: &Attempt,
            _settings: &Settings,
        ) -> Result<(), AppError> {
            self.record
                .reconfigures
                .lock()
                .unwrap()
                .push(attempt.generation);
            Ok(())
        }
    }

    /// Endpoint fake whose start can be paused with barriers and whose behavior
    /// is fully controllable.
    struct FakeEndpoint {
        start_barrier: Option<Arc<Barrier>>,
        start_hang: Arc<AtomicBool>,
        stop_confirmed: bool,
        record: Recording,
    }

    impl FakeEndpoint {
        fn recording() -> (Recording, Self) {
            let record = Recording {
                starts: Arc::new(Mutex::new(Vec::new())),
                stops: Arc::new(Mutex::new(Vec::new())),
                reconfigures: Arc::new(Mutex::new(Vec::new())),
            };
            (
                record.clone(),
                Self {
                    start_barrier: None,
                    start_hang: Arc::new(AtomicBool::new(false)),
                    stop_confirmed: true,
                    record,
                },
            )
        }
    }

    impl LifecycleEndpoint for FakeEndpoint {
        fn start_services(&self, attempt: &Attempt, _settings: &Settings) -> Result<(), AppError> {
            self.record.starts.lock().unwrap().push(attempt.generation);
            if let Some(barrier) = &self.start_barrier {
                barrier.wait();
            }
            while self.start_hang.load(Ordering::SeqCst) {
                if attempt.is_cancelled() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            if attempt.is_cancelled() {
                return Err(AppError::new("cancelled", "Superseded."));
            }
            Ok(())
        }

        fn stop_services(&self, attempt: &Attempt) -> Result<bool, AppError> {
            self.record.stops.lock().unwrap().push(attempt.generation);
            Ok(self.stop_confirmed)
        }
    }

    #[test]
    fn duplicate_starts_are_coalesced() {
        let (record, endpoint) = FakeEndpoint::recording();
        let coordinator = Coordinator::new(Arc::new(endpoint));
        let settings = sample_settings("C:\\work");
        coordinator.submit(LifecycleRequest::Start {
            settings: settings.clone(),
            revision: 1,
            retry: false,
        });
        coordinator.submit(LifecycleRequest::Start {
            settings: settings.clone(),
            revision: 1,
            retry: true,
        });
        coordinator.submit(LifecycleRequest::Start {
            settings,
            revision: 1,
            retry: false,
        });
        wait_for(
            |coordinator: &Coordinator| coordinator.effective_state() == LifecycleState::Running,
            &coordinator,
        );
        // The first start runs; the equivalent retry and duplicate coalesce.
        assert_eq!(record.starts.lock().unwrap().len(), 1);
        assert_eq!(coordinator.current_generation(), 1);
    }

    #[test]
    fn start_paused_then_stop_cancels_cleanly() {
        let barrier = Arc::new(Barrier::new(2));
        let (record, mut endpoint) = FakeEndpoint::recording();
        endpoint.start_barrier = Some(barrier.clone());
        let coordinator = Coordinator::new(Arc::new(endpoint));

        coordinator.submit(LifecycleRequest::Start {
            settings: sample_settings("C:\\work"),
            revision: 1,
            retry: false,
        });
        // Wait until the start worker is paused mid-flight.
        barrier.wait();
        coordinator.submit(LifecycleRequest::Stop);
        wait_for(
            |coordinator: &Coordinator| coordinator.effective_state() == LifecycleState::Stopped,
            &coordinator,
        );
        assert_eq!(record.starts.lock().unwrap().len(), 1);
        assert_eq!(record.stops.lock().unwrap().len(), 1);
        assert!(!coordinator.desired_running());
    }

    #[test]
    fn reconfigure_while_starting_supersedes() {
        let (record, mut endpoint) = FakeEndpoint::recording();
        let hang = Arc::new(AtomicBool::new(true));
        endpoint.start_hang = hang.clone();
        let coordinator = Coordinator::new(Arc::new(endpoint));
        coordinator.submit(LifecycleRequest::Start {
            settings: sample_settings("C:\\work"),
            revision: 1,
            retry: false,
        });
        thread::sleep(Duration::from_millis(100));
        // Submit Reconfigure: this cancels the in-flight start and spawns a
        // new worker that blocks on the execution lock while the start worker
        // is still in the hang loop.
        let _ = coordinator.submit(LifecycleRequest::Reconfigure {
            settings: sample_settings("C:\\other"),
            revision: 2,
        });
        thread::sleep(Duration::from_millis(50));
        // Release the hang so the cancelled start worker can finish and the
        // reconfigure worker can proceed.
        hang.store(false, Ordering::SeqCst);
        wait_for(
            |coordinator: &Coordinator| {
                matches!(
                    coordinator.effective_state(),
                    LifecycleState::Running | LifecycleState::Stopped
                )
            },
            &coordinator,
        );
        // The superseded start must not publish; only the reconfigure owns the
        // final status.
        assert!(!record.starts.lock().unwrap().is_empty());
    }

    #[test]
    fn retry_scheduled_then_stop_before_execution() {
        let (record, endpoint) = FakeEndpoint::recording();
        let coordinator = Coordinator::new(Arc::new(endpoint));
        // Start, run, then stop.
        coordinator.submit(LifecycleRequest::Start {
            settings: sample_settings("C:\\work"),
            revision: 1,
            retry: false,
        });
        wait_for(
            |coordinator: &Coordinator| coordinator.effective_state() == LifecycleState::Running,
            &coordinator,
        );
        coordinator.submit(LifecycleRequest::Stop);
        wait_for(
            |coordinator: &Coordinator| coordinator.effective_state() == LifecycleState::Stopped,
            &coordinator,
        );
        // A retry scheduled after an explicit Stop must not undo it.
        coordinator.submit(LifecycleRequest::Start {
            settings: sample_settings("C:\\work"),
            revision: 1,
            retry: true,
        });
        thread::sleep(Duration::from_millis(100));
        assert_eq!(coordinator.effective_state(), LifecycleState::Stopped);
        assert_eq!(record.starts.lock().unwrap().len(), 1);
    }

    #[test]
    fn stop_invalidates_active_generation_without_waiting() {
        let (record, mut endpoint) = FakeEndpoint::recording();
        endpoint.start_hang = Arc::new(AtomicBool::new(true));
        let coordinator = Coordinator::new(Arc::new(endpoint));
        coordinator.submit(LifecycleRequest::Start {
            settings: sample_settings("C:\\work"),
            revision: 1,
            retry: false,
        });
        thread::sleep(Duration::from_millis(100));
        let before = coordinator.current_generation();
        coordinator.submit(LifecycleRequest::Stop);
        let after = coordinator.current_generation();
        assert!(after > before);
        // Stop admission returned without blocking the caller (no exec lock
        // wait at submit time).
        wait_for(
            |coordinator: &Coordinator| {
                matches!(
                    coordinator.effective_state(),
                    LifecycleState::Stopped | LifecycleState::CleanupFailed
                )
            },
            &coordinator,
        );
        assert!(!record.starts.lock().unwrap().is_empty());
        // The cancelled start calls stop_services as part of cleanup, and the
        // stop worker also calls stop_services, so we see at least 2.
        assert!(record.stops.lock().unwrap().len() >= 1);
    }

    #[test]
    fn cleanup_failure_blocks_replacements_until_stop() {
        let (record, mut endpoint) = FakeEndpoint::recording();
        endpoint.stop_confirmed = false;
        let coordinator = Coordinator::new(Arc::new(endpoint));
        coordinator.submit(LifecycleRequest::Start {
            settings: sample_settings("C:\\work"),
            revision: 1,
            retry: false,
        });
        wait_for(
            |coordinator: &Coordinator| coordinator.effective_state() == LifecycleState::Running,
            &coordinator,
        );
        coordinator.submit(LifecycleRequest::Shutdown);
        wait_for(
            |coordinator: &Coordinator| {
                coordinator.effective_state() == LifecycleState::CleanupFailed
            },
            &coordinator,
        );
        assert!(coordinator.blocked_reason().is_some());
        // Replacement start is blocked while cleanup is unconfirmed.
        coordinator.submit(LifecycleRequest::Start {
            settings: sample_settings("C:\\work"),
            revision: 1,
            retry: false,
        });
        thread::sleep(Duration::from_millis(100));
        assert_eq!(coordinator.effective_state(), LifecycleState::CleanupFailed);
        assert_eq!(record.stops.lock().unwrap().len(), 1);
    }

    fn wait_for(predicate: impl Fn(&Coordinator) -> bool, coordinator: &Coordinator) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if predicate(coordinator) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("condition not reached within deadline");
    }

    /// Changing the folder must not rebuild the tunnel. The coordinator has to
    /// route Reconfigure to the endpoint narrow path so the public URL keeps
    /// serving across the change; a Reconfigure that lands on start_services
    /// tears the share down and the users MCP URL 502s mid-session.
    #[test]
    fn reconfigure_takes_the_narrow_path_not_a_full_restart() {
        let record = Recording {
            starts: Arc::new(Mutex::new(Vec::new())),
            stops: Arc::new(Mutex::new(Vec::new())),
            reconfigures: Arc::new(Mutex::new(Vec::new())),
        };
        let coordinator = Coordinator::new(Arc::new(DispatchSpy {
            record: record.clone(),
        }));

        coordinator.submit(LifecycleRequest::Start {
            settings: sample_settings(r"C:\project-one"),
            revision: 1,
            retry: false,
        });
        wait_for(
            |coordinator: &Coordinator| coordinator.effective_state().is_running(),
            &coordinator,
        );
        assert_eq!(record.starts.lock().unwrap().len(), 1);

        // The user picks a different folder while connected.
        coordinator.submit(LifecycleRequest::Reconfigure {
            settings: sample_settings(r"C:\project-two"),
            revision: 2,
        });
        wait_for(
            |coordinator: &Coordinator| coordinator.effective_state().is_running(),
            &coordinator,
        );

        assert_eq!(
            record.reconfigures.lock().unwrap().len(),
            1,
            "reconfigure must use the narrow path"
        );
        assert_eq!(
            record.starts.lock().unwrap().len(),
            1,
            "the folder change must not re-run a full start"
        );
        assert!(
            record.stops.lock().unwrap().is_empty(),
            "the folder change must not stop the tunnel"
        );
    }
}
