import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const scriptsDir = dirname(fileURLToPath(import.meta.url));

for (const script of ["prepare-zrok.mjs", "prepare-node.mjs", "prepare-gpt-repo-mcp.mjs"]) {
  const result = spawnSync(process.execPath, [join(scriptsDir, script)], {
    cwd: resolve(scriptsDir, ".."),
    stdio: "inherit",
  });
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

// Bundle the readiness probe alongside the packaged resources so the runtime
// probe scheduler can execute it through the bundled Node.
const probeSource = join(scriptsDir, "readiness-probe.mjs");
const probeOutputDir = resolve(scriptsDir, "..", "src-tauri", "resources", "scripts");
const probeOutput = join(probeOutputDir, "readiness-probe.mjs");
if (!existsSync(probeSource)) {
  throw new Error(`readiness probe missing at ${probeSource}`);
}
mkdirSync(probeOutputDir, { recursive: true });
copyFileSync(probeSource, probeOutput);
console.log(`Prepared readiness probe at ${probeOutput}`);