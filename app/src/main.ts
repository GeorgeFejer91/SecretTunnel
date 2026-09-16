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
  logs: LogLine[];
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
  const startupIssue = latestStartupIssue(status);
  const zrokNeedsEnable = status.zrokInstalled && !status.zrokEnabled;
  document.body.classList.toggle("zrok-needs-enable", zrokNeedsEnable);
  elements.zrokSetup.hidden = !zrokNeedsEnable;
  elements.zrokState.textContent = status.zrokInstalled
    ? zrokNeedsEnable
      ? "Needs enable"
      : "Ready"
    : "Missing";
  elements.mcpState.textContent = status.gptRepoMcpFound ? "Ready" : "Missing";
  const mode = status.settings.accessMode === "read_write" ? "read+write" : "read-only";
  setStatusText(status.running ? `Running, ${mode}` : startupIssue ?? `Stopped, ${mode}`);
}

async function openZrokTokenPage() {
  try {
    await invoke("open_url", { url: ZROK_TOKEN_PAGE_URL });
  } catch (error) {
    renderError(error);
  }
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

function latestStartupIssue(status: Status): string | null {
  if (status.running || !status.settings.workspacePath) return null;
  if (status.zrokInstalled && !status.zrokEnabled) return "zrok needs enable";

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

function setStatusText(value: string) {
  elements.statusLine.textContent = value;
}

function mustElement<T extends HTMLElement>(id: string): T {
  const element = document.getElementById(id);
  if (!element) {
    throw new Error(`Missing element #${id}`);
  }
  return element as T;
}
