# Secret Tunnel

A tiny Tauri desktop app for keeping one local `gpt-repo-mcp` folder share available to ChatGPT through a stable zrok URL.

The app is intentionally minimal:

- autostart checkbox
- permanent MCP URL field with copy button
- folder picker plus copy/paste/use-path controls
- read-only vs read+write mode toggle
- compact warning text about leaving the app open and choosing a narrow folder
- zrok and `gpt-repo-mcp` readiness checks

## How It Works

The app writes its own managed `gpt-repo-mcp` config into the app config directory. It does not edit `gpt-repo-mcp/config.local.json`.

When started, it launches:

1. `gpt-repo-mcp` on `127.0.0.1:8787`
2. `zrok2 share public http://127.0.0.1:8787 -n public:<name> --headless`

The ChatGPT URL is:

```text
https://<zrok-name>.share.zrok.io/t/<fixed-token>/mcp
```

The zrok name and the MCP path token are stored locally so the address stays stable between app launches. zrok describes itself as open source and available as SaaS or self-hosted, and its namespace model maps public names to hosts such as `https://api.share.zrok.io`: [zrok home](https://zrok.io/) and [zrok namespaces](https://netfoundry.io/docs/zrok/concepts/namespaces/).

## Requirements

- `gpt-repo-mcp` cloned at `~/Documents/GitHub/gpt-repo-mcp`
- Node/npm available to launch `npm run dev` inside `gpt-repo-mcp`
- `zrok2` installed, enabled, and signed in for your zrok account
- ChatGPT Developer Mode for read+write tools, or read-only mode for read-only surfaces

The app uses repo id `workspace` for the selected folder.

## Local Development

```powershell
npm.cmd install
npm.cmd run build
npm.cmd run tauri build
```

Rust checks:

```powershell
Set-Location .\src-tauri
cargo fmt --all -- --check
cargo check
cargo test
```

## Windows Artifact

The local Windows build emits:

```text
src-tauri/target/release/bundle/msi/Secret Tunnel_0.1.0_x64_en-US.msi
src-tauri/target/release/bundle/nsis/Secret Tunnel_0.1.0_x64-setup.exe
```

## Cross-Platform Downloads

The GitHub Actions workflow builds on Windows, macOS, and Linux and uploads each platform's Tauri bundle as a downloadable workflow artifact.

## Security Notes

- Do not select a drive root, home folder, or system folder.
- Treat the public MCP URL like a temporary credential.
- Read-only mode sets `GPT_REPO_READ_ONLY_SURFACE=1`, so write-capable tools are absent from the MCP tool list.
- Read+write mode only enables file write tools in the managed `gpt-repo-mcp` config; git and validation operations stay disabled.
- zrok must be enabled locally before this app can create or serve the public share.
