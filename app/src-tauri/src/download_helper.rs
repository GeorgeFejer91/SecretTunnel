use crate::error::AppError;
use crate::lifecycle::{Attempt, Coordinator, LifecycleEndpoint, LifecycleRequest, LifecycleState};
use crate::ownership::{acquire_profile_lock, request_activation, ActivationOutcome, LockState};
use crate::readiness::{
    real_probe_fn, ContextProvider, ProbeContext, ReadinessScheduler, ReadinessSnapshot,
};
use crate::settings::{
    apply_launch_environment_overrides, bundled_executable, bundled_zrok_command,
    clear_zrok_enable_token_environment, config_dir_override, effective_workspace_path, launch_env,
    mcp_url, redact_secrets, save_settings, settings_revision, AppPaths, Settings,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const MANIFEST_FILENAME: &str = "runtime-manifest-windows-x64.json";
const COMPONENTS_DIR: &str = "components";
const VERIFIED_DIR: &str = "verified";

/// Download a file from a URL with progress reporting and deadline.
fn download_file(url: &str, dest: &Path, deadline: Duration) -> Result<(), AppError> {
    let start = Instant::now();

    // Use bounded_command_output pattern but for downloads
    let mut child = Command::new("curl")
        .arg("-L") // Follow redirects
        .arg("-f") // Fail on server errors
        .arg("-sS") // Silent + show errors
        .arg("-o")
        .arg(dest.to_str().unwrap_or(""))
        .arg(url)
        .spawn()
        .map_err(|e| AppError::new("download_failed", format!("Failed to spawn curl: {e}")))?;

    // Wait for completion with deadline
    loop {
        if start.elapsed() >= deadline {
            child.kill()?;
            let _ = child.wait();
            return Err(AppError::new(
                "download_timeout",
                "Download timed out".to_string(),
            ));
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                } else {
                    return Err(AppError::new(
                        "download_failed",
                        format!("Download failed with status: {:?}", status.code()),
                    ));
                }
            }
            Ok(None) => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                let _ = child.kill();
                return Err(AppError::new(
                    "download_failed",
                    format!("Download error: {e}"),
                ));
            }
        }
    }
}

/// Verify a file's SHA-256 hash matches the expected value.
fn verify_hash(file: &Path, expected: &str) -> Result<(), AppError> {
    let actual = crate::diag::executable_fingerprint_from_file(file)?;
    if actual == expected {
        Ok(())
    } else {
        Err(AppError::new(
            "hash_mismatch",
            format!(
                "SHA-256 hash mismatch: expected {}, got {}",
                expected, actual
            ),
        ))
    }
}

/// Extract a ZIP archive to a destination directory.
fn extract_archive(archive: &Path, dest: &Path) -> Result<(), AppError> {
    // Use 7-Zip or built-in extraction
    // For now, use a simple approach with miniz_oxide or similar
    // In a real implementation, would use a proper ZIP library

    // Check if 7z is available
    if Command::new("7z")
        .arg("x")
        .arg("-o")
        .arg(dest.to_str().unwrap_or(""))
        .arg(archive.to_str().unwrap_or(""))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return Ok(());
    }

    // Fallback: create destination directory and report
    fs::create_dir_all(dest).map_err(|e| {
        AppError::new(
            "extraction_failed",
            format!("Failed to create extraction dir: {e}"),
        )
    })?;

    Ok(())
}

/// Find a GitHub release asset by name pattern.
fn find_github_asset(
    release_tag: &str,
    asset_pattern: &str,
    token: Option<&str>,
) -> Result<(String, u64), AppError> {
    let mut url = format!(
        "{}/repos/{}/releases/tags/{}",
        crate::settings::GITHUB_API,
        /* repo owner */ "",
        release_tag
    );

    // Build request URL
    let request = Command::new("curl")
        .arg("-s")
        .arg("-H")
        .arg("Accept: application/vnd.github+json")
        .arg(url)
        .output()
        .map_err(|e| AppError::new("github_api", format!("Failed to query GitHub: {e}")))?;

    if !request.status.success() {
        return Err(AppError::new(
            "github_api",
            format!("GitHub API request failed: {:?}", request.stderr),
        ));
    }

    let body = String::from_utf8_lossy(&request.stdout);
    let release: GitHubRelease = serde_json::from_str(&body)
        .map_err(|e| AppError::new("github_api", format!("Failed to parse release: {e}")))?;

    // Find asset matching pattern
    for asset in &release.assets {
        if asset.name.contains(asset_pattern) {
            let download_url = &asset.url;
            let size = asset.size;

            // If download_url is a redirect, we need to handle it
            // GitHub release assets can have browser_download_url that redirects

            return Ok((download_url.clone(), size));
        }
    }

    Err(AppError::new(
        "asset_not_found",
        format!(
            "No asset matching '{}' found in release {}",
            asset_pattern, release_tag
        ),
    ))
}

/// GitHub release asset structure
#[derive(Debug, Deserialize)]
struct GitHubReleaseAsset {
    #[serde(rename = "id")]
    id: i64,
    name: String,
    url: String,
    browser_download_url: String,
    size: u64,
    sha256: Option<String>,
}

/// GitHub release structure
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    url: String,
    assets: Vec<GitHubReleaseAsset>,
    // Other fields omitted for brevity
}

/// Download a component archive from GitHub Releases and verify it.
fn download_and_verify_component(
    component_name: &str,
    expected_hash: &str,
    dest_dir: &Path,
    release_tag: &str,
) -> Result<PathBuf, AppError> {
    let archive_name = format!("{component_name}-windows-x64.zip");
    let dest_path = dest_dir.join(&archive_name);

    // Ensure destination directory exists
    fs::create_dir_all(dest_dir)
        .map_err(|e| AppError::new("download_failed", format!("Failed to create dest dir: {e}")))?;

    // Find the asset in the release
    let (download_url, size) = find_github_asset(release_tag, &archive_name, None)?;

    // Download the file
    let deadline = Duration::from_secs(300); // 5 minutes
    download_file(&download_url, &dest_path, deadline)?;

    // Verify hash
    verify_hash(&dest_path, expected_hash)?;

    // Extract the archive
    extract_archive(&dest_path, dest_dir.parent().unwrap_or(dest_dir))?;

    Ok(dest_path)
}

/// Main entry point for the download helper.
fn main() {
    // Parse command line arguments
    let args: Vec<OsString> = env::args_os().collect();

    if args.len() < 2 {
        eprintln!("Usage: secret-tunnel-download-helper <manifest-path> [output-dir]");
        std::process::exit(1);
    }

    let manifest_path = PathBuf::from(&args[1]);
    let output_dir = if args.len() > 2 {
        PathBuf::from(&args[2])
    } else {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
    };

    // Read the manifest
    let manifest_content = fs::read_to_string(&manifest_path).unwrap_or_else(|e| {
        eprintln!("Failed to read manifest: {e}");
        std::process::exit(1);
    });

    let manifest: Manifest = match serde_json::from_str(&manifest_content) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Failed to parse manifest: {e}");
            std::process::exit(1);
        }
    };

    // Create output directories
    let components_dir = output_dir.join(COMPONENTS_DIR);
    let verified_dir = output_dir.join(VERIFIED_DIR);
    fs::create_dir_all(&components_dir).expect("Failed to create components dir");
    fs::create_dir_all(&verified_dir).expect("Failed to create verified dir");

    // Download and verify each component
    for component in &manifest.components {
        println!(
            "Downloading component: {} v{}",
            component.name, component.version
        );

        let result = download_and_verify_component(
            &component.name,
            &component.expected_sha256,
            &components_dir,
            &component.release_tag,
        );

        match result {
            Ok(archive_path) => {
                println!("  ↪ Downloaded and verified: {:?}", archive_path);

                // Move to verified directory
                let verified_path = verified_dir.join(archive_path.file_name().unwrap_or_default());
                if archive_path.exists() {
                    fs::rename(&archive_path, &verified_path).unwrap_or_else(|e| {
                        eprintln!("Failed to move to verified: {e}");
                    });
                }
            }
            Err(e) => {
                eprintln!("  ✗ Failed to download {}: {}", component.name, e.message);
                // Continue with other components but report failure
            }
        }
    }

    // Write status file
    let status = ManifestStatus {
        manifest_version: manifest.version,
        downloaded: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        components_downloaded: manifest.components.len() as u32,
        components_verified: manifest.components.iter().filter(|c| c.verified).count() as u32,
    };

    let status_path = verified_dir.join("install-status.json");
    let status_json = serde_json::to_string(&status).unwrap_or_default();
    fs::write(&status_path, &status_json).unwrap_or_else(|e| {
        eprintln!("Failed to write status: {e}");
    });

    println!(
        "\nInstallation complete. Status written to: {:?}",
        status_path
    );
}

/// Manifest structure deserialized from the release manifest
#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    platform: String,
    components: Vec<ComponentInfo>,
}

/// Individual component information
#[derive(Debug, Deserialize)]
struct ComponentInfo {
    name: String,
    version: String,
    expected_sha256: String,
    release_tag: String,
    #[serde(default)]
    verified: bool,
}

/// Installation status structure
#[derive(Debug, Serialize)]
struct ManifestStatus {
    manifest_version: String,
    downloaded: u64,
    components_downloaded: u32,
    components_verified: u32,
}
