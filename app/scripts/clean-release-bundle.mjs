import { rm } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const releaseDir = resolve(rootDir, "src-tauri", "target", "release");

for (const path of [
  resolve(releaseDir, "bundle"),
  resolve(releaseDir, "binaries"),
  resolve(releaseDir, "resources"),
]) {
  await rm(path, { recursive: true, force: true });
  console.log(`Removed ${path}`);
}
