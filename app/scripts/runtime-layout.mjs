import { existsSync, readdirSync } from "node:fs";
import { basename, join, resolve } from "node:path";

export function findRuntimeLayout(rootDir, requestedRuntimeRoot = "src-tauri") {
  const requested = resolve(rootDir, requestedRuntimeRoot);
  const searched = [];

  for (const runtimeRoot of runtimeRootCandidates(requested)) {
    searched.push(runtimeRoot);
    const nodePath = executableIn(join(runtimeRoot, "binaries"), "node");
    const zrokPath = executableIn(join(runtimeRoot, "binaries"), "zrok2");
    const runtimeDir = mcpRuntimeDir(runtimeRoot);
    if (nodePath && zrokPath && runtimeDir) {
      return { runtimeRoot, nodePath, zrokPath, runtimeDir };
    }
  }

  throw new Error(
    [
      `Could not find a complete Secret Tunnel runtime layout under ${requested}.`,
      "Expected bundled node, zrok2, and gpt-repo-mcp resources.",
      "Searched:",
      ...searched.map((candidate) => `  - ${candidate}`),
    ].join("\n"),
  );
}

function runtimeRootCandidates(requested) {
  return unique([
    requested,
    join(requested, "Contents", "Resources"),
    join(requested, "Resources"),
    ...findMacAppResourceDirs(requested, 5),
  ]);
}

function findMacAppResourceDirs(root, maxDepth) {
  const results = [];
  visit(root, 0);
  return results;

  function visit(dir, depth) {
    if (depth > maxDepth || !existsSync(dir)) return;

    let entries = [];
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }

    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      if ([".git", "node_modules", "debug", "incremental"].includes(entry.name)) continue;

      const fullPath = join(dir, entry.name);
      if (entry.name.endsWith(".app")) {
        results.push(join(fullPath, "Contents", "Resources"));
        continue;
      }
      visit(fullPath, depth + 1);
    }
  }
}

function executableIn(dir, command) {
  for (const candidate of executableNames(command)) {
    const path = join(dir, candidate);
    if (existsSync(path)) return path;
  }
  return null;
}

function executableNames(command) {
  if (process.platform !== "win32") return [command];
  if (command.includes(".")) return [command];
  return [`${command}.exe`, `${command}.cmd`, `${command}.bat`];
}

function mcpRuntimeDir(runtimeRoot) {
  for (const candidate of [
    join(runtimeRoot, "resources", "gpt-repo-mcp"),
    join(runtimeRoot, "gpt-repo-mcp"),
  ]) {
    if (
      existsSync(join(candidate, "dist", "server.js")) &&
      existsSync(join(candidate, "node_modules"))
    ) {
      return candidate;
    }
  }
  return null;
}

function unique(values) {
  const seen = new Set();
  const result = [];
  for (const value of values) {
    const normalized = resolve(value);
    const key = process.platform === "win32" ? normalized.toLowerCase() : normalized;
    if (seen.has(key)) continue;
    seen.add(key);
    result.push(normalized);
  }
  return result;
}
