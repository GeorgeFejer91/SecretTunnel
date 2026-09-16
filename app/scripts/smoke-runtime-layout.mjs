import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { findRuntimeLayout } from "./runtime-layout.mjs";

const nodeName = process.platform === "win32" ? "node.exe" : "node";
const zrokName = process.platform === "win32" ? "zrok2.exe" : "zrok2";
const tempRoot = await mkdtemp(join(tmpdir(), "secret-tunnel-layout-smoke-"));

try {
  const directRoot = join(tempRoot, "src-tauri");
  await createRuntimeLayout(directRoot);
  const direct = findRuntimeLayout(tempRoot, "src-tauri");
  assertEqual(direct.runtimeRoot, directRoot, "direct runtime layout");
  assertEqual(basename(direct.nodePath), nodeName, "direct node path");
  assertEqual(basename(direct.zrokPath), zrokName, "direct zrok path");

  const releaseRoot = join(tempRoot, "target", "release");
  const appResources = join(
    releaseRoot,
    "bundle",
    "macos",
    "Secret Tunnel.app",
    "Contents",
    "Resources",
  );
  await createRuntimeLayout(appResources);
  const appBundle = findRuntimeLayout(tempRoot, join("target", "release"));
  assertEqual(appBundle.runtimeRoot, appResources, "macOS app bundle resource layout");

  console.log("Runtime layout smoke check passed.");
} finally {
  await rm(tempRoot, { recursive: true, force: true });
}

async function createRuntimeLayout(root) {
  await mkdir(join(root, "binaries"), { recursive: true });
  await mkdir(join(root, "resources", "gpt-repo-mcp", "dist"), { recursive: true });
  await mkdir(join(root, "resources", "gpt-repo-mcp", "node_modules"), { recursive: true });
  await writeFile(join(root, "binaries", nodeName), "");
  await writeFile(join(root, "binaries", zrokName), "");
  await writeFile(join(root, "resources", "gpt-repo-mcp", "dist", "server.js"), "");
}

function assertEqual(actual, expected, label) {
  if (actual !== expected) {
    throw new Error(`${label}: expected ${expected}, got ${actual}`);
  }
}
