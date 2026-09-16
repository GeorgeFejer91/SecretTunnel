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
  gptRepoMcpFound: boolean;
  logs: LogLine[];
};

const elements = {
  statusLine: mustElement<HTMLParagraphElement>("status-line"),
  toggleServices: mustElement<HTMLButtonElement>("toggle-services"),
  mcpUrl: mustElement<HTMLInputElement>("mcp-url"),
  copyUrl: mustElement<HTMLButtonElement>("copy-url"),
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
  logs: mustElement<HTMLPreElement>("logs"),
};

let currentStatus: Status | null = null;
let busy = false;

window.addEventListener("DOMContentLoaded", () => {
  wireEvents();
  refresh();
  window.setInterval(refresh, 4000);
});

function wireEvents() {
  elements.toggleServices.addEventListener("click", () => {
    if (!currentStatus) return;
    runStatusCommand(currentStatus.running ? "stop_services" : "start_services");
  });

  elements.copyUrl.addEventListener("click", () => copyText(elements.mcpUrl.value));
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
    renderError(error);
    await refresh();
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
  elements.toggleServices.textContent = status.running ? "Stop" : "Start";
  elements.toggleServices.classList.toggle("danger", status.running);
  elements.zrokState.textContent = status.zrokInstalled ? "Ready" : "Missing";
  elements.mcpState.textContent = status.gptRepoMcpFound ? "Ready" : "Missing";
  const mode = status.settings.accessMode === "read_write" ? "read+write" : "read-only";
  setStatusText(status.running ? `Running, ${mode}` : `Stopped, ${mode}`);
  elements.logs.textContent = status.logs
    .slice(-8)
    .map((entry) => `[${entry.source}] ${entry.line}`)
    .join("\n");
}

function renderAccessMode(mode: Status["settings"]["accessMode"]) {
  const writeEnabled = mode === "read_write";
  elements.modeRead.classList.toggle("active", !writeEnabled);
  elements.modeWrite.classList.toggle("active", writeEnabled);
  elements.modeRead.setAttribute("aria-pressed", String(!writeEnabled));
  elements.modeWrite.setAttribute("aria-pressed", String(writeEnabled));
}

function renderError(error: unknown) {
  const appError = error as Partial<AppError>;
  setStatusText(appError.message ?? String(error));
}

function setControlsDisabled(disabled: boolean) {
  [
    elements.toggleServices,
    elements.folderPicker,
    elements.pasteFolder,
    elements.copyFolder,
    elements.saveFolder,
    elements.copyUrl,
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
