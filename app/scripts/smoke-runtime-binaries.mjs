import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { findRuntimeLayout } from "./runtime-layout.mjs";

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const layout = findRuntimeLayout(rootDir, process.argv[2] ?? "src-tauri");
const expectedZrokVersion = process.env.SECRET_TUNNEL_ZROK_VERSION ?? "2.0.4";
const expectedZrokTarget = expectedZrokTargets()[`${process.platform}:${process.arch}`];
if (!expectedZrokTarget) {
  throw new Error(`No expected zrok provenance target for ${process.platform}/${process.arch}.`);
}

const nodeVersion = run(layout.nodePath, ["--version"]);
if (nodeVersion.trim() !== process.version) {
  throw new Error(`Bundled Node version mismatch. Expected ${process.version}, got ${nodeVersion.trim()}.`);
}

const zrokVersion = run(layout.zrokPath, ["version"]);
if (!zrokVersion.includes(expectedZrokVersion)) {
  throw new Error(`Bundled zrok version mismatch. Expected ${expectedZrokVersion}, got:\n${zrokVersion}`);
}

await verifyNodeMarker();
await verifyZrokMarker();

console.log(`Runtime binary smoke passed for ${layout.runtimeRoot}.`);

function run(command, args) {
  const result = spawnSync(command, args, { encoding: "utf8", windowsHide: true });
  const output = `${result.stdout ?? ""}${result.stderr ?? ""}`;
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed with code ${result.status}:\n${output}`);
  }
  return output;
}

async function verifyNodeMarker() {
  const marker = await readJson(join(dirname(layout.nodePath), "node-source.json"));
  const actualHash = await sha256(layout.nodePath);
  assertEqual(marker.source, "builder-node-runtime", "Node marker source");
  assertEqual(marker.version, process.version, "Node marker version");
  assertEqual(marker.platform, process.platform, "Node marker platform");
  assertEqual(marker.arch, process.arch, "Node marker arch");
  assertEqual(marker.executableName, basename(layout.nodePath), "Node marker executableName");
  assertEqual(marker.sha256, actualHash, "Node marker sha256");
}

async function verifyZrokMarker() {
  const binaryDir = dirname(layout.zrokPath);
  const legacyMarker = join(binaryDir, ".zrok-source.json");
  if (existsSync(legacyMarker)) {
    throw new Error(`Legacy hidden zrok marker must not be packaged: ${legacyMarker}`);
  }

  const marker = await readJson(join(binaryDir, "zrok-source.json"));
  assertEqual(marker.source, "official-release-archive", "zrok marker source");
  assertEqual(marker.version, expectedZrokVersion, "zrok marker version");
  assertEqual(marker.archiveName, expectedZrokTarget.archiveName, "zrok marker archiveName");
  assertEqual(marker.archiveSha256, expectedZrokTarget.archiveSha256, "zrok marker archiveSha256");
}

async function readJson(path) {
  try {
    return JSON.parse(await readFile(path, "utf8"));
  } catch (error) {
    throw new Error(`Could not read runtime provenance marker ${path}: ${error.message}`);
  }
}

async function sha256(path) {
  const hash = createHash("sha256");
  hash.update(await readFile(path));
  return hash.digest("hex");
}

function assertEqual(actual, expected, label) {
  if (actual !== expected) {
    throw new Error(`${label} mismatch. Expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}.`);
  }
}

function expectedZrokTargets() {
  return {
    "win32:x64": {
      archiveName: `zrok_${expectedZrokVersion}_windows_amd64.tar.gz`,
      archiveSha256: "8e4062a159f65c3735d67d82de0f6a6f59555e9f98a786e80c1e6ab22d92d8c9",
    },
    "darwin:x64": {
      archiveName: `zrok_${expectedZrokVersion}_darwin_amd64.tar.gz`,
      archiveSha256: "d0d0882d84768081c7cbd45c03490bd13e305d19861eaf4811a11e6eb1db5924",
    },
    "darwin:arm64": {
      archiveName: `zrok_${expectedZrokVersion}_darwin_arm64.tar.gz`,
      archiveSha256: "ad90ee0730bdd066a0a95c1f57bb250bac1b2d5c474ba662043820ea0d2b7e86",
    },
    "linux:x64": {
      archiveName: `zrok_${expectedZrokVersion}_linux_amd64.tar.gz`,
      archiveSha256: "1877981b9050c9d69c61bc12c0b92c2da7330e3fbb374faa78ffdbfa37f8a8e3",
    },
    "linux:arm64": {
      archiveName: `zrok_${expectedZrokVersion}_linux_arm64.tar.gz`,
      archiveSha256: "71a08d11058959a0b90e8f59d4a33612b5fc010fced8c65883995cf64e5502cc",
    },
  };
}
