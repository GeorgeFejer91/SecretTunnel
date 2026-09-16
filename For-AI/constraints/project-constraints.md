# Secret Tunnel — Project Constraints

## Scope

1. **One repo**: Secret Tunnel (this inventory). Do not modify or touch the ECGaming
   repository or any repo other than this one.
2. **Minimal desktop launcher**: keep the app intentionally small. Do not turn it into
   a general-purpose MCP manager.

## Platform

1. Primary target: **Windows** (x64). CI also builds macOS and Linux.
2. The packaged app must not depend on user-installed `node`, `npm`, `zrok`, or a
   permissive PowerShell execution policy.
3. The app must never download executable dependencies at runtime.
4. Windows builds must work even when the PowerShell execution policy blocks
   `npm.ps1` — call `npm.cmd`.

## Security (HARD)

1. Do not expose a drive root, home folder, or system folder as workspace.
2. Write globs always deny `.git/**`, `.env`, `.env.*`, `**/*.pem`, `**/*.key`.
3. `max_bytes_per_write` must stay bounded (currently 1048576).
4. git and validation operations stay disabled in the managed config
   (`operations.enabled = false`).
5. zrok enable tokens are read once, used only for `zrok2 enable --headless`, and
   removed from the app process after a successful enable. They are never stored.
6. Never commit secrets, tokens, or `.env` values to this repo.
7. The public MCP URL is a temporary credential; treat it that way in docs.

## Read vs Write Access

1. Read-only mode sets `GPT_REPO_READ_ONLY_SURFACE=1`; write-capable tools must be
   **absent** from `tools/list`, not merely blocked at call time.
2. Read+write mode enables file-write tools ONLY; git and validation operations are
   never enabled from this repo's managed config.
3. **ChatGPT plan reality (2026-09)**: OpenAI's help center says Pro/Plus accounts get
   read/fetch-only MCP in developer mode; full write actions require Business or
   Enterprise/Edu workspaces (beta). This is host-side policy. Do not advertise write
   support on a Pro account; document the limitation instead.

## Build & Distribution

1. `prepare:runtime` must produce a self-contained runtime (Node, zrok2,
   gpt-repo-mcp bundle).
2. Release installers go to `outputs/installers/` but are gitignored because they
   exceed GitHub's 100 MB file limit; checksums/manifest are committed.
3. Every release build should record checksums (`release:checksums`).

## AI Orchestration (HARD)

1. All project constraints, goals, context, and orchestration protocols live in
   `For-AI/`. No orchestration logic lives in `app/` or `outputs/`.
2. Every AI agent must read `For-AI/CONTEXT.md` first, then the relevant protocols.
3. Update `For-AI/` whenever behavior changes; docs drift is a bug.

## Testing

1. Rust unit tests must run in `cargo test` and stay green.
2. Frontend must pass `npm.cmd run build` (TS + Vite).
3. `smoke:mcp`, `smoke:ui`, `smoke:binaries`, `smoke:layout` should pass before a
   release. `smoke:live` requires a real zrok account and is run manually/in CI on
   demand.