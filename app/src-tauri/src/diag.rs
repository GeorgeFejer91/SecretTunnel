use crate::settings::AppPaths;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Append-only diagnostics log. Writes JSON-lines to the file at
/// `AppPaths::diagnostics_path`. Thread-safe via an internal mutex.
#[derive(Clone)]
pub struct Diagnostics {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

use std::sync::Arc;

impl Diagnostics {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            path: paths.diagnostics_path.clone(),
            lock: Arc::new(Mutex::new(())),
        }
    }

    /// Append a structured event to the diagnostics log. Failures are
    /// swallowed: diagnostics must never crash the application.
    pub fn event(&self, stage: &str, message: &str) {
        self.event_inner(stage, message);
    }

    fn event_inner(&self, stage: &str, message: &str) {
        let Ok(_guard) = self.lock.lock() else {
            return;
        };
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let document = json!({
            "ts": timestamp_ms,
            "stage": stage,
            "message": message,
        });
        let mut line = serde_json::to_string(&document).unwrap_or_default();
        line.push('\n');
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

/// Identity of this build, emitted as the first diagnostic event.
pub fn build_identity() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let commit = option_env!("GITHUB_SHA").unwrap_or("unknown");
    format!("secret-tunnel {version} ({os}-{arch}, commit {commit})")
}

/// Compute a short fingerprint (first 12 hex chars of SHA-256) of the
/// current executable. Used to tie a running process to a specific
/// packaged build. Failures return "unknown".
pub fn executable_fingerprint() -> String {
    let Ok(exe) = std::env::current_exe() else {
        return "unknown".to_string();
    };
    let Ok(bytes) = fs::read(&exe) else {
        return "unknown".to_string();
    };
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(&bytes);
    let hex = digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    hex.chars().take(12).collect()
}

/// Log the bootstrap event: build identity, PID, executable path, and
/// whether a config-dir override is active.
pub fn bootstrap(diagnostics: &Diagnostics, config_dir_override: bool) {
    diagnostics.event(
        "bootstrap",
        &format!(
            "pid={} exe={} identity={} configDirOverride={}",
            std::process::id(),
            std::env::current_exe()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
            build_identity(),
            config_dir_override,
        ),
    );
}
