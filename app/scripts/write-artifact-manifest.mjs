import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { readdir, stat, writeFile } from "node:fs/promises";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const bundleDir = resolve(rootDir, "src-tauri", "target", "release", "bundle");
const manifestPath = join(bundleDir, "artifact-manifest.json");
const sumsPath = join(bundleDir, "SHA256SUMS.txt");

const files = (await listFiles(bundleDir))
  .filter((file) => file !== manifestPath && file !== sumsPath)
  .sort((left, right) => relativePath(left).localeCompare(relativePath(right)));

const artifacts = [];
for (const file of files) {
  const info = await stat(file);
  artifacts.push({
    path: relativePath(file),
    bytes: info.size,
    sha256: await sha256(file),
  });
}

await writeFile(manifestPath, `${JSON.stringify({ artifacts }, null, 2)}\n`);
await writeFile(
  sumsPath,
  `${artifacts.map((artifact) => `${artifact.sha256}  ${artifact.path}`).join("\n")}\n`,
);

console.log(`Wrote ${artifacts.length} artifact checksums to ${sumsPath}`);

async function listFiles(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const fullPath = join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...await listFiles(fullPath));
    } else if (entry.isFile()) {
      files.push(fullPath);
    }
  }
  return files;
}

function relativePath(file) {
  return relative(bundleDir, file).split(sep).join("/");
}

async function sha256(file) {
  const hash = createHash("sha256");
  await new Promise((resolvePromise, rejectPromise) => {
    createReadStream(file)
      .on("data", (chunk) => hash.update(chunk))
      .on("error", rejectPromise)
      .on("end", resolvePromise);
  });
  return hash.digest("hex");
}
