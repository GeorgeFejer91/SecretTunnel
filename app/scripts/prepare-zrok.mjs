import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createWriteStream, existsSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { chmod, copyFile, readFile, stat, writeFile } from "node:fs/promises";
import { get } from "node:https";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ZROK_VERSION = "2.0.4";
const TARGETS = {
  "win32:x64": {
    archivePlatform: "windows_amd64",
    executableName: "zrok2.exe",
    sha256: "8e4062a159f65c3735d67d82de0f6a6f59555e9f98a786e80c1e6ab22d92d8c9",
  },
  "darwin:x64": {
    archivePlatform: "darwin_amd64",
    executableName: "zrok2",
    sha256: "d0d0882d84768081c7cbd45c03490bd13e305d19861eaf4811a11e6eb1db5924",
  },
  "darwin:arm64": {
    archivePlatform: "darwin_arm64",
    executableName: "zrok2",
    sha256: "ad90ee0730bdd066a0a95c1f57bb250bac1b2d5c474ba662043820ea0d2b7e86",
  },
  "linux:x64": {
    archivePlatform: "linux_amd64",
    executableName: "zrok2",
    sha256: "1877981b9050c9d69c61bc12c0b92c2da7330e3fbb374faa78ffdbfa37f8a8e3",
  },
  "linux:arm64": {
    archivePlatform: "linux_arm64",
    executableName: "zrok2",
    sha256: "71a08d11058959a0b90e8f59d4a33612b5fc010fced8c65883995cf64e5502cc",
  },
};

const target = TARGETS[`${process.platform}:${process.arch}`];
if (!target) {
  throw new Error(`No bundled zrok target for ${process.platform}/${process.arch}.`);
}

const ARCHIVE_NAME = `zrok_${ZROK_VERSION}_${target.archivePlatform}.tar.gz`;
const DOWNLOAD_URL = `https://github.com/openziti/zrok/releases/download/v${ZROK_VERSION}/${ARCHIVE_NAME}`;

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const binaryDir = join(rootDir, "src-tauri", "binaries");
const outputPath = join(binaryDir, target.executableName);
const markerPath = join(binaryDir, "zrok-source.json");
const cacheDir = join(tmpdir(), `secret-tunnel-zrok-${ZROK_VERSION}-${target.archivePlatform}`);
const archivePath = join(cacheDir, ARCHIVE_NAME);
const extractDir = join(cacheDir, "extract");

mkdirSync(binaryDir, { recursive: true });

if (existsSync(outputPath) && hasExpectedVersion(outputPath) && await hasVerifiedMarker()) {
  console.log(`zrok2 ${ZROK_VERSION} already prepared at ${outputPath}`);
  process.exit(0);
}

const localZrok = process.env.SECRET_TUNNEL_USE_LOCAL_ZROK === "1" ? findLocalZrok() : null;
if (localZrok && hasExpectedVersion(localZrok)) {
  await copyFile(localZrok, outputPath);
  await makeExecutable(outputPath);
  await writeMarker({ source: "local", localZrok });
  console.log(`Copied zrok2 ${ZROK_VERSION} from ${localZrok}`);
  process.exit(0);
}

mkdirSync(cacheDir, { recursive: true });
await download(DOWNLOAD_URL, archivePath);

const archiveHash = await sha256(archivePath);
if (archiveHash !== target.sha256) {
  throw new Error(
    `zrok archive checksum mismatch. Expected ${target.sha256}, got ${archiveHash}.`,
  );
}

rmSync(extractDir, { recursive: true, force: true });
mkdirSync(extractDir, { recursive: true });

const tar = spawnSync("tar", ["-xzf", archivePath, "-C", extractDir], {
  stdio: "inherit",
});
if (tar.status !== 0) {
  throw new Error("Could not extract the zrok release archive with tar.");
}

const extracted = findFile(extractDir, target.executableName);
if (!extracted) {
  throw new Error(`The zrok release archive did not contain ${target.executableName}.`);
}

await copyFile(extracted, outputPath);
await makeExecutable(outputPath);
if (!hasExpectedVersion(outputPath)) {
  throw new Error(`Prepared ${target.executableName} does not report version ${ZROK_VERSION}.`);
}
await writeMarker({ source: "official-release-archive" });

console.log(`Prepared bundled zrok2 ${ZROK_VERSION} at ${outputPath}`);

function findLocalZrok() {
  const candidates = [];
  if (process.env.LOCALAPPDATA) {
    candidates.push(join(process.env.LOCALAPPDATA, "Programs", "zrok", "zrok2.exe"));
  }
  for (const candidate of candidates) {
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

function hasExpectedVersion(path) {
  if (!existsSync(path)) return false;
  const result = spawnSync(path, ["version"], { encoding: "utf8" });
  const output = `${result.stdout ?? ""}${result.stderr ?? ""}`;
  return result.status === 0 && output.includes(ZROK_VERSION);
}

function findFile(dir, fileName) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const fullPath = join(dir, entry.name);
    if (entry.isDirectory()) {
      const found = findFile(fullPath, fileName);
      if (found) return found;
    } else if (entry.isFile() && basename(entry.name).toLowerCase() === fileName.toLowerCase()) {
      return fullPath;
    }
  }
  return null;
}

async function sha256(path) {
  const hash = createHash("sha256");
  hash.update(await readFile(path));
  return hash.digest("hex");
}

async function hasVerifiedMarker() {
  try {
    const marker = JSON.parse(await readFile(markerPath, "utf8"));
    return marker.version === ZROK_VERSION
      && marker.archiveName === ARCHIVE_NAME
      && marker.archiveSha256 === target.sha256
      && marker.source === "official-release-archive";
  } catch {
    return false;
  }
}

async function writeMarker(extra) {
  await writeFile(
    markerPath,
    `${JSON.stringify({
      version: ZROK_VERSION,
      archiveName: ARCHIVE_NAME,
      archiveSha256: target.sha256,
      preparedAt: new Date().toISOString(),
      ...extra,
    }, null, 2)}\n`,
  );
}

async function download(url, destination) {
  if (existsSync(destination) && (await stat(destination)).size > 0) {
    const existingHash = await sha256(destination);
    if (existingHash === target.sha256) {
      console.log(`Using cached ${ARCHIVE_NAME}`);
      return;
    }
  }

  await new Promise((resolvePromise, rejectPromise) => {
    const request = (currentUrl, redirects = 0) => {
      get(currentUrl, (response) => {
        if ([301, 302, 303, 307, 308].includes(response.statusCode ?? 0)) {
          if (!response.headers.location || redirects > 5) {
            rejectPromise(new Error(`Could not follow redirect for ${currentUrl}`));
            return;
          }
          request(new URL(response.headers.location, currentUrl).toString(), redirects + 1);
          return;
        }

        if (response.statusCode !== 200) {
          rejectPromise(new Error(`Download failed with HTTP ${response.statusCode}`));
          return;
        }

        const file = createWriteStream(destination);
        response.pipe(file);
        file.on("finish", () => file.close(resolvePromise));
        file.on("error", rejectPromise);
      }).on("error", rejectPromise);
    };

    request(url);
  });
}

async function makeExecutable(path) {
  if (process.platform !== "win32") {
    await chmod(path, 0o755);
  }
}
