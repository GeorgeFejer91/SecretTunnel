# Secret Tunnel — Project Context

> This is the **single source of truth** for what Secret Tunnel is, why it exists, and its
> current state. AI agents should read this file first before touching anything in the repo.

## What It Is

Secret Tunnel is a minimal **Tauri desktop app** (`app/`) that keeps one local
`gpt-repo-mcp` folder share available to ChatGPT through a stable **zrok** tunnel URL.

It solves these problems for the owner:

1. ChatGPT cannot reach a local stdio MCP server directly; it needs a public HTTPS
   endpoint speaking SSE or Streamable HTTP.
2. Surfaces disappear when a terminal is closed. Secret Tunnel makes the tunnel
   persistent and manageable from a small desktop UI.
3. The same folder share is reusable across conversations because the zrok name and
   MCP path token are stable between launches.

## How It Works

When started, the app:

1. Writes a **managed** `gpt-repo-mcp` config into the app config directory
   (`gpt-repo-mcp.config.json`). It never edits `gpt-repo-mcp/config.local.json`.
2. Launches bundled Node running bundled `gpt-repo-mcp/dist/server.js` on
   `127.0.0.1:8787`.
3. Launches `zrok2 share public http://127.0.0.1:8787 -n public:<name> --headless`.

The ChatGPT-facing URL is:

```text
https://<zrok-name>.shares.zrok.io/t/<fixed-token>/mcp
```

The zrok name and MCP path token are stored locally so the address stays stable.

## Repo Layout (Concept)

- `For-AI/` — project constraints, goals, context, and all file/AI orchestration
  protocols. This is the **input** to AI-driven work.
- `app/` — the Tauri application source (frontend + Rust backend + build scripts).
- `outputs/` — the **outputs** of orchestration: installers, checksums, previews.
- Everything else at the repo root — README, git config, CI — is housekeeping.

## Key Configuration Surface (app)

Source of truth: `app/src-tauri/src/settings.rs`, `commands.rs`, `process.rs`.

- **Access modes**: `read` (default) sets `GPT_REPO_READ_ONLY_SURFACE=1` so
  write-capable tools are absent from the MCP tool list; `read_write` enables file
  writes only (git and validate operations stay disabled).
- **Managed MCP config** (`write_managed_mcp_config` in `settings.rs`) sets `repo_id=workspace`,
  `allow_non_git=true`, write globs allowing everything except `.git/**`, `.env*`,
  `**/*.pem`, `**/*.key`, `max_bytes_per_write=1048576`. `operations.enabled=false`.
- **Limits**: `max_files=50`, `max_bytes_per_file=128000`, `max_total_bytes=750000`.
- **Stable URL**: `https://<name>.shares.zrok.io/t/<token>/mcp` (see `mcp_url`; the zrok v2 SaaS public frontend is `shares.zrok.io`, plural).
- **Default gpt-repo-mcp path**: `%USERPROFILE%\Documents\GitHub\gpt-repo-mcp`.

## External Dependencies (bundled at build time, not in git)

- `node.exe` / `node` — bundled by `scripts/prepare-node.mjs`.
- `zrok2` — bundled by `scripts/prepare-zrok.mjs`.
- `gpt-repo-mcp` — bundled into `src-tauri/resources/gpt-repo-mcp/` by
  `scripts/prepare-gpt-repo-mcp.mjs`. The `app/.gitignore` excludes these from git.
- Required at runtime before first connect: a **zrok account enable token**
  (pasted once, or via `SECRET_TUNNEL_ZROK_ENABLE_TOKEN` / `ZROK_ENABLE_TOKEN`).

## Important: Write Access and ChatGPT Plans

OpenAI's help center states Pro/Plus accounts get **read/fetch-only** MCP payloads in
developer mode today; **full MCP write actions** are rolling out to Business and
Enterprise/Edu workspaces. This is a host-side policy, not something the app can
bypass. See `For-AI/constraints/project-constraints.md`.

## Current Status (2026-09-16)

- Version `0.1.0`, identifier `com.georgefejer.chatgpt-local-mcp-launcher`.
- Windows release installers built:
  - `outputs/installers/Secret Tunnel_0.1.0_x64_en-US.msi`
  - `outputs/installers/Secret Tunnel_0.1.0_x64-setup.exe`
- SHA-256 checksums in `outputs/checksums/`.
- Cross-platform CI exists in `app/.github/workflows/build.yml` (Windows/macOS/Linux).
- The owner's ChatGPT account is **Pro**, so the live tunnel must run in **read-only**
  mode for use inside ChatGPT. Write experiments should target the API/CLI or a
  Business workspace.

## Owner

George Fejer (GitHub: `GeorgeFejer91`).