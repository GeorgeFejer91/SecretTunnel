# Secret Tunnel — AI Workflow Protocol

This protocol defines how AI agents (opencode, Codex, ChatGPT, etc.) operate inside
this repository. It exists so that AI-produced changes stay consistent, verifiable,
and reversible.

## Mandatory Read Order

Before any edit, read in this order:

1. `For-AI/CONTEXT.md` — the identity, architecture, and current status.
2. `For-AI/constraints/project-constraints.md` — the hard constraints.
3. The protocol files relevant to the task (`build-process.md`, `release-process.md`).
4. The actual files you intend to change, plus their neighbors (imports, callers).

## Edit Rules

1. **No comment sprinkling.** Follow the existing code style; the repo is written with
   intentional minimalism. Do not add explanatory comments unless asked.
2. **Prefer editing over rewriting.** Match surrounding style, naming, and patterns.
3. **Never commit secrets.** zrok enable tokens, `ZROK_ENABLE_TOKEN`, password values,
   or `.env` content must never appear in committed files or logs.
4. **Never touch ECGaming** or other repos. Only `SecretTunnel` is in scope.
5. **Keep git out of `outputs/installers/`.** Installers are locally present on disk
   but must stay gitignored (they exceed GitHub's file-size limit). Reference them,
   don't commit them.
6. **Do not hand-edit generated configs.** The managed MCP config is written by the
   app at runtime (`write_managed_mcp_config`), not by us.

## Change Protocol

1. State intent and the files involved before editing.
2. Make minimal, focused changes.
3. After editing, run the relevant checks (see `build-process.md`):
   - TypeScript/Vite: `npm.cmd run build` (in `app/`)
   - Rust: `cargo fmt --all -- --check`, `cargo check`, `cargo test` (in `app/src-tauri`)
4. Report what passed and what failed honestly. Do not claim green checks that did not
   run.
5. If a check fails, fix it before declaring the task done.

## Behavior-Sensitive Areas (Extra Care)

These files encode security/access behavior; changes here get extra scrutiny:

- `app/src-tauri/src/settings.rs` — access mode, write globs, path validation,
  managed config generation.
- `app/src-tauri/src/process.rs` — process spawning, env vars (including
  `GPT_REPO_READ_ONLY_SURFACE`), zrok share flags.
- `app/src-tauri/src/commands.rs` — UI-facing command surface.
- `app/scripts/prepare-gpt-repo-mcp.mjs` — which MCP revision gets bundled.

## Tool-Surface Rule (Important)

Read-only vs read+write mode is controlled by `GPT_REPO_READ_ONLY_SURFACE=1` set in
`spawn_mcp` (`process.rs`). When this var is set, gpt-repo-mcp exposes ONLY
`readOnlyHint === true` tools in `tools/list` — write tools are entirely absent, not
blocked at runtime. Keep this behavior when modifying the mode logic.

## Context Maintenance

- After any decision that changes how the project works, update `For-AI/CONTEXT.md`
  and/or the relevant protocol in the same change set.
- If a new constraint appears, add it to
  `For-AI/constraints/project-constraints.md`.
- Documentation drift is a bug. Keep `For-AI/` truthful to the code.