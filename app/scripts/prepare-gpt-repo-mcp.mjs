import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, rmSync } from "node:fs";
import { copyFile, readFile, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outputDir = join(rootDir, "src-tauri", "resources", "gpt-repo-mcp");
const markerPath = join(outputDir, "secret-tunnel-runtime.json");
const defaultRepoUrl = "https://github.com/CAHN91/gpt-repo-mcp.git";
const sourceRepoUrl = process.env.GPT_REPO_MCP_REPO_URL ?? defaultRepoUrl;
const sourceRepoRef = process.env.GPT_REPO_MCP_REPO_REF?.trim();
const requireCleanSource = process.env.SECRET_TUNNEL_REQUIRE_CLEAN_MCP_SOURCE === "1";
const requirePinnedSource = process.env.SECRET_TUNNEL_REQUIRE_PINNED_MCP_SOURCE === "1";

const source = prepareSourceDir();
const sourceDir = source.dir;
const npm = process.platform === "win32" ? "npm.cmd" : "npm";

// A vendored copy lives inside this repository, so `git` run in that directory
// answers about *this* project, not the server's own history: HEAD would be the
// launcher's commit and `status --porcelain` would report unrelated edits
// elsewhere in the tree. Provenance therefore comes from the vendor manifest,
// and integrity is tracked by this repository's own history rather than by a
// dirty-checkout probe that cannot mean anything here.
const vendorManifest =
  source.kind === "vendored-source" ? await readJson(join(sourceDir, "VENDOR.json")) : null;
if (source.kind === "vendored-source" && !vendorManifest) {
  throw new Error(
    "Vendored gpt-repo-mcp is missing VENDOR.json; the bundled server's origin cannot be recorded.",
  );
}

const effectiveRepoUrl = vendorManifest?.forkRepoUrl ?? sourceRepoUrl;
const sourceGitHead = vendorManifest
  ? (vendorManifest.forkCommit ?? vendorManifest.upstreamCommit ?? null)
  : gitOutput(sourceDir, ["rev-parse", "HEAD"]);
const sourceDirty = vendorManifest
  ? false
  : (gitOutput(sourceDir, ["status", "--porcelain"]) ?? "").trim().length > 0;

validateReleaseSource();

if (!existsSync(join(sourceDir, "node_modules"))) {
  run(npm, ["ci"], sourceDir);
}
run(npm, ["run", "build"], sourceDir);

const distServer = join(sourceDir, "dist", "server.js");
const packageJson = join(sourceDir, "package.json");
const packageLock = join(sourceDir, "package-lock.json");
const packageInfo = JSON.parse(await readFile(packageJson, "utf8"));
const serverHash = await sha256(distServer);
const lockHash = await sha256(packageLock);
const currentMarker = await readJson(markerPath);

if (
  currentMarker?.serverHash === serverHash &&
  currentMarker?.lockHash === lockHash &&
  currentMarker?.sourceKind === source.kind &&
  currentMarker?.sourceRepoUrl === effectiveRepoUrl &&
  currentMarker?.sourceRepoRef === (sourceRepoRef ?? null) &&
  currentMarker?.sourceGitHead === sourceGitHead &&
  currentMarker?.sourceDirty === sourceDirty &&
  existsSync(join(outputDir, "node_modules"))
) {
  console.log(`gpt-repo-mcp runtime already prepared at ${outputDir}`);
  process.exit(0);
}

rmSync(outputDir, { recursive: true, force: true });
mkdirSync(join(outputDir, "dist"), { recursive: true });
await copyFile(distServer, join(outputDir, "dist", "server.js"));
await copyFile(packageJson, join(outputDir, "package.json"));
await copyFile(packageLock, join(outputDir, "package-lock.json"));

run(npm, ["ci", "--omit=dev", "--ignore-scripts", "--no-audit", "--no-fund"], outputDir);

await writeFile(
  markerPath,
  `${JSON.stringify({
    source: "gpt-repo-mcp-runtime",
    packageName: packageInfo.name,
    packageVersion: packageInfo.version,
    serverHash,
    lockHash,
    sourceKind: source.kind,
    sourceRepoUrl: effectiveRepoUrl,
    upstreamRepoUrl: vendorManifest?.upstreamRepoUrl ?? null,
    upstreamCommit: vendorManifest?.upstreamCommit ?? null,
    license: vendorManifest?.license ?? null,
    sourceRepoRef: sourceRepoRef ?? null,
    sourceGitHead,
    sourceDirty,
    preparedAt: new Date().toISOString(),
  }, null, 2)}\n`,
);

console.log(`Prepared bundled gpt-repo-mcp runtime at ${outputDir}`);

function prepareSourceDir() {
  const explicit = process.env.GPT_REPO_MCP_SOURCE_DIR;
  // The vendored copy wins over a sibling checkout on purpose. Preferring a
  // developer's working tree is what let a verified local build contain an
  // uncommitted fix that no clean clone reproduced, so the default source is
  // the one recorded in this repository. Point GPT_REPO_MCP_SOURCE_DIR at a
  // checkout to iterate on the server deliberately.
  const candidates = [
    explicit ? { dir: resolve(explicit), kind: "explicit-local-source" } : null,
    { dir: join(rootDir, "vendor", "gpt-repo-mcp"), kind: "vendored-source" },
    { dir: resolve(rootDir, "..", "gpt-repo-mcp"), kind: "sibling-local-source" },
  ].filter(Boolean);

  for (const candidate of candidates) {
    if (existsSync(join(candidate.dir, "package.json")) && existsSync(join(candidate.dir, "src"))) {
      console.log(`Using ${candidate.kind} at ${candidate.dir}`);
      return candidate;
    }
  }

  const cacheDir = join(rootDir, ".runtime-cache", "gpt-repo-mcp");
  if (!existsSync(join(cacheDir, "package.json"))) {
    mkdirSync(dirname(cacheDir), { recursive: true });
    run("git", ["clone", "--depth", "1", sourceRepoUrl, cacheDir], rootDir);
    if (sourceRepoRef) checkoutSourceRef(cacheDir, sourceRepoRef);
  } else if (sourceRepoRef) {
    checkoutSourceRef(cacheDir, sourceRepoRef);
  }
  return { dir: cacheDir, kind: "git-cache" };
}

function checkoutSourceRef(cacheDir, ref) {
  run("git", ["fetch", "--depth", "1", "origin", ref], cacheDir);
  run("git", ["checkout", "--detach", "FETCH_HEAD"], cacheDir);
}

function validateReleaseSource() {
  if (requireCleanSource && sourceDirty) {
    throw new Error(
      [
        "Bundled gpt-repo-mcp source is dirty.",
        "Commit or discard the MCP source changes before producing a strict release build.",
      ].join(" "),
    );
  }

  if (requirePinnedSource && !sourceRepoRef) {
    throw new Error(
      [
        "Strict release builds require GPT_REPO_MCP_REPO_REF to pin the bundled MCP source.",
        "Use a reviewed branch, tag, or commit SHA.",
      ].join(" "),
    );
  }

  if (requirePinnedSource && source.kind === "git-cache" && sourceGitHead === null) {
    throw new Error("Strict release builds require git metadata for the bundled MCP source.");
  }
}

function run(command, args, cwd) {
  const windowsCmd = process.platform === "win32" && command.endsWith(".cmd");
  const result = spawnSync(windowsCmd ? (process.env.ComSpec ?? "cmd.exe") : command, windowsCmd ? ["/d", "/s", "/c", command, ...args] : args, {
    cwd,
    stdio: "inherit",
  });
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed in ${cwd}`);
  }
}

function gitOutput(cwd, args) {
  const result = spawnSync("git", args, {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "ignore"],
  });
  if (result.status !== 0) return null;
  return result.stdout.trim();
}

async function sha256(path) {
  const hash = createHash("sha256");
  hash.update(await readFile(path));
  return hash.digest("hex");
}

async function readJson(path) {
  try {
    return JSON.parse(await readFile(path, "utf8"));
  } catch {
    return null;
  }
}
