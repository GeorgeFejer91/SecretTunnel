# Secret Tunnel

A tiny Tauri desktop app that keeps one local `gpt-repo-mcp` folder share available to
ChatGPT through a stable zrok URL.

## Repository Layout

| Path | Contents |
| --- | --- |
| `For-AI/` | Project context, constraints, and AI orchestration protocols (start here). |
| `app/` | The Tauri application source (frontend, Rust backend, build scripts). |
| `outputs/` | Build outputs: installers (kept locally, gitignored) and committed checksums. |

## Quick Facts

- Version: `0.1.0`
- Local MCP server: `127.0.0.1:8787`, tunneled via bundled `zrok2`
- ChatGPT URL: `https://<name>.shares.zrok.io/t/<token>/mcp`
- Requires a one-time zrok enable token on first run
- Read-only mode (`GPT_REPO_READ_ONLY_SURFACE=1`) removes write tools entirely
- Read+write mode only enables file-write tools; git/validation ops stay off

## Build & Develop

See `For-AI/PROTOCOLS/build-process.md` for commands (run from `app/`).

```powershell
npm.cmd install
npm.cmd run prepare:runtime
npm.cmd run build
npm.cmd run tauri build
```

## Documentation

- Detailed context: `For-AI/CONTEXT.md`
- Constraints: `For-AI/constraints/project-constraints.md`
- Protocols: `For-AI/PROTOCOLS/`

## Notes on ChatGPT Write Access

ChatGPT Pro currently provides read/fetch-only MCP tools in developer mode; full MCP
write actions are rolling out to Business and Enterprise/Edu workspaces. See
`For-AI/constraints/project-constraints.md`.