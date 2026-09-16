import { spawn, spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { fileURLToPath } from "node:url";
import { findRuntimeLayout } from "./runtime-layout.mjs";

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const releaseRoot = process.argv[2] ?? "src-tauri/target/release";
const layout = findRuntimeLayout(rootDir, releaseRoot);
const appPath = process.env.SECRET_TUNNEL_APP_EXE
  ? resolve(process.env.SECRET_TUNNEL_APP_EXE)
  : findReleaseExecutable(resolve(rootDir, releaseRoot));

if (!appPath || !existsSync(appPath)) {
  throw new Error(`Secret Tunnel release executable not found. Build the release app first. Looked under ${releaseRoot}.`);
}

const enableToken = process.env.SECRET_TUNNEL_ZROK_ENABLE_TOKEN ?? process.env.ZROK_ENABLE_TOKEN;
if (!zrokEnvironmentEnabled() && !enableToken) {
  const message = "Live E2E requires zrok to be enabled, or SECRET_TUNNEL_ZROK_ENABLE_TOKEN/ZROK_ENABLE_TOKEN to be set.";
  if (process.env.SECRET_TUNNEL_LIVE_E2E_ALLOW_SKIP === "1") {
    if (process.env.SECRET_TUNNEL_LIVE_E2E_VERIFY_DISABLED === "1") {
      await verifyDisabledZrokPackagedLaunch();
      console.log("Packaged app reported expected disabled-zrok start-failed status.");
    }
    console.log(`${message} Skipping because SECRET_TUNNEL_LIVE_E2E_ALLOW_SKIP=1.`);
    process.exit(0);
  }
  throw new Error(message);
}

const tempRoot = await mkdtemp(join(tmpdir(), "secret-tunnel-live-e2e-"));
const workspace = join(tempRoot, "workspace");
const profileRoot = join(tempRoot, "profile");
const zrokName = `st${randomUUID().replaceAll("-", "").slice(0, 20)}`;
const publicPathToken = randomUUID().replaceAll("-", "");
const mcpUrl = `https://${zrokName}.share.zrok.io/t/${publicPathToken}/mcp`;
const stdoutPath = join(tempRoot, "secret-tunnel.stdout.log");
const stderrPath = join(tempRoot, "secret-tunnel.stderr.log");
const statusPath = join(tempRoot, "secret-tunnel.status.json");

let child = null;
let stdout = "";
let stderr = "";

try {
  await mkdir(workspace, { recursive: true });
  await writeFile(join(workspace, "README.md"), "# Secret Tunnel live E2E\n");

  const env = {
    ...process.env,
    SECRET_TUNNEL_WORKSPACE_PATH: workspace,
    SECRET_TUNNEL_ACCESS_MODE: "read",
    SECRET_TUNNEL_ZROK_NAME: zrokName,
    SECRET_TUNNEL_PUBLIC_PATH_TOKEN: publicPathToken,
    SECRET_TUNNEL_STATUS_FILE: statusPath,
  };

  if (enableToken) {
    env.SECRET_TUNNEL_ZROK_ENABLE_TOKEN = enableToken;
  }

  applyIsolatedProfileEnv(env, profileRoot);

  child = spawn(appPath, ["--background"], {
    cwd: dirname(appPath),
    env,
    detached: process.platform !== "win32",
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });

  child.stdout.on("data", (chunk) => {
    stdout += chunk.toString();
  });
  child.stderr.on("data", (chunk) => {
    stderr += chunk.toString();
  });

  await waitForLocalHealth(() => childExited(child, stdout, stderr));
  await waitForAppRunningStatus();
  await waitForZrokEnabled();
  await waitForMcpToolSurface(mcpUrl);

  console.log(`Live E2E passed. MCP URL: ${mcpUrl}`);
} catch (error) {
  await writeFile(stdoutPath, stdout).catch(() => {});
  await writeFile(stderrPath, stderr).catch(() => {});
  throw error;
} finally {
  if (child) {
    stopProcessTree(child);
  }
  if (process.env.SECRET_TUNNEL_LIVE_E2E_KEEP_TEMP !== "1") {
    await rm(tempRoot, { recursive: true, force: true });
  } else {
    console.log(`Kept live E2E temp folder: ${tempRoot}`);
  }
}

function findReleaseExecutable(runtimeRoot) {
  const envPath = process.env.SECRET_TUNNEL_APP_EXE;
  if (envPath) return envPath;

  const candidates = process.platform === "win32"
    ? [join(runtimeRoot, "secret-tunnel.exe")]
    : process.platform === "darwin"
      ? [
          join(runtimeRoot, "secret-tunnel"),
          join(runtimeRoot, "bundle", "macos", "Secret Tunnel.app", "Contents", "MacOS", "Secret Tunnel"),
          join(runtimeRoot, "bundle", "macos", "Secret Tunnel.app", "Contents", "MacOS", "secret-tunnel"),
        ]
      : [join(runtimeRoot, "secret-tunnel")];

  return candidates.find((candidate) => existsSync(candidate)) ?? null;
}

function applyIsolatedProfileEnv(env, profileRoot) {
  if (process.platform === "win32") {
    env.APPDATA = join(profileRoot, "AppData", "Roaming");
    env.LOCALAPPDATA = join(profileRoot, "AppData", "Local");
    return;
  }
  env.HOME = join(profileRoot, "home");
}

async function verifyDisabledZrokPackagedLaunch() {
  const disabledTempRoot = await mkdtemp(join(tmpdir(), "secret-tunnel-disabled-zrok-"));
  const disabledWorkspace = join(disabledTempRoot, "workspace");
  const disabledStatusPath = join(disabledTempRoot, "secret-tunnel.status.json");
  let disabledChild = null;

  try {
    await mkdir(disabledWorkspace, { recursive: true });
    await writeFile(join(disabledWorkspace, "README.md"), "# Disabled zrok smoke\n");

    const env = {
      ...process.env,
      SECRET_TUNNEL_WORKSPACE_PATH: disabledWorkspace,
      SECRET_TUNNEL_ACCESS_MODE: "read",
      SECRET_TUNNEL_ZROK_NAME: `st${randomUUID().replaceAll("-", "").slice(0, 20)}`,
      SECRET_TUNNEL_PUBLIC_PATH_TOKEN: randomUUID().replaceAll("-", ""),
      SECRET_TUNNEL_STATUS_FILE: disabledStatusPath,
    };
    delete env.SECRET_TUNNEL_ZROK_ENABLE_TOKEN;
    delete env.ZROK_ENABLE_TOKEN;
    applyIsolatedProfileEnv(env, join(disabledTempRoot, "profile"));

    disabledChild = spawn(appPath, ["--background"], {
      cwd: dirname(appPath),
      env,
      detached: process.platform !== "win32",
      stdio: ["ignore", "ignore", "ignore"],
      windowsHide: true,
    });

    await waitForStatusFile(
      disabledStatusPath,
      (status) => status?.event === "start-failed"
        && status?.running === false
        && status?.workspaceConfigured === true
        && status?.failureCode === "zrok_not_enabled",
      "disabled-zrok start-failed status",
    );
  } finally {
    if (disabledChild) {
      stopProcessTree(disabledChild);
    }
    if (process.env.SECRET_TUNNEL_LIVE_E2E_KEEP_TEMP !== "1") {
      await rm(disabledTempRoot, { recursive: true, force: true });
    } else {
      console.log(`Kept disabled-zrok temp folder: ${disabledTempRoot}`);
    }
  }
}

function zrokEnvironmentEnabled() {
  const result = spawnSync(layout.zrokPath, ["status"], { encoding: "utf8", windowsHide: true });
  if (result.status !== 0) return false;
  const output = `${result.stdout ?? ""}${result.stderr ?? ""}`.toLowerCase();
  return output.trim() !== "" && !output.includes("zrok2 enable") && !output.includes("not enabled");
}

async function waitForZrokEnabled() {
  const deadline = Date.now() + 45_000;
  while (Date.now() < deadline) {
    if (zrokEnvironmentEnabled()) return;
    await sleep(1000);
  }
  throw new Error("zrok did not become enabled during live E2E.");
}

async function waitForLocalHealth(exitCheck) {
  const deadline = Date.now() + 45_000;
  let lastError = null;
  while (Date.now() < deadline) {
    exitCheck();
    try {
      const response = await fetch("http://127.0.0.1:8787/health");
      const body = await response.json();
      if (response.ok && body?.ok === true && body?.name === "gpt-repo-mcp") return;
      lastError = new Error(`Unexpected local health response: ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    await sleep(500);
  }
  throw new Error(`Local MCP health did not become ready: ${lastError?.message ?? "unknown error"}`);
}

async function waitForAppRunningStatus() {
  await waitForStatusFile(
    statusPath,
    (status) => status?.running === true
      && status?.starting === false
      && status?.workspaceConfigured === true
      && status?.zrokEnabled === true
      && status?.mcpRuntimeFound === true,
    "running status",
  );
}

async function waitForStatusFile(path, predicate, label) {
  const deadline = Date.now() + 45_000;
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      const status = JSON.parse(await readFile(path, "utf8"));
      if (predicate(status)) return;
      lastError = new Error(`${label} not reached: ${JSON.stringify(status)}`);
    } catch (error) {
      lastError = error;
    }
    await sleep(500);
  }
  throw new Error(`Secret Tunnel app did not report ${label}: ${lastError?.message ?? "unknown error"}`);
}

async function waitForMcpToolSurface(url) {
  const deadline = Date.now() + 90_000;
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      await verifyReadOnlyToolSurface(url);
      return;
    } catch (error) {
      lastError = error;
    }
    await sleep(1500);
  }
  throw new Error(`Public zrok MCP did not become ready: ${lastError?.message ?? "unknown error"}`);
}

async function verifyReadOnlyToolSurface(url) {
  const [{ Client }, { StreamableHTTPClientTransport }] = await Promise.all([
    import(pathToFileURL(join(layout.runtimeDir, "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js"))),
    import(pathToFileURL(join(layout.runtimeDir, "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "streamableHttp.js"))),
  ]);
  const client = new Client({ name: "secret-tunnel-live-e2e", version: "0.1.0" });
  const transport = new StreamableHTTPClientTransport(new URL(url));

  try {
    await client.connect(transport);
    const listed = await client.listTools();
    const tools = listed.tools ?? [];
    if (tools.length === 0) {
      throw new Error("Read-only tools/list returned no tools.");
    }
    const writeCapable = tools.filter((tool) => tool.annotations?.readOnlyHint !== true);
    if (writeCapable.length > 0) {
      throw new Error(`Read-only tools/list exposed write-capable tools: ${writeCapable.map((tool) => tool.name).join(", ")}`);
    }
  } finally {
    await client.close();
  }
}

function childExited(process, stdout, stderr) {
  if (process.exitCode !== null || process.signalCode !== null) {
    throw new Error(
      [
        `Secret Tunnel exited early. code=${process.exitCode} signal=${process.signalCode}`,
        `stdout:\n${stdout}`,
        `stderr:\n${stderr}`,
      ].join("\n"),
    );
  }
}

function stopProcessTree(childProcess) {
  if (childProcess.exitCode !== null || childProcess.signalCode !== null) return;
  if (process.platform === "win32") {
    spawnSync("taskkill", ["/PID", String(childProcess.pid), "/T", "/F"], {
      stdio: "ignore",
      windowsHide: true,
    });
    return;
  }
  try {
    process.kill(-childProcess.pid, "SIGTERM");
  } catch {
    childProcess.kill("SIGTERM");
  }
}

function sleep(ms) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, ms));
}
