import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync } from "node:fs";
import { copyFile, readFile, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const binaryDir = join(rootDir, "src-tauri", "binaries");
const nodeName = process.platform === "win32" ? "node.exe" : "node";
const outputPath = join(binaryDir, nodeName);
const markerPath = join(binaryDir, "node-source.json");

mkdirSync(binaryDir, { recursive: true });

if (existsSync(outputPath) && sameNodeVersion(outputPath) && await hasVerifiedMarker()) {
  console.log(`Node ${process.version} already prepared at ${outputPath}`);
  process.exit(0);
}

await copyFile(process.execPath, outputPath);
if (process.platform !== "win32") {
  const { chmod } = await import("node:fs/promises");
  await chmod(outputPath, 0o755);
}

if (!sameNodeVersion(outputPath)) {
  throw new Error(`Prepared ${nodeName} does not report ${process.version}.`);
}

await writeMarker();

console.log(`Prepared bundled Node ${process.version} at ${outputPath}`);

function sameNodeVersion(path) {
  const result = spawnSync(path, ["--version"], { encoding: "utf8" });
  return result.status === 0 && `${result.stdout}${result.stderr}`.trim() === process.version;
}

async function hasVerifiedMarker() {
  try {
    const marker = JSON.parse(await readFile(markerPath, "utf8"));
    return marker.source === "builder-node-runtime"
      && marker.version === process.version
      && marker.platform === process.platform
      && marker.arch === process.arch
      && marker.executableName === nodeName
      && marker.sha256 === await sha256(outputPath);
  } catch {
    return false;
  }
}

async function writeMarker() {
  await writeFile(
    markerPath,
    `${JSON.stringify({
      source: "builder-node-runtime",
      version: process.version,
      platform: process.platform,
      arch: process.arch,
      executableName: nodeName,
      sha256: await sha256(outputPath),
      preparedAt: new Date().toISOString(),
    }, null, 2)}\n`,
  );
}

async function sha256(path) {
  const hash = createHash("sha256");
  hash.update(await readFile(path));
  return hash.digest("hex");
}
