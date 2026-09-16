# Secret Tunnel — Release Protocol

## Versioning & Identifier

- App version: `0.1.0` (see `app/src-tauri/tauri.conf.json` and `Cargo.toml`).
- Bundle identifier: `com.georgefejer.chatgpt-local-mcp-launcher`.
- CI triggers on `v*` tags pushed to `main`.

## Building a Release

Run from `app/`:

```powershell
# 1. Optional: pin a reviewed MCP source revision
$env:SECRET_TUNNEL_REQUIRE_CLEAN_MCP_SOURCE = '1'
$env:SECRET_TUNNEL_REQUIRE_PINNED_MCP_SOURCE = '1'
$env:GPT_REPO_MCP_REPO_REF = '<reviewed-commit-sha>'

# 2. Prepare bundled runtime
npm.cmd run prepare:runtime

# 3. Build release bundles
npm.cmd run tauri build

# 4. Generate checksums + artifact manifest (Windows)
npm.cmd run release:checksums
```

Windows artifacts are emitted at:

```text
app/src-tauri/target/release/bundle/msi/Secret Tunnel_0.1.0_x64_en-US.msi
app/src-tauri/target/release/bundle/nsis/Secret Tunnel_0.1.0_x64-setup.exe
app/src-tauri/target/release/bundle/SHA256SUMS.txt
app/src-tauri/target/release/bundle/artifact-manifest.json
```

## Promoting Artifacts into This Repo

1. Copy the MSI and EXE into `outputs/installers/`.
2. Copy `SHA256SUMS.txt` + `artifact-manifest.json` into `outputs/checksums/` and
   commit those two files.
3. The installers themselves are NOT committed (GitHub rejects files > 100 MB).
   They are distributed as **GitHub Release assets**.

## Cross-Platform

Each platform's bundle is produced by the CI workflow (`app/.github/workflows/build.yml`)
as a downloadable workflow artifact. For public distribution, attach them to a GitHub
Release alongside the Windows installers.

## Distribution

- GitHub Releases: attach the MSI, NSIS EXE, and the platform bundles, plus
  `SHA256SUMS.txt`.
- The owner's "read-only mode" launch instructions live in the repo README and were
  validated with:
  ```powershell
  $env:GPT_REPO_READ_ONLY_SURFACE = '1'
  npm.cmd run connect   # or connect:secure   (inside a gpt-repo-mcp checkout)
  ```

## First-Run Requirements for End Users

- A zrok account enable token (pasted once, or via `SECRET_TUNNEL_ZROK_ENABLE_TOKEN`).
- A pre-cloned + built `gpt-repo-mcp` at the default GitHub path (or choose it), OR a
  packaged build that already bundles gpt-repo-mcp in `resources/`.
- Windows builds embed the offline WebView2 runtime.

## Security Reminders for Notes

- Public MCP URL should be treated like a temporary credential.
- Recommend a narrow folder, never a drive root or home folder.
- Read-only mode: `GPT_REPO_READ_ONLY_SURFACE=1` makes write tools absent.
- Read+write mode only enables file-write tools; git and validation ops stay disabled.