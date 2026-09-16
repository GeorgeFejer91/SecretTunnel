# Secret Tunnel

A tiny Tauri desktop app that keeps one local `gpt-repo-mcp` folder share available to
ChatGPT through a stable zrok URL.

## Licence

Secret Tunnel is released under the [MIT Licence](LICENSE).

It redistributes third-party components that keep their own licences — notably
[gpt-repo-mcp](https://github.com/CAHN91/gpt-repo-mcp) (MIT, Promptiva AB),
[zrok](https://github.com/openziti/zrok) (Apache-2.0) and
[Node.js](https://nodejs.org/) (MIT). See
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Credits

The idea, and the component that does the actual work, come from
**[gpt-repo-mcp](https://github.com/CAHN91/gpt-repo-mcp)** by **Promptiva AB** (MIT).
That project is the MCP server which exposes a folder to ChatGPT and enforces what may
be read or written. Secret Tunnel is the launcher and tunnel wrapped around it: it
starts the server, publishes it through zrok, and gives it a desktop UI.

A copy of that server is vendored in [`app/vendor/gpt-repo-mcp/`](app/vendor/gpt-repo-mcp/)
under its original MIT licence, with one change (honouring `GPT_REPO_READ_ONLY_SURFACE`)
that is offered back to the upstream project. See
[`VENDOR.md`](app/vendor/gpt-repo-mcp/VENDOR.md) for the exact provenance.

The tunnel is provided by [zrok](https://zrok.io/), built on
[OpenZiti](https://openziti.io/) by NetFoundry.

## Repository Layout

| Path | Contents |
| --- | --- |
| `For-AI/` | Project context, constraints, and AI orchestration protocols (start here). |
| `app/` | The Tauri application source (frontend, Rust backend, build scripts). |
| `app/vendor/gpt-repo-mcp/` | Vendored MCP server (MIT, Promptiva AB) — see its `VENDOR.md`. |
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