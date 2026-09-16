use crate::error::AppError;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub settings_path: PathBuf,
    pub managed_config_path: PathBuf,
    pub diagnostics_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    Read,
    ReadWrite,
}

impl AccessMode {
    pub fn parse(value: &str) -> Result<Self, AppError> {
        match value {
            "read" => Ok(Self::Read),
            "read_write" => Ok(Self::ReadWrite),
            _ => Err(AppError::new(
                "invalid_access_mode",
                "Choose either read or read+write.",
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadWrite => "read_write",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default = "default_access_mode")]
    pub access_mode: AccessMode,
    #[serde(default = "default_zrok_name")]
    pub zrok_name: String,
    #[serde(default = "default_public_path_token")]
    pub public_path_token: String,
    #[serde(default = "default_gpt_repo_mcp_path")]
    pub gpt_repo_mcp_path: String,
}

impl Settings {
    fn normalize(mut self) -> Self {
        if self.version == 0 {
            self.version = default_version();
        }
        self.workspace_path = self
            .workspace_path
            .map(|path| normalize_windows_verbatim_prefix(&path));
        if self.zrok_name.trim().is_empty() {
            self.zrok_name = default_zrok_name();
        }
        if self.public_path_token.trim().is_empty() {
            self.public_path_token = default_public_path_token();
        }
        if self.gpt_repo_mcp_path.trim().is_empty() {
            self.gpt_repo_mcp_path = default_gpt_repo_mcp_path();
        }
        self
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: default_version(),
            workspace_path: None,
            access_mode: default_access_mode(),
            zrok_name: default_zrok_name(),
            public_path_token: default_public_path_token(),
            gpt_repo_mcp_path: default_gpt_repo_mcp_path(),
        }
    }
}

pub fn app_paths() -> Result<AppPaths, AppError> {
    let config_dir = match config_dir_override()? {
        Some(dir) => dir,
        None => {
            let dirs = ProjectDirs::from("com", "GeorgeFejer", "ChatGPT Local MCP Launcher")
                .ok_or_else(|| AppError::new("app_data", "Could not resolve app data directory."))?;
            dirs.config_dir().to_path_buf()
        }
    };
    fs::create_dir_all(&config_dir).map_err(|_| {
        AppError::new(
            "config_dir_unavailable",
            "The application configuration directory cannot be created or written.",
        )
    })?;
    Ok(AppPaths {
        config_dir: config_dir.clone(),
        settings_path: config_dir.join("settings.json"),
        managed_config_path: config_dir.join("gpt-repo-mcp.config.json"),
        diagnostics_path: config_dir.join("diagnostics.log"),
    })
}

/// `SECRET_TUNNEL_CONFIG_DIR` selects the exact configuration directory used for
/// settings, the managed MCP configuration, and diagnostics. When set it is used
/// verbatim: no organization/application suffix is appended, and an invalid or
/// unwritable override fails hard rather than falling back to the production
/// profile. This is the isolation mechanism used by the live smoke harness.
fn config_dir_override() -> Result<Option<PathBuf>, AppError> {
    let Some(value) = env::var_os("SECRET_TUNNEL_CONFIG_DIR") else {
        return Ok(None);
    };
    let trimmed = value.to_string_lossy().trim().to_string();
    if trimmed.is_empty() {
        return Err(AppError::new(
            "invalid_config_dir",
            "SECRET_TUNNEL_CONFIG_DIR must not be empty.",
        ));
    }
    let path = PathBuf::from(&trimmed);
    if !path.is_absolute() {
        return Err(AppError::new(
            "invalid_config_dir",
            "SECRET_TUNNEL_CONFIG_DIR must be an absolute path.",
        ));
    }
    Ok(Some(path))
}

/// Read settings without persisting anything. This is the observation path:
/// status polling must never rewrite the settings file.
pub fn load_settings(paths: &AppPaths) -> Result<Option<Settings>, AppError> {
    if !paths.settings_path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&paths.settings_path)?;
    let settings: Settings = serde_json::from_str(&raw)?;
    Ok(Some(settings))
}

/// Load existing settings, or create defaults on a genuine first run.
/// Reads are observation-only; normalization is applied in memory only.
/// A missing or temporarily unavailable workspace is preserved rather than
/// replaced with `None`: it remains stored so the user does not lose their
/// selection, but startup is blocked until the workspace is reachable again.
pub fn load_or_create_settings(paths: &AppPaths) -> Result<Settings, AppError> {
    if let Some(raw) = load_settings(paths)? {
        return Ok(raw.normalize());
    }

    let settings = Settings::default();
    save_settings(paths, &settings)?;
    Ok(settings)
}

/// Return the effective workspace path by validating the stored selection.
/// This does not modify settings. It returns `Ok(None)` when no workspace
/// is configured, `Ok(Some(path))` when the workspace is available and safe,
/// and `Err` with a specific code when the stored selection is blocked.
pub fn effective_workspace_path(
    workspace_path: &Option<String>,
) -> Result<Option<String>, AppError> {
    let Some(path) = workspace_path.as_deref() else {
        return Ok(None);
    };
    validate_workspace_path(path).map(Some)
}

pub fn save_settings(paths: &AppPaths, settings: &Settings) -> Result<(), AppError> {
    if let Some(parent) = paths.settings_path.parent() {
        fs::create_dir_all(parent).map_err(|_| {
            AppError::new(
                "settings_write_failed",
                "Could not create the settings directory.",
            )
        })?;
    }
    let tmp = paths
        .settings_path
        .with_extension(format!(
            "json.tmp.{}.{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
    if let Err(error) = fs::write(&tmp, serde_json::to_vec_pretty(settings)?) {
        let _ = fs::remove_file(&tmp);
        return Err(AppError::new("settings_write_failed", error.to_string()));
    }
    if let Err(error) = fs::rename(&tmp, &paths.settings_path) {
        let _ = fs::remove_file(&tmp);
        return Err(AppError::new("settings_write_failed", error.to_string()));
    }
    Ok(())
}

/// Compute settings with any in-memory launch overrides applied. Does NOT
/// persist the result: the override values live only for this process's
/// lifetime. This allows the smoke harness to inject a test workspace, zrok
/// name, and path token without touching the production settings file.
pub fn apply_launch_environment_overrides(
    paths: &AppPaths,
) -> Result<Option<Settings>, AppError> {
    // Purely in-memory: load existing settings without creating a file, and use
    // defaults only as a base for the override merge. Nothing is persisted here.
    let mut settings = load_settings(paths)?.unwrap_or_default();
    let mut changed = false;

    if let Some(path) = launch_env("SECRET_TUNNEL_WORKSPACE_PATH") {
        settings.workspace_path = Some(validate_workspace_path(&path)?);
        changed = true;
    }
    if let Some(mode) = launch_env("SECRET_TUNNEL_ACCESS_MODE") {
        settings.access_mode = AccessMode::parse(&mode)?;
        changed = true;
    }
    if let Some(name) = launch_env("SECRET_TUNNEL_ZROK_NAME") {
        settings.zrok_name = validate_zrok_name(&name)?;
        changed = true;
    }
    if let Some(token) = launch_env("SECRET_TUNNEL_PUBLIC_PATH_TOKEN") {
        settings.public_path_token = validate_public_path_token(&token)?;
        changed = true;
    }

    Ok(changed.then_some(settings))
}

/// Deterministic, process-independent revision for a settings snapshot. Two
/// snapshots with identical configuration yield the same revision, so the
/// lifecycle coordinator can coalesce equivalent starts.
pub fn settings_revision(settings: &Settings) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    settings.workspace_path.hash(&mut hasher);
    settings.access_mode.as_str().hash(&mut hasher);
    settings.zrok_name.hash(&mut hasher);
    settings.public_path_token.hash(&mut hasher);
    settings.gpt_repo_mcp_path.hash(&mut hasher);
    hasher.finish()
}

pub fn validate_zrok_name(value: &str) -> Result<String, AppError> {
    let normalized = value.trim().to_ascii_lowercase();
    if !(4..=32).contains(&normalized.len()) {
        return Err(AppError::new(
            "invalid_zrok_name",
            "Use 4 to 32 lowercase letters, numbers, or hyphens.",
        ));
    }
    if normalized.starts_with('-') || normalized.ends_with('-') {
        return Err(AppError::new(
            "invalid_zrok_name",
            "The zrok name cannot start or end with a hyphen.",
        ));
    }
    if !normalized
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(AppError::new(
            "invalid_zrok_name",
            "Use only lowercase letters, numbers, and hyphens.",
        ));
    }
    Ok(normalized)
}

pub fn validate_public_path_token(value: &str) -> Result<String, AppError> {
    let token = value.trim();
    if !(8..=64).contains(&token.len()) {
        return Err(AppError::new(
            "invalid_public_path_token",
            "Use 8 to 64 letters, numbers, hyphens, or underscores.",
        ));
    }
    if !token
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(AppError::new(
            "invalid_public_path_token",
            "Use only letters, numbers, hyphens, or underscores.",
        ));
    }
    Ok(token.to_string())
}

pub fn validate_workspace_path(path: &str) -> Result<String, AppError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(AppError::new("missing_folder", "Select a folder first."));
    }
    let raw = PathBuf::from(trimmed);
    if !raw.is_absolute() {
        return Err(AppError::new(
            "invalid_folder",
            "Use an absolute folder path.",
        ));
    }
    let canonical = fs::canonicalize(&raw).map_err(|_| {
        AppError::new(
            "folder_unavailable",
            "That folder does not exist or cannot be read.",
        )
    })?;
    if !canonical.is_dir() {
        return Err(AppError::new(
            "invalid_folder",
            "The selected path is not a folder.",
        ));
    }
    if is_filesystem_root(&canonical) {
        return Err(AppError::new(
            "unsafe_folder",
            "Do not expose a drive root or filesystem root.",
        ));
    }
    if is_home_directory(&canonical) || is_system_directory(&canonical) {
        return Err(AppError::new(
            "unsafe_folder",
            "Pick a project folder, not a home, system, or application folder.",
        ));
    }
    Ok(normalize_windows_verbatim_prefix(
        &canonical.to_string_lossy(),
    ))
}

pub fn write_managed_mcp_config(paths: &AppPaths, settings: &Settings) -> Result<(), AppError> {
    let root = settings
        .workspace_path
        .as_deref()
        .ok_or_else(|| AppError::new("missing_folder", "Select a folder first."))
        .and_then(validate_workspace_path)?;
    let root_path = Path::new(&root);
    let display_name = root_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Selected folder");
    let writes_enabled = settings.access_mode == AccessMode::ReadWrite;
    let document = json!({
        "repos": [{
            "repo_id": "workspace",
            "display_name": display_name,
            "root": root,
            "allow_non_git": true,
            "writes": {
                "enabled": writes_enabled,
                "allowed_globs": ["**"],
                "denied_globs": [".git/**", ".env", ".env.*", "**/*.pem", "**/*.key"],
                "max_bytes_per_write": 1048576
            },
            "operations": {
                "enabled": false
            }
        }],
        "limits": {
            "max_files": 50,
            "max_bytes_per_file": 128000,
            "max_total_bytes": 750000
        }
    });

    if let Some(parent) = paths.managed_config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = paths.managed_config_path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(&document)?)?;
    fs::rename(tmp, &paths.managed_config_path)?;
    Ok(())
}

pub fn mcp_url(settings: &Settings) -> String {
    format!(
        "https://{}.shares.zrok.io/t/{}/mcp",
        settings.zrok_name, settings.public_path_token
    )
}

/// Replace credential-bearing strings with a redacted marker so that
/// diagnostic output and logs do not leak secrets.
pub fn redact_secrets(input: &str, secrets: &[String]) -> String {
    let mut out = input.to_string();
    for secret in secrets {
        if !secret.is_empty() && secret.len() >= 4 {
            out = out.replace(secret, "[redacted]");
        }
    }
    out
}

/// Collect secrets that should be redacted from diagnostic output.
pub fn secrets_for_redaction(settings: &Settings) -> Vec<String> {
    let mut secrets = Vec::new();
    if !settings.public_path_token.is_empty() {
        secrets.push(settings.public_path_token.clone());
    }
    // Also redact the enable token if present in the process environment,
    // which is transient but still a credential.
    if let Some(token) = env::var_os("SECRET_TUNNEL_ZROK_ENABLE_TOKEN") {
        let t = token.to_string_lossy().trim().to_string();
        if t.len() >= 8 {
            secrets.push(t);
        }
    }
    secrets
}

fn launch_env(variable: &str) -> Option<String> {
    env::var_os(variable)
        .map(|value| value.to_string_lossy().trim().to_string())
        .filter(|value| !value.is_empty())
}

fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none()
}

fn is_home_directory(path: &Path) -> bool {
    ["USERPROFILE", "HOME"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .filter_map(|home| fs::canonicalize(home).ok())
        .any(|home| same_path(&home, path))
}

fn is_system_directory(path: &Path) -> bool {
    system_directory_candidates()
        .into_iter()
        .filter_map(|candidate| fs::canonicalize(candidate).ok())
        .any(|candidate| same_path(&candidate, path))
}

fn system_directory_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for variable in [
        "WINDIR",
        "SystemRoot",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
    ] {
        if let Some(value) = std::env::var_os(variable) {
            candidates.push(PathBuf::from(value));
        }
    }
    if cfg!(windows) {
        candidates.extend([
            PathBuf::from(r"C:\Windows"),
            PathBuf::from(r"C:\Program Files"),
        ]);
    } else {
        candidates.extend([
            PathBuf::from("/bin"),
            PathBuf::from("/etc"),
            PathBuf::from("/usr"),
        ]);
    }
    candidates
}

fn same_path(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        normalize_windows_verbatim_prefix(&left.to_string_lossy())
            .eq_ignore_ascii_case(&normalize_windows_verbatim_prefix(&right.to_string_lossy()))
    } else {
        left == right
    }
}

fn default_version() -> u32 {
    1
}

fn default_access_mode() -> AccessMode {
    AccessMode::Read
}

fn default_public_path_token() -> String {
    Uuid::new_v4().simple().to_string()
}

fn default_zrok_name() -> String {
    let token = default_public_path_token();
    format!("gptmcp{}", &token[..12])
}

pub fn fresh_public_path_token() -> String {
    default_public_path_token()
}

pub fn fresh_zrok_name() -> String {
    let token = fresh_public_path_token();
    format!("gptmcp{}", &token[..12])
}

fn default_gpt_repo_mcp_path() -> String {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    home.map(PathBuf::from)
        .map(|path| {
            path.join("Documents")
                .join("GitHub")
                .join("gpt-repo-mcp")
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_else(|| "gpt-repo-mcp".to_string())
}

pub fn normalize_windows_verbatim_prefix(path: &str) -> String {
    if let Some(stripped) = path.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{stripped}")
    } else if let Some(stripped) = path.strip_prefix("\\\\?\\") {
        stripped.to_string()
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_launch_environment_overrides, effective_workspace_path, fresh_public_path_token,
        fresh_zrok_name, is_home_directory, load_or_create_settings, load_settings,
        normalize_windows_verbatim_prefix, redact_secrets, save_settings,
        validate_public_path_token, validate_zrok_name, AccessMode, AppPaths, Settings,
    };
    use std::sync::Mutex;
    use std::{env, fs};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn test_paths() -> AppPaths {
        let root = env::temp_dir().join(format!(
            "secret-tunnel-settings-test-{}",
            std::process::id()
        ));
        AppPaths {
            config_dir: root.clone(),
            settings_path: root.join("settings.json"),
            managed_config_path: root.join("gpt-repo-mcp.config.json"),
            diagnostics_path: root.join("diagnostics.log"),
        }
    }

    #[test]
    fn validates_zrok_names() {
        assert_eq!(validate_zrok_name("My-Mcp1").unwrap(), "my-mcp1");
        assert!(validate_zrok_name("abc").is_err());
        assert!(validate_zrok_name("-abcd").is_err());
        assert!(validate_zrok_name("abc_def").is_err());
    }

    #[test]
    fn validates_public_path_tokens() {
        assert_eq!(
            validate_public_path_token(" token_123-abc ").unwrap(),
            "token_123-abc"
        );
        assert!(validate_public_path_token("short").is_err());
        assert!(validate_public_path_token("has space").is_err());
        assert!(validate_public_path_token("has/slash").is_err());
    }

    #[test]
    fn generates_fresh_zrok_identity() {
        let name = fresh_zrok_name();
        let token = fresh_public_path_token();
        assert_eq!(validate_zrok_name(&name).unwrap(), name);
        assert_eq!(validate_public_path_token(&token).unwrap(), token);
        assert_ne!(fresh_zrok_name(), name);
        assert_ne!(fresh_public_path_token(), token);
        assert!(name.starts_with("gptmcp"));
    }

    #[test]
    fn parses_access_modes() {
        assert_eq!(AccessMode::parse("read").unwrap(), AccessMode::Read);
        assert_eq!(
            AccessMode::parse("read_write").unwrap(),
            AccessMode::ReadWrite
        );
        assert!(AccessMode::parse("ship").is_err());
    }

    #[test]
    fn normalizes_windows_verbatim_prefixes() {
        assert_eq!(
            normalize_windows_verbatim_prefix(r"\\?\C:\Users\George\Project"),
            r"C:\Users\George\Project"
        );
        assert_eq!(
            normalize_windows_verbatim_prefix(r"\\?\UNC\server\share\Project"),
            r"\\server\share\Project"
        );
        assert_eq!(
            normalize_windows_verbatim_prefix(r"C:\Users\George\Project"),
            r"C:\Users\George\Project"
        );
    }

    #[test]
    fn detects_configured_home_directory() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = env::temp_dir().join(format!("secret-tunnel-home-test-{}", std::process::id()));
        fs::create_dir_all(&home).unwrap();
        let previous = env::var_os("USERPROFILE");
        env::set_var("USERPROFILE", &home);
        assert!(is_home_directory(&fs::canonicalize(&home).unwrap()));
        if let Some(previous) = previous {
            env::set_var("USERPROFILE", previous);
        } else {
            env::remove_var("USERPROFILE");
        }
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn preserves_unsafe_workspace_in_stored_settings() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = env::temp_dir().join(format!(
            "secret-tunnel-settings-test-{}",
            std::process::id()
        ));
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let paths = AppPaths {
            config_dir: root.clone(),
            settings_path: root.join("settings.json"),
            managed_config_path: root.join("gpt-repo-mcp.config.json"),
            diagnostics_path: root.join("diagnostics.log"),
        };
        let previous = env::var_os("USERPROFILE");
        env::set_var("USERPROFILE", &home);
        let settings = Settings {
            workspace_path: Some(home.to_string_lossy().to_string()),
            ..Settings::default()
        };
        save_settings(&paths, &settings).unwrap();

        // load_or_create_settings must preserve the stored path (observation-only)
        let loaded = load_or_create_settings(&paths).unwrap();
        assert_eq!(loaded.workspace_path, Some(home.to_string_lossy().to_string()));

        // effective_workspace_path blocks the unsafe selection without erasing it
        let effective = effective_workspace_path(&loaded.workspace_path);
        assert!(effective.is_err());
        let err = effective.unwrap_err();
        assert_eq!(err.code, "unsafe_folder");

        // The on-disk file is unchanged
        let reloaded = load_settings(&paths).unwrap().unwrap();
        assert_eq!(reloaded.workspace_path, Some(home.to_string_lossy().to_string()));

        if let Some(previous) = previous {
            env::set_var("USERPROFILE", previous);
        } else {
            env::remove_var("USERPROFILE");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preserves_unavailable_workspace_in_stored_settings() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = env::temp_dir().join(format!(
            "secret-tunnel-unavailable-test-{}",
            std::process::id()
        ));
        let paths = AppPaths {
            config_dir: root.clone(),
            settings_path: root.join("settings.json"),
            managed_config_path: root.join("gpt-repo-mcp.config.json"),
            diagnostics_path: root.join("diagnostics.log"),
        };
        let missing = r"C:\nonexistent\fake\folder";
        let settings = Settings {
            workspace_path: Some(missing.to_string()),
            ..Settings::default()
        };
        save_settings(&paths, &settings).unwrap();

        // load_or_create_settings must preserve the stored path (observation-only)
        let loaded = load_or_create_settings(&paths).unwrap();
        assert_eq!(loaded.workspace_path, Some(missing.to_string()));

        // effective_workspace_path reports the folder as unavailable
        let effective = effective_workspace_path(&loaded.workspace_path);
        assert!(effective.is_err());
        let err = effective.unwrap_err();
        assert_eq!(err.code, "folder_unavailable");

        // The on-disk file is unchanged
        let reloaded = load_settings(&paths).unwrap().unwrap();
        assert_eq!(reloaded.workspace_path, Some(missing.to_string()));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn applies_launch_environment_overrides_in_memory_only() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = env::temp_dir().join(format!(
            "secret-tunnel-launch-env-test-{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let paths = AppPaths {
            config_dir: root.clone(),
            settings_path: root.join("settings.json"),
            managed_config_path: root.join("gpt-repo-mcp.config.json"),
            diagnostics_path: root.join("diagnostics.log"),
        };

        let previous_workspace = env::var_os("SECRET_TUNNEL_WORKSPACE_PATH");
        let previous_mode = env::var_os("SECRET_TUNNEL_ACCESS_MODE");
        let previous_name = env::var_os("SECRET_TUNNEL_ZROK_NAME");
        let previous_token = env::var_os("SECRET_TUNNEL_PUBLIC_PATH_TOKEN");

        env::set_var("SECRET_TUNNEL_WORKSPACE_PATH", &workspace);
        env::set_var("SECRET_TUNNEL_ACCESS_MODE", "read_write");
        env::set_var("SECRET_TUNNEL_ZROK_NAME", "Launch-MCP-1");
        env::set_var("SECRET_TUNNEL_PUBLIC_PATH_TOKEN", "token_123456");

        let result = apply_launch_environment_overrides(&paths).unwrap();
        assert!(result.is_some());
        let overridden = result.unwrap();
        assert_eq!(overridden.access_mode, AccessMode::ReadWrite);
        assert_eq!(overridden.zrok_name, "launch-mcp-1");
        assert_eq!(overridden.public_path_token, "token_123456");
        assert_eq!(
            overridden.workspace_path,
            Some(normalize_windows_verbatim_prefix(
                &fs::canonicalize(&workspace).unwrap().to_string_lossy()
            ))
        );

        // The settings file was NOT modified by the in-memory override
        let persisted = load_settings(&paths).unwrap();
        assert!(persisted.is_none(), "no settings file should have been created");

        restore_env("SECRET_TUNNEL_WORKSPACE_PATH", previous_workspace);
        restore_env("SECRET_TUNNEL_ACCESS_MODE", previous_mode);
        restore_env("SECRET_TUNNEL_ZROK_NAME", previous_name);
        restore_env("SECRET_TUNNEL_PUBLIC_PATH_TOKEN", previous_token);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn redacts_credentials_from_output() {
        let token = "abcdef1234567890";
        let url = format!("https://foo.shares.zrok.io/t/{token}/mcp");
        let secrets = vec![token.to_string()];
        let redacted = redact_secrets(&url, &secrets);
        assert!(!redacted.contains(token));
        assert!(redacted.contains("[redacted]"));
    }

    fn restore_env(variable: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            env::set_var(variable, value);
        } else {
            env::remove_var(variable);
        }
    }
}
