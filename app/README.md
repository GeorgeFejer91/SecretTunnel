# Secret Tunnel

A tiny Tauri desktop app for keeping one local `gpt-repo-mcp` folder share available to ChatGPT through a stable zrok URL.

The app is intentionally minimal:

- autostart checkbox
- permanent MCP URL field with copy button
- folder picker plus copy/paste/use-path controls
- read-only vs read+write mode toggle
- compact warning text about leaving the app open and choosing a narrow folder
- bundled zrok, Node, and `gpt-repo-mcp` readiness checks

## How It Works

The app writes its own managed `gpt-repo-mcp` config into the app config directory. It does not edit `gpt-repo-mcp/config.local.json`.

When started, it launches:

1. bundled Node running bundled `gpt-repo-mcp/dist/server.js` on `127.0.0.1:8787`
2. bundled `zrok2 share public http://127.0.0.1:8787 -n public:<name> --headless`

The ChatGPT URL is:

```text
https://<zrok-name>.shares.zrok.io/t/<fixed-token>/mcp
```

The zrok name and the MCP path token are stored locally so the address stays stable between app launches. zrok describes itself as open source and available as SaaS or self-hosted, and its namespace model maps public names to hosts such as `https://api.shares.zrok.io` (the zrok v2 SaaS public frontend uses the plural `shares.zrok.io`): [zrok home](https://zrok.io/) and [zrok namespaces](https://netfoundry.io/docs/zrok/concepts/namespaces/).

`zrok2`, Node, and the MCP server runtime are bundled with Secret Tunnel. On Windows, the installer also embeds the offline WebView2 runtime so first launch does not depend on a separate WebView download. The packaged app never downloads executable dependencies at runtime and does not depend on a user-installed `node`, `npm`, `zrok`, or PowerShell execution policy. The first run still needs a zrok account enable token; paste it into the app once, and Secret Tunnel runs `zrok2 enable ... --headless` in the background. Managed installs can instead launch the app once with `SECRET_TUNNEL_ZROK_ENABLE_TOKEN` or `ZROK_ENABLE_TOKEN` set. The token is not stored by Secret Tunnel.

## Requirements

- a zrok account enable token for first-run setup
- the normal OS webview stack for the target platform; Windows builds include the offline WebView2 installer
- ChatGPT Developer Mode for read+write tools, or read-only mode for read-only surfaces

The app uses repo id `workspace` for the selected folder.

Autostart launches pass `--background`; Secret Tunnel minimizes the main window for those launches while still starting the MCP and zrok processes if a folder is configured.

## Local Development

```powershell
npm.cmd install
npm.cmd run prepare:runtime
npm.cmd run build
npm.cmd run tauri build
```

Runtime packaging can be pointed at a specific MCP source when CI or release builds need a reviewed revision:

```powershell
$env:GPT_REPO_MCP_REPO_URL = 'https://github.com/CAHN91/gpt-repo-mcp.git'
$env:GPT_REPO_MCP_REPO_REF = '<branch-tag-or-commit>'
npm.cmd run prepare:runtime
```

Strict release prep can require a clean MCP source and a pinned MCP ref:

```powershell
$env:SECRET_TUNNEL_REQUIRE_CLEAN_MCP_SOURCE = '1'
$env:SECRET_TUNNEL_REQUIRE_PINNED_MCP_SOURCE = '1'
$env:GPT_REPO_MCP_REPO_REF = '<reviewed-commit-sha>'
npm.cmd run prepare:runtime
Remove-Item Env:\SECRET_TUNNEL_REQUIRE_CLEAN_MCP_SOURCE
Remove-Item Env:\SECRET_TUNNEL_REQUIRE_PINNED_MCP_SOURCE
Remove-Item Env:\GPT_REPO_MCP_REPO_REF
```

Rust checks:

```powershell
Set-Location .\src-tauri
cargo fmt --all -- --check
cargo check
cargo test
```

Live packaged E2E, after `npm.cmd run tauri build`, starts the release app with an isolated profile and temporary workspace, waits for bundled MCP locally, then verifies `tools/list` through the public zrok URL:

```powershell
npm.cmd run smoke:live
```

If zrok is not already enabled on the machine, provide a launch-only token:

```powershell
$env:SECRET_TUNNEL_ZROK_ENABLE_TOKEN = '<zrok-enable-token>'
npm.cmd run smoke:live
Remove-Item Env:\SECRET_TUNNEL_ZROK_ENABLE_TOKEN
```

The live E2E is not run by default in CI because it needs a real zrok account environment and public tunnel. To run it in GitHub Actions, add the repository secret `SECRET_TUNNEL_ZROK_ENABLE_TOKEN`, start the `Build` workflow manually, and enable the `run_live_e2e` input. The workflow runs that public tunnel proof only on the Windows packaged app and fails if the secret is missing.
For this smoke only, the app honors `SECRET_TUNNEL_STATUS_FILE` and writes sanitized booleans such as `running`, `starting`, `zrokEnabled`, and `mcpRuntimeFound`; it does not write the selected folder, public URL, or tokens.

## Windows Artifact

The local Windows build emits:

```text
src-tauri/target/release/bundle/msi/Secret Tunnel_0.1.0_x64_en-US.msi
src-tauri/target/release/bundle/nsis/Secret Tunnel_0.1.0_x64-setup.exe
```

After a release build, generate checksums and a deterministic artifact manifest:

```powershell
npm.cmd run release:checksums
```

This writes `src-tauri/target/release/bundle/SHA256SUMS.txt` and `artifact-manifest.json`.

## Cross-Platform Downloads

The GitHub Actions workflow builds on Windows, macOS, and Linux and uploads each platform's Tauri bundle as a downloadable workflow artifact.

## Security Notes

- Do not select a drive root, home folder, or system folder.
- Treat the public MCP URL like a temporary credential.
- Read-only mode sets `GPT_REPO_READ_ONLY_SURFACE=1`, so write-capable tools are absent from the MCP tool list.
- Read+write mode only enables file write tools in the managed `gpt-repo-mcp` config; git and validation operations stay disabled.
- zrok account enablement happens through the bundled `zrok2` binary, but it still requires the user's zrok token once. Launch-token environment variables are read once, used only for `zrok2 enable`, and removed from the app process after a successful enable.
