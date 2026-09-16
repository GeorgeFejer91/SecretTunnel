import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const [
  html,
  css,
  mainTs,
  mainLib,
  commandsRs,
  processRs,
  settingsRs,
  tauriConfigRaw,
  flowSvg,
  liveE2e,
  cleanRelease,
  prepareMcp,
  workflow,
] = await Promise.all([
  read("index.html"),
  read("src/styles.css"),
  read("src/main.ts"),
  read("src-tauri/src/lib.rs"),
  read("src-tauri/src/commands.rs"),
  read("src-tauri/src/process.rs"),
  read("src-tauri/src/settings.rs"),
  read("src-tauri/tauri.conf.json"),
  read("public/secret-tunnel-flow.svg"),
  read("scripts/smoke-live-e2e.mjs"),
  read("scripts/clean-release-bundle.mjs"),
  read("scripts/prepare-gpt-repo-mcp.mjs"),
  // The workflow lives at the repository root, one level above app/.
  read("../.github/workflows/build.yml"),
]);

const tauriConfig = JSON.parse(tauriConfigRaw);
const mainWindow = tauriConfig.app?.windows?.[0];
const bundle = tauriConfig.bundle ?? {};

assertIncludes(html, 'src="/secret-tunnel-cloud.svg"', "static cloud art must render immediately");
assertIncludes(html, 'id="hero-flow-art"', "animated flow art element must exist");
assertIncludes(html, 'data-src="/secret-tunnel-flow.svg"', "animated SVG must be lazy-loaded by status");
assertIncludes(html, 'id="zrok-token"', "zrok enable token field must be built into first-run setup");
assertFalse(/\bStart\b/.test(html), "manual Start button must not return to the UI");

assertIncludes(css, "overflow: hidden;", "window content must remain fixed without scroll UI");
assertIncludes(css, "body.needs-folder .folder-button", "folder chooser must prompt when no folder is selected");
assertIncludes(css, "animation: folder-halo", "folder chooser prompt must breathe");
assertIncludes(css, "body.running.flow-ready .hero-art-static", "static art must hide only after running animation is ready");
assertIncludes(css, "body.running.flow-ready .hero-art-flow", "animated art must show only after running animation is ready");
assertIncludes(css, "animation: cloud-breathe", "running art must breathe after successful initialization");

assertMatch(
  mainTs,
  /document\.body\.classList\.toggle\("running",\s*status\.running\)/,
  "frontend must derive running class from native status",
);
assertMatch(
  mainTs,
  /renderFlowArt\(status\.running\)/,
  "frontend must pass native running state into animation loader",
);
assertMatch(
  mainTs,
  /function renderFlowArt\(running: boolean\)[\s\S]*if \(running\)[\s\S]*elements\.flowArt\.src = src[\s\S]*elements\.flowArt\.removeAttribute\("src"\)/,
  "animated SVG must only load while running and unload while stopped",
);

// get_status is an observation path: webview polling must never drive service
// startup. Retries belong to the backend supervisor, which honours an explicit
// stop; a poll-driven start ignores it and restarts what the user just stopped.
assertNoMatch(
  commandsRs,
  /pub fn get_status[\s\S]*?request_autostart_if_configured/,
  "status polling must not start services",
);
assertMatch(
  processRs,
  /pub fn start_supervisor\(&self\)/,
  "the backend must own startup retries",
);
assertMatch(
  commandsRs,
  /pub fn choose_workspace_folder[\s\S]*sync_services_after_settings_change\(&state\)\?/,
  "folder picker must start or restart services after selection",
);
assertMatch(
  commandsRs,
  /pub fn set_workspace_path[\s\S]*sync_services_after_settings_change\(&state\)\?/,
  "typed folder path save must start or restart services",
);
assertMatch(
  commandsRs,
  /pub fn enable_zrok[\s\S]*state\.start_if_configured\(\)\?/,
  "zrok enable must start services after successful setup",
);

// Health colouring. The green state must be earned by a verified public MCP
// handshake, not by a spawned process: "running" has repeatedly been true while
// the endpoint answered 502 or 503, and a green light that can lie is worse
// than no light at all.
assertIncludes(html, 'id="public-state"', "UI must report whether ChatGPT can reach the app");
assertIncludes(html, 'id="diag-reason"', "UI must have a place to explain a red state");
assertIncludes(html, 'id="open-instructions"', "UI must offer setup instructions");
assertIncludes(html, 'id="open-about"', "UI must explain what the app does");
assertIncludes(html, '<dialog id="instructions"', "instructions must open as a dialog");
assertIncludes(html, '<dialog id="about"', "about must open as a dialog");
assertIncludes(css, ".state-ok", "UI must define a healthy state colour");
assertIncludes(css, ".state-warn", "UI must define an in-progress state colour");
assertIncludes(css, ".state-bad", "UI must define a blocked state colour");
assertMatch(
  mainTs,
  /publicProtocol\.status === "verified"[\s\S]*level: "ok"/,
  "reachability may only go green on a verified public MCP handshake",
);
assertMatch(
  mainTs,
  /cleanupBlockedReason[\s\S]*level: "bad"/,
  "a cleanup block must surface as a red state instead of being silent",
);

assertMatch(
  processRs,
  /fn status_probe_path_from_environment[\s\S]*SECRET_TUNNEL_STATUS_FILE/,
  "native runtime must support an opt-in status file for live E2E verification",
);
assertMatch(
  processRs,
  /fn write_status_document[\s\S]*"running"[\s\S]*"workspaceConfigured"[\s\S]*"failureCode"/,
  "status document must write sanitized running-state and failure fields",
);
assertMatch(
  processRs,
  /pub fn enable_zrok_from_environment_if_present[\s\S]*zrok_enable_token_from_environment\(\)[\s\S]*enable_zrok_environment\(&token\)/,
  "launch environment token must be able to enable zrok without storing it in settings",
);
assertMatch(
  processRs,
  /fn zrok_enable_token_from_environment\(\)[\s\S]*SECRET_TUNNEL_ZROK_ENABLE_TOKEN[\s\S]*ZROK_ENABLE_TOKEN/,
  "zrok launch token helper must support managed setup environment variables",
);
assertIncludes(processRs, 'bundled_executable("node")', "MCP must use bundled Node");
assertIncludes(processRs, "Command::new(bundled_zrok_command()?)", "zrok must use bundled zrok2");
assertIncludes(processRs, 'command.env("GPT_REPO_READ_ONLY_SURFACE", "1")', "read mode must expose read-only MCP tools only");
assertIncludes(processRs, "CREATE_NO_WINDOW", "Windows child processes must remain backgrounded");

assertMatch(
  mainLib,
  /apply_launch_environment_overrides\(&paths\)/,
  "app launch must apply managed environment settings before startup",
);
assertMatch(
  settingsRs,
  /pub fn apply_launch_environment_overrides[\s\S]*SECRET_TUNNEL_WORKSPACE_PATH[\s\S]*SECRET_TUNNEL_ACCESS_MODE[\s\S]*SECRET_TUNNEL_ZROK_NAME[\s\S]*SECRET_TUNNEL_PUBLIC_PATH_TOKEN/,
  "settings must support explicit launch environment overrides",
);
assertMatch(
  liveE2e,
  /verifyDisabledZrokPackagedLaunch[\s\S]*zrok_not_enabled/,
  "live E2E smoke must verify disabled-zrok packaged launch",
);
assertMatch(
  liveE2e,
  /waitForAppRunningStatus\(\)[\s\S]*waitForMcpToolSurface\(mcpUrl\)/,
  "live E2E smoke must verify app running state before the public MCP tool surface",
);
assertMatch(
  cleanRelease,
  /"bundle"[\s\S]*"binaries"[\s\S]*"resources"/,
  "release cleanup must remove stale bundle and runtime resource directories",
);
assertMatch(
  prepareMcp,
  /SECRET_TUNNEL_REQUIRE_CLEAN_MCP_SOURCE[\s\S]*SECRET_TUNNEL_REQUIRE_PINNED_MCP_SOURCE[\s\S]*GPT_REPO_MCP_REPO_REF/,
  "MCP runtime prep must support strict clean and pinned source release gates",
);
assertMatch(
  workflow,
  /SECRET_TUNNEL_REQUIRE_CLEAN_MCP_SOURCE/,
  "release workflow must require clean MCP source during runtime prep",
);
assertMatch(
  workflow,
  /run_live_e2e[\s\S]*Live zrok E2E[\s\S]*github\.event_name == 'workflow_dispatch'[\s\S]*windows-latest[\s\S]*SECRET_TUNNEL_ZROK_ENABLE_TOKEN[\s\S]*npm run smoke:live/,
  "release workflow must require clean MCP source and keep live zrok E2E manual, Windows-scoped, and secret-backed",
);
assertMatch(
  mainLib,
  /if launched_in_background\(\)[\s\S]*window\.minimize\(\)/,
  "autostart background launches must minimize the app window",
);
assertMatch(
  mainLib,
  /enable_zrok_from_environment_if_present\(\)[\s\S]*start_if_configured\(\)/,
  "launch setup must try zrok auto-enable before starting configured services",
);

assertEqual(mainWindow.width, 580, "window width must stay fixed");
assertEqual(mainWindow.height, 648, "window height must stay fixed");
assertEqual(mainWindow.resizable, false, "window must not be resizable");
assertEqual(mainWindow.maximizable, false, "window must not be maximizable");
assertArrayIncludes(bundle.resources, "binaries/", "installer must carry complete runtime binaries directory");
assertArrayIncludes(bundle.resources, "resources/", "installer must carry complete bundled MCP resources directory");
assertEqual(
  bundle.windows?.webviewInstallMode?.type,
  "offlineInstaller",
  "Windows installer must include offline WebView2 runtime support",
);
assertEqual(
  bundle.windows?.webviewInstallMode?.silent,
  true,
  "Windows WebView2 setup must be silent",
);

assertIncludes(flowSvg, 'id="flow-animation"', "flow SVG must include named animation");
assertIncludes(flowSvg, "stroke-dashoffset", "flow SVG must animate dash movement");
assertMatch(flowSvg, /dur="7\.2s"/, "flow SVG must use the faster running-state timing");

console.log("UI/runtime contract smoke check passed.");

function read(relativePath) {
  return readFile(resolve(rootDir, relativePath), "utf8");
}

function assertIncludes(text, needle, message) {
  if (!text.includes(needle)) {
    throw new Error(`${message}: missing ${JSON.stringify(needle)}`);
  }
}

function assertMatch(text, pattern, message) {
  if (!pattern.test(text)) {
    throw new Error(`${message}: ${pattern}`);
  }
}

function assertNoMatch(text, pattern, message) {
  if (pattern.test(text)) {
    throw new Error(`${message}: ${pattern}`);
  }
}

function assertEqual(actual, expected, message) {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

function assertArrayIncludes(value, expected, message) {
  if (!Array.isArray(value) || !value.includes(expected)) {
    throw new Error(`${message}: ${JSON.stringify(value)}`);
  }
}

function assertFalse(condition, message) {
  if (condition) {
    throw new Error(message);
  }
}
