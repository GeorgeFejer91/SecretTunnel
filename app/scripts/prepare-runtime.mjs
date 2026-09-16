import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
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
