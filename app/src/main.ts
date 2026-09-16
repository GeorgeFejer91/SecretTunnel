import { invoke } from "@tauri-apps/api/core";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";

type AppError = {
  code: string;
  message: string;
};

type LogLine = {
  source: string;
  line: string;
};

type ProbeStatus = "pending" | "verifying" | "verified" | "failed" | "unsupported";

type ProbeEntry = {
  kind: string;
  status: ProbeStatus;
  checkedAt: string | null;
  detail: string | null;
};

/// The backend's readiness gates for the active generation. `publicProtocol` is
/// the only one that means "ChatGPT could actually talk to this": a spawned
/// process and a registered share both prove nothing on their own.
type Readiness = {
  localProcess: ProbeEntry;
  localEndpoint: ProbeEntry;
  localProtocol: ProbeEntry;
  publicTunnel: ProbeEntry;
  publicProtocol: ProbeEntry;
  generation: number;
  verifiedRevision: number | null;
  degraded: boolean;
  issues: string[];
};

type Status = {
  settings: {
    workspacePath: string | null;
    accessMode: "read" | "read_write";
    zrokName: string;
    publicPathToken: string;
    gptRepoMcpPath: string;
    managedConfigPath: string;
  };
  mcpUrl: string;
  running: boolean;
  autostartEnabled: boolean;
  zrokInstalled: boolean;
  zrokEnabled: boolean;
  gptRepoMcpFound: boolean;
  workspaceConfigured: boolean;
  startupBlockedReason: string | null;
  startupBlockedMessage: string | null;
  lifecycleState: string;
  desiredRunning: boolean;
  cleanupBlockedReason: string | null;
  readiness: Readiness | null;
  logs: LogLine[];
};

type Health = "ok" | "warn" | "bad";

type Indicator = {
  level: Health;
  text: string;
  /// One actionable sentence, shown only when something is wrong.
  reason?: string;
};

const ZROK_TOKEN_PAGE_URL = "https://myzrok.io";

const elements = {
  statusLine: mustElement<HTMLElement>("status-line"),
  flowArt: mustElement<HTMLImageElement>("hero-flow-art"),
  mcpUrl: mustElement<HTMLInputElement>("mcp-url"),
  copyUrl: mustElement<HTMLButtonElement>("copy-url"),
  regenerateUrl: mustElement<HTMLButtonElement>("regenerate-url"),
  zrokSetup: mustElement<HTMLElement>("zrok-setup"),
  zrokToken: mustElement<HTMLInputElement>("zrok-token"),
  enableZrok: mustElement<HTMLButtonElement>("enable-zrok"),
  getTokenLink: mustElement<HTMLAnchorElement>("get-token-link"),
  folderPicker: mustElement<HTMLButtonElement>("folder-picker"),
  folderPath: mustElement<HTMLInputElement>("folder-path"),
  pasteFolder: mustElement<HTMLButtonElement>("paste-folder"),
  copyFolder: mustElement<HTMLButtonElement>("copy-folder"),
  saveFolder: mustElement<HTMLButtonElement>("save-folder"),
  autostart: mustElement<HTMLInputElement>("autostart"),
  modeRead: mustElement<HTMLButtonElement>("mode-read"),
  modeWrite: mustElement<HTMLButtonElement>("mode-write"),
  zrokState: mustElement<HTMLElement>("zrok-state"),
  mcpState: mustElement<HTMLElement>("mcp-state"),
  publicState: mustElement<HTMLElement>("public-state"),
  diagReason: mustElement<HTMLParagraphElement>("diag-reason"),
  instructions: mustElement<HTMLDialogElement>("instructions"),
  openInstructions: mustElement<HTMLButtonElement>("open-instructions"),
  closeInstructions: mustElement<HTMLButtonElement>("close-instructions"),
  doneInstructions: mustElement<HTMLButtonElement>("done-instructions"),
  copyUrlFromInstructions: mustElement<HTMLButtonElement>("copy-url-from-instructions"),
  about: mustElement<HTMLDialogElement>("about"),
  openAbout: mustElement<HTMLButtonElement>("open-about"),
  closeAbout: mustElement<HTMLButtonElement>("close-about"),
  doneAbout: mustElement<HTMLButtonElement>("done-about"),
};

let currentStatus: Status | null = null;
let busy = false;

window.addEventListener("DOMContentLoaded", () => {
  wireEvents();
  refresh();
  window.setInterval(refresh, 4000);
});

function wireEvents() {
  elements.flowArt.addEventListener("load", () => {
    document.body.classList.toggle("flow-ready", currentStatus?.running ?? false);
  });
  elements.copyUrl.addEventListener("click", () => copyText(elements.mcpUrl.value));
  elements.regenerateUrl.addEventListener("click", () =>
    runStatusCommand("regenerate_mcp_url"),
  );
  elements.enableZrok.addEventListener("click", enableZrok);
  elements.getTokenLink.addEventListener("click", openZrokTokenPage);
  elements.zrokToken.addEventListener("keydown", (event) => {
    if (event.key === "Enter") enableZrok();
  });
  elements.folderPicker.addEventListener("click", chooseFolder);
  elements.copyFolder.addEventListener("click", () => copyText(elements.folderPath.value));
  elements.pasteFolder.addEventListener("click", pasteFolder);
  elements.saveFolder.addEventListener("click", saveFolder);
  elements.autostart.addEventListener("change", () => {
    runStatusCommand("set_autostart", { enabled: elements.autostart.checked });
  });
  elements.modeRead.addEventListener("click", () => setAccessMode("read"));
  elements.modeWrite.addEventListener("click", () => setAccessMode("read_write"));

  elements.openInstructions.addEventListener("click", () => elements.instructions.showModal());
  elements.closeInstructions.addEventListener("click", () => elements.instructions.close());
  elements.doneInstructions.addEventListener("click", () => elements.instructions.close());
  elements.copyUrlFromInstructions.addEventListener("click", () => copyText(elements.mcpUrl.value));
  elements.openAbout.addEventListener("click", () => elements.about.showModal());
  elements.closeAbout.addEventListener("click", () => elements.about.close());
  elements.doneAbout.addEventListener("click", () => elements.about.close());
  // Clicking the backdrop closes the dialog; clicks inside it must not.
  for (const sheet of [elements.instructions, elements.about]) {
    sheet.addEventListener("click", (event) => {
      if (event.target === sheet) sheet.close();
    });
  }
  // The CSP blocks navigation, so external links are opened by the backend.
  for (const link of document.querySelectorAll<HTMLElement>("[data-open-url]")) {
    link.addEventListener("click", () => {
      const url = link.dataset.openUrl;
      if (url) openExternal(url);
    });
  }
}

async function openExternal(url: string) {
  try {
    await invoke("open_url", { url });
  } catch (error) {
    renderError(error);
  }
}

async function refresh() {
  if (busy) return;
  try {
    render(await invoke<Status>("get_status"));
  } catch (error) {
    renderError(error);
  }
}

async function chooseFolder() {
  await withBusy(async () => {
    const status = await invoke<Status | null>("choose_workspace_folder");
    if (status) render(status);
  });
}

async function pasteFolder() {
  try {
    elements.folderPath.value = await readText();
    elements.folderPath.focus();
  } catch {
    setStatusText("Clipboard read was blocked.");
  }
}

async function saveFolder() {
  await runStatusCommand("set_workspace_path", { path: elements.folderPath.value });
}

async function setAccessMode(mode: Status["settings"]["accessMode"]) {
  if (currentStatus?.settings.accessMode === mode) return;
  await runStatusCommand("set_access_mode", { mode });
}

async function runStatusCommand(command: string, args?: Record<string, unknown>) {
  await withBusy(async () => {
    render(await invoke<Status>(command, args));
  });
}

async function withBusy(work: () => Promise<void>) {
  if (busy) return;
  busy = true;
  setControlsDisabled(true);
  try {
    await work();
  } catch (error) {
    await refresh();
    renderError(error);
  } finally {
    busy = false;
    setControlsDisabled(false);
  }
}

async function copyText(value: string) {
  if (!value) return;
  try {
    await writeText(value);
    setStatusText("Copied.");
  } catch {
    setStatusText("Clipboard write failed.");
  }
}

function render(status: Status) {
  currentStatus = status;
  elements.mcpUrl.value = status.mcpUrl;
  elements.folderPath.value = status.settings.workspacePath ?? "";
  elements.autostart.checked = status.autostartEnabled;
  renderAccessMode(status.settings.accessMode);
  document.body.classList.toggle("running", status.running);
  document.body.classList.toggle("needs-folder", !status.settings.workspacePath);
  renderFlowArt(status.running);
  const zrokNeedsEnable = status.zrokInstalled && !status.zrokEnabled;
  document.body.classList.toggle("zrok-needs-enable", zrokNeedsEnable);
  elements.zrokSetup.hidden = !zrokNeedsEnable;

  applyIndicator(elements.zrokState, zrokIndicator(status));
  applyIndicator(elements.mcpState, mcpIndicator(status));
  const reachable = reachabilityIndicator(status);
  applyIndicator(elements.publicState, reachable);
  const overall = overallIndicator(status, reachable);
  applyIndicator(elements.statusLine, overall);

  // Show the single most useful explanation, preferring the overall verdict.
  const reason = overall.reason ?? reachable.reason ?? null;
  elements.diagReason.textContent = reason ?? "";
  // The line is clamped to two lines, so keep the full text reachable on hover.
  elements.diagReason.title = reason ?? "";
  elements.diagReason.hidden = reason === null;
}

function applyIndicator(element: HTMLElement, indicator: Indicator) {
  element.textContent = indicator.text;
  element.classList.remove("state-ok", "state-warn", "state-bad");
  element.classList.add(`state-${indicator.level}`);
}

function zrokIndicator(status: Status): Indicator {
  if (!status.zrokInstalled) {
    return { level: "bad", text: "Missing", reason: "The bundled zrok2 is missing. Reinstall Secret Tunnel." };
  }
  if (!status.zrokEnabled) {
    return { level: "bad", text: "Needs enable", reason: "Paste your zrok token above to enable the tunnel." };
  }
  return { level: "ok", text: "Ready" };
}

function mcpIndicator(status: Status): Indicator {
  return status.gptRepoMcpFound
    ? { level: "ok", text: "Ready" }
    : { level: "bad", text: "Missing", reason: "The bundled gpt-repo-mcp runtime is missing. Reinstall Secret Tunnel." };
}

/// Whether ChatGPT could actually reach this folder right now. Only a verified
/// handshake through the public URL counts: a running process and a registered
/// share can both be true while the endpoint returns 502 or 503.
function reachabilityIndicator(status: Status): Indicator {
  if (!status.running) return { level: "bad", text: "Not running" };

  const readiness = status.readiness;
  if (!readiness) return { level: "warn", text: "Checking…" };

  const { publicTunnel, publicProtocol } = readiness;
  if (publicProtocol.status === "verified") {
    return { level: "ok", text: "Connected" };
  }
  if (publicProtocol.status === "failed" || publicTunnel.status === "failed") {
    const detail = publicProtocol.detail ?? publicTunnel.detail ?? "the public URL is not responding";
    return { level: "bad", text: "Unreachable", reason: `ChatGPT cannot reach this app: ${detail}` };
  }
  return { level: "warn", text: "Checking…" };
}

function overallIndicator(status: Status, reachable: Indicator): Indicator {
  const mode = status.settings.accessMode === "read_write" ? "read+write" : "read-only";

  // Hard blockers first, most specific and most actionable.
  if (status.cleanupBlockedReason) {
    return {
      level: "bad",
      text: "Needs a restart",
      reason: `Previous services could not be confirmed stopped (${status.cleanupBlockedReason}). Close and reopen Secret Tunnel.`,
    };
  }
  if (!status.settings.workspacePath) {
    return { level: "bad", text: "No folder selected", reason: "Pick the folder you want ChatGPT to read." };
  }
  if (status.startupBlockedMessage) {
    return { level: "bad", text: "Folder unavailable", reason: status.startupBlockedMessage };
  }
  const zrok = zrokIndicator(status);
  if (zrok.level === "bad") return { level: "bad", text: zrok.text, reason: zrok.reason };
  const mcp = mcpIndicator(status);
  if (mcp.level === "bad") return { level: "bad", text: mcp.text, reason: mcp.reason };

  const failedStart = latestStartupIssue(status);
  if (failedStart) return { level: "bad", text: failedStart, reason: failedStart };

  if (!status.running) {
    return status.desiredRunning
      ? { level: "warn", text: "Starting…" }
      : { level: "warn", text: `Stopped, ${mode}` };
  }
  if (reachable.level === "bad") {
    return { level: "bad", text: "Running, not reachable", reason: reachable.reason };
  }
  if (reachable.level === "warn") {
    return { level: "warn", text: `Connecting, ${mode}` };
  }
  return { level: "ok", text: `Connected, ${mode}` };
}

async function openZrokTokenPage() {
  await openExternal(ZROK_TOKEN_PAGE_URL);
}

async function enableZrok() {
  const token = elements.zrokToken.value.trim();
  if (!token) {
    setStatusText("Paste zrok token first.");
    elements.zrokToken.focus();
    return;
  }

  await withBusy(async () => {
    render(await invoke<Status>("enable_zrok", { token }));
    elements.zrokToken.value = "";
  });
}

function renderFlowArt(running: boolean) {
  const src = elements.flowArt.dataset.src;
  if (!src) return;

  if (running) {
    if (!elements.flowArt.getAttribute("src")) {
      document.body.classList.remove("flow-ready");
      elements.flowArt.src = src;
    }
    return;
  }

  elements.flowArt.removeAttribute("src");
  document.body.classList.remove("flow-ready");
}

function renderAccessMode(mode: Status["settings"]["accessMode"]) {
  const writeEnabled = mode === "read_write";
  elements.modeRead.classList.toggle("active", !writeEnabled);
  elements.modeWrite.classList.toggle("active", writeEnabled);
  elements.modeRead.setAttribute("aria-pressed", String(!writeEnabled));
  elements.modeWrite.setAttribute("aria-pressed", String(writeEnabled));
}

/// The most recent launch failure reported by the backend. zrok and runtime
/// problems are diagnosed from status fields instead, so this only covers
/// failures that have no dedicated indicator.
function latestStartupIssue(status: Status): string | null {
  if (status.running) return null;

  const failedStart = status.logs
    .slice()
    .reverse()
    .find((entry) => entry.line.startsWith("Auto-start failed:"));
  if (!failedStart) return null;

  const message = failedStart.line.replace("Auto-start failed:", "").trim();
  if (!message) return "Setup needs attention";
  return message;
}

function renderError(error: unknown) {
  const appError = error as Partial<AppError>;
  setStatusText(appError.message ?? String(error));
}

function setControlsDisabled(disabled: boolean) {
  [
    elements.folderPicker,
    elements.zrokToken,
    elements.enableZrok,
    elements.pasteFolder,
    elements.copyFolder,
    elements.saveFolder,
    elements.copyUrl,
    elements.regenerateUrl,
    elements.autostart,
    elements.modeRead,
    elements.modeWrite,
  ].forEach((control) => {
    control.disabled = disabled;
  });
}

/// Transient messages ("Copied.", clipboard failures) are not health states, so
/// they drop the colouring rather than inheriting the previous verdict's. The
/// next refresh restores the real indicator.
function setStatusText(value: string) {
  elements.statusLine.textContent = value;
  elements.statusLine.classList.remove("state-ok", "state-warn", "state-bad");
}

function mustElement<T extends HTMLElement>(id: string): T {
  const element = document.getElementById(id);
  if (!element) {
    throw new Error(`Missing element #${id}`);
  }
  return element as T;
}
