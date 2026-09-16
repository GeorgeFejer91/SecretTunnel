import { execSync } from "child_process";
import { createHash, createReadStream, stat } from "crypto";
import { mkdirSync, writeFileSync, readFileSync, readdirSync, rmSync } from "fs";
import { join, resolve, relative } from "path";
import { tmpdir } from "os";
import { prompt } from "inquirer";
import http from "http";
import https from "https";

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const binaryDir = join(rootDir, "src-tauri", "binaries");
const outputDir = join(rootDir, "outputs", "release");
const checksumsPath = join(rootDir, "outputs", "checksums");

const APP_VERSION = "0.1.0";
const PLATFORM = process.platform;
const ARCH = process.arch;

// Archive naming conventions
const ARCHIVE_TEMPLATES = {
  node: (version) => `node-windows-x64-${version}.zip`,
  zrok: (version) => `zrok-windows-x64-${version}.zip`,
  mcp: (commit) => `mcp-runtime-${commit}-windows-x64.zip`,
};

// GitHub Releases API base
const GITHUB_API = "https://api.github.com";
const REPO_OWNER = "GeorgeFejer91";
const REPO_NAME = "SecretTunnel";

// Known component versions (these would be updated per release)
const KNOWN_COMPONENTS = {
  node: process.version,
  zrok: "2.0.4",
};

// Get current git commit
function getGitCommit() {
  try {
    return execSync("git rev-parse HEAD", { cwd: rootDir, encoding: "utf8" }).trim();
  } catch {
    return "unknown";
  }
}

// Get current dirty state
function getGitDirty() {
  try {
    const status = execSync("git status --porcelain", { cwd: rootDir, encoding: "utf8" });
    return status.trim().length > 0 ? "dirty" : "clean";
  } catch {
    return "unknown";
  }
}

// Compute SHA-256 of a file
function sha256(filePath) {
  return createHash("sha256")
    .update(readFileSync(filePath))
    .digest("hex");
}

// Compute SHA-256 of a stream
function sha256Stream(stream) {
  return createHash("sha256")
    .update(stream)
    .digest("hex");
}

// Download a file from URL with progress
function downloadFile(url, destPath) {
  return new Promise((resolve, reject) => {
    const file = createWriteStream(destPath);
    const request = https ? https.get(url) : http.get(url);

    request.on("error", (err) => {
      rmSync(destPath, { force: true });
      reject(err);
    });

    request.on("response", (response) => {
      response.pipe(file);
      file.on("finish", () => {
        file.close(() => {
          resolve(destPath);
        });
      });
    });

    file.on("error", (err) => {
      rmSync(destPath, { force: true });
      reject(err);
    });
  });
}

// Get file size
function getFileSize(filePath) {
  try {
    return stat(filePath).size;
  } catch {
    return 0;
  }
}

// Upload to GitHub Releases (asset upload)
// This is a simplified version - in production would use gh release create or API
function uploadAsset(localPath, remoteName, token) {
  // Placeholder - would need proper GitHub API upload
  console.log(`Would upload ${localPath} as ${remoteName}`);
  return Promise.resolve(true);
}

// Main packaging function
async function packageRelease() {
  console.log(`=== Secret Tunnel Release Packaging v${APP_VERSION} ===`);
  console.log(`Platform: ${PLATFORM}/${ARCH}`);
  console.log(`Git commit: ${getGitCommit()}`);
  console.log(`Git state: ${getGitDirty()}`);

  // Create output directories
  mkdirSync(outputDir, { recursive: true });
  mkdirSync(checksumsPath, { recursive: true });

  // 1. Prepare Node archive
  const nodeVersion = KNOWN_COMPONENTS.node;
  const nodeArchiveName = ARCHIVE_TEMPLATES.node(nodeVersion);
  const nodeArchivePath = join(binaryDir, nodeArchiveName);
  console.log(`\n=== Preparing Node ${nodeVersion} archive ===`);

  // Check if Node binary exists and create archive
  const nodeExe = join(binaryDir, "node.exe");
  if (existsSync(nodeExe)) {
    // Create a simple zip with just the node binary
    // In production, would include node_modules and relevant files
    try {
      execSync(
        `powershell -Command "Compress-Archive -Path '${nodeExe}' -DestinationPath '${nodeArchivePath}'"`,
        { cwd: rootDir }
      );
      console.log(`Created Node archive: ${nodeArchivePath}`);
      console.log(`Node archive size: ${getFileSize(nodeArchivePath)} bytes`);
    } catch (err) {
      console.error(`Failed to create Node archive: ${err.message}`);
    }
  } else {
    console.log("Node binary not found - would download from GitHub Releases");
  }

  // 2. Prepare zrok archive
  const zrokVersion = KNOWN_COMPONENTS.zrok;
  const zrokArchiveName = ARCHIVE_TEMPLATES.zrok(zrokVersion);
  const zrokArchivePath = join(binaryDir, zrokArchiveName);
  console.log(`\n=== Preparing zrok ${zrokVersion} archive ===`);

  const zrokExe = join(binaryDir, "zrok2.exe");
  if (existsSync(zrokExe)) {
    try {
      execSync(
        `powershell -Command "Compress-Archive -Path '${zrokExe}' -DestinationPath '${zrokArchivePath}'"`,
        { cwd: rootDir }
      );
      console.log(`Created zrok archive: ${zrokArchivePath}`);
      console.log(`zrok archive size: ${getFileSize(zrokArchivePath)} bytes`);
    } catch (err) {
      console.error(`Failed to create zrok archive: ${err.message}`);
    }
  } else {
    console.log("zrok2.exe not found - would download from GitHub Releases");
  }

  // 3. Prepare MCP runtime archive
  const gitCommit = getGitCommit();
  const mcpArchiveName = ARCHIVE_TEMPLATES.mcp(gitCommit);
  const mcpArchivePath = join(binaryDir, mcpArchiveName);
  console.log(`\n=== Preparing MCP runtime archive (commit: ${gitCommit}) ===`);

  const mcpDir = join(rootDir, "src-tauri", "resources", "gpt-repo-mcp");
  if (existsSync(mcpDir) && existsSync(join(mcpDir, "dist", "server.js"))) {
    try {
      // Create zip of the MCP runtime directory
      execSync(
        `powershell -Command "Compress-Archive -Path '${join(mcpDir, '*')}' -DestinationPath '${mcpArchivePath}'"`,
        { cwd: rootDir }
      );
      console.log(`Created MCP runtime archive: ${mcpArchivePath}`);
      console.log(`MCP runtime archive size: ${getFileSize(mcpArchivePath)} bytes`);
    } catch (err) {
      console.error(`Failed to create MCP runtime archive: ${err.message}`);
    }
  } else {
    console.log("MCP runtime not found - would prepare from source");
  }

  // 4. Generate release manifest
  console.log(`\n=== Generating release manifest ===`);

  const manifest = {
    version: APP_VERSION,
    platform: `${PLATFORM}-${ARCH}`,
    gitCommit,
    gitDirty: getGitDirty(),
    timestamp: new Date().toISOString(),
    components: {
      node: {
        version: nodeVersion,
        archive: nodeArchiveName,
        path: nodeArchivePath,
        compressedSize: existsSync(nodeArchivePath) ? getFileSize(nodeArchivePath) : 0,
        unpackedSize: existsSync(nodeArchivePath) ? getFileSize(nodeArchivePath) : 0, // would be larger after extraction
        sha256: existsSync(nodeArchivePath) ? sha256(nodeArchivePath) : "pending",
      },
      zrok: {
        version: zrokVersion,
        archive: zrokArchiveName,
        path: zrokArchivePath,
        compressedSize: existsSync(zrokArchivePath) ? getFileSize(zrokArchivePath) : 0,
        unpackedSize: existsSync(zrokArchivePath) ? getFileSize(zrokArchivePath) : 0,
        sha256: existsSync(zrokArchivePath) ? sha256(zrokArchivePath) : "pending",
      },
      mcp: {
        commit: gitCommit,
        archive: mcpArchiveName,
        path: mcpArchivePath,
        compressedSize: existsSync(mcpArchivePath) ? getFileSize(mcpArchivePath) : 0,
        unpackedSize: existsSync(mcpArchivePath) ? getFileSize(mcpArchivePath) : 0,
        sha256: existsSync(mcpArchivePath) ? sha256(mcpArchivePath) : "pending",
      },
    },
    downloadInfo: {
      // These would be populated after upload to GitHub Releases
      releaseUrl: "",
      assets: {},
    },
  };

  // Write manifest
  const manifestPath = join(outputDir, "runtime-manifest-windows-x64.json");
  writeFileSync(manifestPath, JSON.stringify(manifest, null, 2));
  console.log(`Generated manifest: ${manifestPath}`);

  // 5. Generate signature file (placeholder - would use actual signing key)
  console.log(`\n=== Manifest generation complete ===`);
  console.log(`\nNext steps:`);
  console.log(`1. Upload archives to GitHub Releases as assets`);
  console.log(`2. Sign the manifest with release signing key`);
  console.log(`3. Update download URLs in the manifest`);
  console.log(`4. Build NSIS online installer referencing this manifest`);
  console.log(`5. Build offline installer including all components`);

  // Write checksums
  const checksums = {
    node: existsSync(nodeArchivePath) ? sha256(nodeArchivePath) : "pending",
    zrok: existsSync(zrokArchivePath) ? sha256(zrokArchivePath) : "pending",
    mcp: existsSync(mcpArchivePath) ? sha256(mcpArchivePath) : "pending",
  };

  const checksumsFile = join(checksumsPath, "release-checksums.json");
  writeFileSync(checksumsFile, JSON.stringify(checksums, null, 2));
  console.log(`Wrote checksums: ${checksumsFile}`);
}

// Run the packaging
packageRelease().catch((err) => {
  console.error("Packaging failed:", err);
  process.exit(1);
});