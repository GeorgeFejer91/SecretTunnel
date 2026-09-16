use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use fs4::fs_std::FileExt;

const LOCK_FILE: &str = "profile.lock";
const META_FILE: &str = "profile.meta.json";
const ACTIVATION_DIR: &str = ".activation";
const REQUEST_ACK_WAIT: Duration = Duration::from_millis(2500);
const WATCH_INTERVAL: Duration = Duration::from_millis(200);
const MAX_REQUEST_AGE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerMetadata {
    pub pid: u32,
    pub instance_id: String,
    pub started_at_ms: u128,
    pub config_dir: String,
}

/// The resolved, canonical configuration directory for a profile. Two different
/// spellings of the same real directory map to the same lock and activation
/// channel.
fn canonical_config_dir(config_dir: &Path) -> Result<PathBuf, AppError> {
    fs::create_dir_all(config_dir).map_err(|_| {
        AppError::new(
            "config_dir_unavailable",
            "The application configuration directory cannot be created or written.",
        )
    })?;
    fs::canonicalize(config_dir).map_err(|_| {
        AppError::new(
            "config_dir_unavailable",
            "The application configuration directory cannot be resolved.",
        )
    })
}

pub struct ProfileLock {
    file: File,
    /// Written at acquisition so a future owner can report who holds the lock.
    /// Nothing reads it back yet.
    #[allow(dead_code)]
    meta_path: PathBuf,
}

impl ProfileLock {
    #[allow(dead_code)]
    pub fn meta_path(&self) -> &Path {
        &self.meta_path
    }
}

pub enum LockState {
    Acquired(ProfileLock),
    /// Carries the existing owner's metadata. The launcher currently only
    /// matches on the variant, so the payload is not read.
    HeldElsewhere(#[allow(dead_code)] OwnerMetadata),
}

/// Acquire an OS-backed exclusive lock scoped to the canonical configuration
/// directory. The lock file and metadata live inside the config directory, so
/// different profiles (including isolated smoke-test profiles) never contend.
pub fn acquire_profile_lock(config_dir: &Path) -> Result<LockState, AppError> {
    let canonical = canonical_config_dir(config_dir)?;
    let lock_path = canonical.join(LOCK_FILE);
    let meta_path = canonical.join(META_FILE);

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|_| AppError::new("profile_lock", "Could not open the profile lock file."))?;

    match FileExt::try_lock_exclusive(&file) {
        Ok(true) => {
            let metadata = OwnerMetadata {
                pid: std::process::id(),
                instance_id: fresh_short_id(),
                started_at_ms: now_millis(),
                config_dir: canonical.to_string_lossy().to_string(),
            };
            let _ = write_meta(&meta_path, &metadata);
            Ok(LockState::Acquired(ProfileLock { file, meta_path }))
        }
        Ok(false) => {
            let owner = read_meta(&meta_path).unwrap_or_else(|| OwnerMetadata {
                pid: 0,
                instance_id: "unknown".to_string(),
                started_at_ms: 0,
                config_dir: canonical.to_string_lossy().to_string(),
            });
            Ok(LockState::HeldElsewhere(owner))
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            let owner = read_meta(&meta_path).unwrap_or_else(|| OwnerMetadata {
                pid: 0,
                instance_id: "unknown".to_string(),
                started_at_ms: 0,
                config_dir: canonical.to_string_lossy().to_string(),
            });
            Ok(LockState::HeldElsewhere(owner))
        }
        Err(error) => Err(AppError::new("profile_lock", format!("{error}"))),
    }
}

impl Drop for ProfileLock {
    fn drop(&mut self) {
        // Release the exclusive OS lock but leave both files in place: deleting
        // or replacing the lock file to bypass a lock is explicitly forbidden,
        // and the OS lock itself already disappeared with the process on crash.
        let _ = FileExt::unlock(&self.file);
    }
}

fn write_meta(path: &Path, metadata: &OwnerMetadata) -> std::io::Result<()> {
    let tmp = path.with_extension("meta.json.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&tmp)?;
    serde_json::to_writer(&mut file, metadata)?;
    drop(file);
    fs::rename(&tmp, path)
}

fn read_meta(path: &Path) -> Option<OwnerMetadata> {
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn fresh_short_id() -> String {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{pid:x}-{nanos:x}")
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationOutcome {
    /// The owner acknowledged the request; its window was shown/focused.
    Activated,
    /// No owner responded in time. The profile is owned by another instance.
    AlreadyRunning,
    /// The request could not even be written.
    WriteFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivationRequest {
    from_pid: u32,
    from_instance_id: String,
    requested_at_ms: u128,
}

fn activation_dir(config_dir: &Path) -> Result<PathBuf, AppError> {
    let canonical = canonical_config_dir(config_dir)?;
    Ok(canonical.join(ACTIVATION_DIR))
}

/// Second-instance path: write a profile-scoped activation request, wait for
/// the owner's acknowledgement, then let the caller exit. This is purely a
/// notification; it never reads or writes settings and never touches services.
pub fn request_activation(
    config_dir: &Path,
    wait: Option<Duration>,
) -> Result<ActivationOutcome, AppError> {
    let dir = activation_dir(config_dir)?;
    fs::create_dir_all(dir.join("inbox")).map_err(|_| {
        AppError::new(
            "activation_channel",
            "Could not create the activation request inbox.",
        )
    })?;
    fs::create_dir_all(dir.join("acks")).map_err(|_| {
        AppError::new(
            "activation_channel",
            "Could not create the activation acknowledgement inbox.",
        )
    })?;

    let token = fresh_short_id();
    let request_name = format!("{}.request", token);
    let ack_name = format!("{}.ack", token);
    let request = ActivationRequest {
        from_pid: std::process::id(),
        from_instance_id: token.clone(),
        requested_at_ms: now_millis(),
    };
    let request_path = dir.join("inbox").join(&request_name);
    let ack_path = dir.join("acks").join(&ack_name);

    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&request_path)?;
        serde_json::to_writer(&mut file, &request)?;
        drop(file);
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&request_path);
        return Ok(ActivationOutcome::WriteFailed);
    }

    let deadline = Instant::now() + wait.unwrap_or(REQUEST_ACK_WAIT);
    while Instant::now() < deadline {
        if ack_path.exists() {
            let _ = fs::remove_file(&request_path);
            let _ = fs::remove_file(&ack_path);
            return Ok(ActivationOutcome::Activated);
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = fs::remove_file(&request_path);
    Ok(ActivationOutcome::AlreadyRunning)
}

/// Owner-side watcher: poll the profile-scoped inbox for activation requests.
/// Each request invokes `on_activate` (show/unminimize/focus) and is then
/// acknowledged. Returns a handle that can be used to stop the watcher.
pub struct ActivationWatcher {
    quit: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ActivationWatcher {
    pub fn spawn<F>(config_dir: &Path, on_activate: F) -> Result<Self, AppError>
    where
        F: Fn() + Send + Sync + 'static,
    {
        let dir = activation_dir(config_dir)?;
        fs::create_dir_all(dir.join("inbox")).map_err(|_| {
            AppError::new(
                "activation_channel",
                "Could not create the activation request inbox.",
            )
        })?;
        fs::create_dir_all(dir.join("acks")).map_err(|_| {
            AppError::new(
                "activation_channel",
                "Could not create the activation acknowledgement inbox.",
            )
        })?;

        let quit = Arc::new(AtomicBool::new(false));
        let watcher_quit = quit.clone();
        let callback = Arc::new(Mutex::new(on_activate));
        let thread = thread::spawn(move || {
            while !watcher_quit.load(Ordering::SeqCst) {
                if let Ok(entries) = fs::read_dir(dir.join("inbox")) {
                    for entry in entries.flatten() {
                        if watcher_quit.load(Ordering::SeqCst) {
                            break;
                        }
                        let name = entry.file_name().to_string_lossy().to_string();
                        if !name.ends_with(".request") {
                            continue;
                        }
                        let is_stale = entry
                            .metadata()
                            .and_then(|meta| meta.modified())
                            .and_then(|modified| modified.elapsed().map_err(std::io::Error::other))
                            .map(|age| age > MAX_REQUEST_AGE)
                            .unwrap_or(false);
                        if is_stale {
                            let _ = fs::remove_file(entry.path());
                            continue;
                        }
                        if let Ok(callback) = callback.lock() {
                            callback();
                        }
                        let token = name.strip_suffix(".request").unwrap_or(&name);
                        let ack_path = dir.join("acks").join(format!("{token}.ack"));
                        let _ = fs::write(&ack_path, now_millis().to_string().as_bytes());
                        let _ = fs::remove_file(entry.path());
                    }
                }
                thread::sleep(WATCH_INTERVAL);
            }
        });
        Ok(Self {
            quit,
            thread: Some(thread),
        })
    }

    pub fn stop(&mut self) {
        self.quit.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ActivationWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let unique = format!("{}-{:x}", label, now_millis());
        std::env::temp_dir()
            .join("secret-tunnel-ownership-tests")
            .join(unique)
    }

    #[test]
    fn sequential_acquisition_with_equivalent_path_spellings() {
        let root = temp_root("eq-spellings");
        let dir = root.join("profiles").join("alpha");
        let a = acquire_profile_lock(&dir).unwrap();
        assert!(matches!(a, LockState::Acquired(_)));

        // Dropping releases the lock so a later spelling can reacquire.
        drop(a);

        // Equivalent spellings of the same directory resolve to one lock file:
        // a relative jump through the same real directory.
        let spelled_a = dir.join("..").join(dir.file_name().unwrap());
        let b = acquire_profile_lock(&spelled_a).unwrap();
        assert!(matches!(b, LockState::Acquired(_)));
        drop(b);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn second_acquisition_is_held_elsewhere_until_release() {
        let root = temp_root("held-elsewhere");
        let dir = root.join("profiles").join("beta");
        let first = acquire_profile_lock(&dir).unwrap();
        assert!(matches!(first, LockState::Acquired(_)));

        let second = acquire_profile_lock(&dir).unwrap();
        let LockState::HeldElsewhere(owner) = second else {
            panic!("expected HeldElsewhere while the first lock is held");
        };
        assert_eq!(
            owner.config_dir,
            fs::canonicalize(&dir).unwrap().to_string_lossy()
        );

        drop(first);
        let third = acquire_profile_lock(&dir).unwrap();
        assert!(matches!(third, LockState::Acquired(_)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stale_metadata_does_not_block_relaunch() {
        let root = temp_root("stale-meta");
        let dir = root.join("profiles").join("gamma");
        let first = acquire_profile_lock(&dir).unwrap();
        let LockState::Acquired(lock) = first else {
            panic!("expected acquired");
        };
        let meta_path = lock.meta_path().to_path_buf();
        // Simulate a crash: the metadata file remains but the OS lock is gone.
        drop(lock);
        assert!(meta_path.exists());

        let relaunch = acquire_profile_lock(&dir).unwrap();
        assert!(matches!(relaunch, LockState::Acquired(_)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn distinct_profiles_do_not_contend() {
        let root = temp_root("distinct");
        let prod = root.join("profiles").join("production");
        let test = root.join("profiles").join("test-isolated");
        let prod_lock = acquire_profile_lock(&prod).unwrap();
        let test_lock = acquire_profile_lock(&test).unwrap();
        assert!(matches!(prod_lock, LockState::Acquired(_)));
        assert!(matches!(test_lock, LockState::Acquired(_)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn activation_request_is_acknowledged_by_watcher() {
        let root = temp_root("activation");
        let dir = root.join("profiles").join("delta");
        let _ = acquire_profile_lock(&dir).unwrap();

        let activated = Arc::new(Mutex::new(false));
        let callback = {
            let activated = activated.clone();
            move || {
                *activated.lock().unwrap() = true;
            }
        };
        let mut watcher = ActivationWatcher::spawn(&dir, callback).unwrap();

        let outcome = request_activation(&dir, Some(Duration::from_secs(5))).unwrap();
        assert_eq!(outcome, ActivationOutcome::Activated);

        // Give the watcher a moment to observe; then confirm the callback ran.
        let _ = outcome;
        thread::sleep(Duration::from_millis(400));
        assert!(*activated.lock().unwrap(), "activation callback never ran");
        watcher.stop();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn activation_without_owner_is_already_running() {
        let root = temp_root("no-owner");
        let dir = root.join("profiles").join("epsilon");
        let _ = fs::create_dir_all(&dir);
        // No lock held, no watcher: the request is written but never acked.
        let outcome = request_activation(&dir, Some(Duration::from_millis(500))).unwrap();
        assert_eq!(outcome, ActivationOutcome::AlreadyRunning);
        let _ = fs::remove_dir_all(&root);
    }
}
