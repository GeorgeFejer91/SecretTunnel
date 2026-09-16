use crate::error::AppError;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppPaths {
    pub settings_path: PathBuf,
    pub managed_config_path: PathBuf,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    let dirs = ProjectDirs::from("com", "GeorgeFejer", "ChatGPT Local MCP Launcher")
        .ok_or_else(|| AppError::new("app_data", "Could not resolve app data directory."))?;
    let config_dir = dirs.config_dir();
    fs::create_dir_all(config_dir)?;
    Ok(AppPaths {
        settings_path: config_dir.join("settings.json"),
        managed_config_path: config_dir.join("gpt-repo-mcp.config.json"),
    })
}

pub fn load_or_create_settings(paths: &AppPaths) -> Result<Settings, AppError> {
    if paths.settings_path.exists() {
        let raw = fs::read_to_string(&paths.settings_path)?;
        let settings: Settings = serde_json::from_str(&raw)?;
        let normalized = settings.normalize();
        save_settings(paths, &normalized)?;
        return Ok(normalized);
    }

    let settings = Settings::default();
    save_settings(paths, &settings)?;
    Ok(settings)
}

pub fn save_settings(paths: &AppPaths, settings: &Settings) -> Result<(), AppError> {
    if let Some(parent) = paths.settings_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = paths.settings_path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(settings)?)?;
    fs::rename(tmp, &paths.settings_path)?;
    Ok(())
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
            "invalid_folder",
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
    Ok(canonical.to_string_lossy().to_string())
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
        "https://{}.share.zrok.io/t/{}/mcp",
        settings.zrok_name, settings.public_path_token
    )
}

pub fn gpt_repo_mcp_exists(settings: &Settings) -> bool {
    let root = Path::new(&settings.gpt_repo_mcp_path);
    root.join("package.json").is_file() && root.join("src").is_dir()
}

fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none()
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

#[cfg(test)]
mod tests {
    use super::{validate_zrok_name, AccessMode};

    #[test]
    fn validates_zrok_names() {
        assert_eq!(validate_zrok_name("My-Mcp1").unwrap(), "my-mcp1");
        assert!(validate_zrok_name("abc").is_err());
        assert!(validate_zrok_name("-abcd").is_err());
        assert!(validate_zrok_name("abc_def").is_err());
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
}
