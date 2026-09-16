import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { fileURLToPath } from "node:url";
import { findRuntimeLayout } from "./runtime-layout.mjs";

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const layout = findRuntimeLayout(rootDir, process.argv[2] ?? "src-tauri");
const runtimeDir = layout.runtimeDir;
const serverPath = join(runtimeDir, "dist", "server.js");
const configPath = join(tmpdir(), `secret-tunnel-smoke-mcp-${process.pid}.json`);
const port = process.env.SECRET_TUNNEL_SMOKE_PORT ?? String(18000 + (process.pid % 10000));

await verifyRuntimeMarker();

await writeFile(
  configPath,
  `${JSON.stringify(
    {
      repos: [
        {
          repo_id: "workspace",
          display_name: "Secret Tunnel",
          root: rootDir,
          allow_non_git: true,
          writes: {
            enabled: false,
            allowed_globs: ["**"],
            denied_globs: [".git/**", ".env", ".env.*", "**/*.pem", "**/*.key"],
            max_bytes_per_write: 1048576,
          },
          operations: { enabled: false },
        },
      ],
      limits: { max_files: 10, max_bytes_per_file: 64000, max_total_bytes: 256000 },
    },
    null,
    2,
  )}\n`,
);

const child = spawn(layout.nodePath, [serverPath], {
  cwd: runtimeDir,
  env: {
    ...process.env,
    GPT_REPO_CONFIG: configPath,
    REPO_READER_CONFIG: configPath,
    GPT_REPO_HOST: "127.0.0.1",
    PORT: port,
    GPT_REPO_PUBLIC_PATH_TOKEN: "smoketest",
    REPO_READER_PUBLIC_PATH_TOKEN: "smoketest",
    GPT_REPO_READ_ONLY_SURFACE: "1",
  },
  stdio: ["ignore", "pipe", "pipe"],
  windowsHide: true,
});

let stderr = "";
let stdout = "";
let exitCode = null;
let exitSignal = null;
child.stdout.on("data", (chunk) => {
  stdout += chunk.toString();
});
child.stderr.on("data", (chunk) => {
  stderr += chunk.toString();
});
child.on("exit", (code, signal) => {
  exitCode = code;
  exitSignal = signal;
});

try {
  await waitForHealth(`http://127.0.0.1:${port}/health`);
  await verifyReadOnlyToolSurface(`http://127.0.0.1:${port}/t/smoketest/mcp`);
  console.log(`Bundled MCP smoke check passed for ${layout.runtimeRoot}.`);
} finally {
  child.kill();
}

async function waitForHealth(url) {
  const deadline = Date.now() + 8000;
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      const body = await response.json();
      if (response.ok && body?.ok === true && body?.name === "gpt-repo-mcp") return;
      lastError = new Error(`Unexpected health response: ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    if (exitCode !== null || exitSignal !== null) {
      throw new Error(
        `Bundled MCP exited before health was ready. code=${exitCode} signal=${exitSignal}\nstdout:\n${stdout}\nstderr:\n${stderr}`,
      );
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 300));
  }
  throw new Error(`Bundled MCP health check failed. ${lastError?.message ?? ""}\nstdout:\n${stdout}\nstderr:\n${stderr}`);
}

async function verifyReadOnlyToolSurface(url) {
  const [{ Client }, { StreamableHTTPClientTransport }] = await Promise.all([
    import(pathToFileURL(join(runtimeDir, "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js"))),
    import(pathToFileURL(join(runtimeDir, "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "streamableHttp.js"))),
  ]);
  const client = new Client({ name: "secret-tunnel-smoke", version: "0.1.0" });
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

async function verifyRuntimeMarker() {
  const legacyMarker = join(runtimeDir, ".secret-tunnel-runtime.json");
  if (existsSync(legacyMarker)) {
    throw new Error(`Legacy hidden MCP runtime marker must not be packaged: ${legacyMarker}`);
  }

  const markerPath = join(runtimeDir, "secret-tunnel-runtime.json");
  const marker = JSON.parse(await readFile(markerPath, "utf8"));
  if (marker.source !== "gpt-repo-mcp-runtime") {
    throw new Error(`Unexpected MCP runtime marker source: ${marker.source}`);
  }
  if (marker.packageName !== "gpt-repo-mcp" || !marker.packageVersion) {
    throw new Error(`Unexpected MCP package marker: ${JSON.stringify(marker)}`);
  }
  if (!marker.serverHash || !marker.lockHash) {
    throw new Error("MCP runtime marker must include serverHash and lockHash.");
  }
  if (Object.hasOwn(marker, "sourceDir")) {
    throw new Error("MCP runtime marker must not expose local sourceDir paths.");
  }
}
