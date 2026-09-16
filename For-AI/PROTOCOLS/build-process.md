# Secret Tunnel — Build & Verification Protocol

All commands are described for **Windows PowerShell**. Use `npm.cmd` (not `npm`) so
the call works even when the PowerShell execution policy blocks `npm.ps1`.

> **Build slow?** Do not start guessing. [build-performance.md](build-performance.md)
> records where the time actually goes, measured per step, plus the caching and
> concurrency already in place and the rules that keep them working.

## Environment Prerequisites

- Node.js 24 (matches CI)
- Rust stable (matches CI)
- A zrok account enable token for first-run and live E2E only
- For local release builds, `gpt-repo-mcp` will be cloned/bundled by the prepare step

## Commands (run from `app/`)

### Install dependencies

```powershell
npm.cmd install          # or: npm.cmd ci
```

### Prepare bundled runtime (Node, zrok2, gpt-repo-mcp)

```powershell
npm.cmd run prepare:runtime
```

`prepare:runtime` is the aggregator that runs, in order:
`prepare-zrok.mjs`, `prepare-node.mjs`, `prepare-gpt-repo-mcp.mjs`.

OPTIONAL injection of a specific MCP source revision (for CI / reviewed builds):

```powershell
$env:GPT_REPO_MCP_REPO_URL = 'https://github.com/CAHN91/gpt-repo-mcp.git'
$env:GPT_REPO_MCP_REPO_REF = '<branch-tag-or-commit>'
npm.cmd run prepare:runtime
```

Strict release prep requires a clean + pinned MCP source:

```powershell
$env:SECRET_TUNNEL_REQUIRE_CLEAN_MCP_SOURCE = '1'
$env:SECRET_TUNNEL_REQUIRE_PINNED_MCP_SOURCE = '1'
$env:GPT_REPO_MCP_REPO_REF = '<reviewed-commit-sha>'
npm.cmd run prepare:runtime
Remove-Item Env:\SECRET_TUNNEL_REQUIRE_CLEAN_MCP_SOURCE
Remove-Item Env:\SECRET_TUNNEL_REQUIRE_PINNED_MCP_SOURCE
Remove-Item Env:\GPT_REPO_MCP_REPO_REF
```

### Frontend checks + build (TS + Vite)

```powershell
npm.cmd run build
```

### Rust checks

```powershell
# from app/src-tauri
cargo fmt --all -- --check
cargo check
cargo test
```

### Full Tauri release build

```powershell
npm.cmd run tauri build
```

Emits (Windows):

```text
app/src-tauri/target/release/bundle/msi/Secret Tunnel_0.1.0_x64_en-US.msi
app/src-tauri/target/release/bundle/nsis/Secret Tunnel_0.1.0_x64-setup.exe
```

### Smoke tests (local)

```powershell
npm.cmd run smoke:binaries            # staged Node/zrok2 present
npm.cmd run smoke:layout              # runtime layout under src-tauri matches expectations
npm.cmd run smoke:ui                  # frontend/UI contract checks
npm.cmd run smoke:mcp                 # bundled gpt-repo-mcp serves tools/list locally
```

### Live packaged E2E (tunnel proof, requires zrok account)

```powershell
$env:SECRET_TUNNEL_ZROK_ENABLE_TOKEN = '<zrok-enable-token>'
npm.cmd run smoke:live
Remove-Item Env:\SECRET_TUNNEL_ZROK_ENABLE_TOKEN
```

Launches the release app with an isolated profile + temp workspace, waits for the
bundled MCP locally, then verifies `tools/list` through the public zrok URL.

### Release checksums (after a release build)

```powershell
npm.cmd run release:checksums
```

This writes `app/src-tauri/target/release/bundle/SHA256SUMS.txt` and
`artifact-manifest.json` (deterministic, sorted, relative paths).

## CI

GitHub Actions workflow: `app/.github/workflows/build.yml`.

- Builds Windows, macOS, Linux.
- Runs frontend build, Rust fmt/check/test, then `tauri build`.
- Uploads each platform bundle as a workflow artifact.
- Live E2E is NOT default; it is a manual workflow input (`run_live_e2e`) gated on the
  `SECRET_TUNNEL_ZROK_ENABLE_TOKEN` repository secret.

## Verification Checklist Before Merging a Change

1. `npm.cmd run build` passes (TS check + Vite).
2. `cargo fmt --all -- --check`, `cargo check`, `cargo test` pass.
3. Rust unit tests in `src-tauri/src/settings.rs` pass (validation + access mode).
4. If behavior affects tool surface: run `smoke:mcp` and confirm `tools/list`.
5. If behavior affects the UI: run `smoke:ui`.