# Secret Tunnel — File Structure Protocol

## Repository Root

```text
SecretTunnel/
├── For-AI/                    # AI orchestration input (this layer)
│   ├── CONTEXT.md             # Single source of truth for project state
│   ├── PROTOCOLS/             # File/process/AI protocols
│   └── constraints/           # Hard constraints and requirements
├── app/                       # The Tauri application source
├── outputs/                   # Orchestration outputs
│   ├── installers/            # Built MSI/EXE (NOT committed to git)
│   └── checksums/             # SHA256SUMS.txt + artifact-manifest.json (committed)
├── .github/                   # CI workflow copies (also live inside app/)
├── README.md
└── .gitignore
```

## The For-AI Layer

Everything an AI needs to work safely and correctly on this project lives under
`For-AI/`. **Nothing under For-AI/ is compiled, bundled, or shipped.** It is pure
orchestration documentation.

| File | Purpose |
| --- | --- |
| `CONTEXT.md` | Project identity, architecture, current status, key facts. Read first. |
| `PROTOCOLS/file-structure.md` | This file: how the tree is organized. |
| `PROTOCOLS/build-process.md` | Exact commands to prepare, test, check, and build. |
| `PROTOCOLS/ai-workflow.md` | How AI agents should operate: read order, edit rules, verification. |
| `PROTOCOLS/release-process.md` | Packaging, checksums, distribution, and release notes. |
| `constraints/project-constraints.md` | Hard constraints: scope, platform, security, write-access reality. |

## The app Layer

`app/` is the original `chatgpt-local-mcp-launcher` repository content. Standard
layout (kept intact):

```text
app/
├── src/                    # Tauri frontend (TypeScript, Vite)
├── src-tauri/              # Rust backend
│   ├── src/                # commands.rs, process.rs, settings.rs, error.rs, lib.rs, main.rs
│   ├── icons/              # App icons
│   ├── capabilities/       # Tauri capability grants
│   ├── binaries/           # node + zrok2 (gitignored, filled by prepare scripts)
│   ├── resources/          # gpt-repo-mcp bundle (gitignored, filled by prepare scripts)
│   └── tauri.conf.json     # Product name, windows, bundle resources
├── scripts/                # prepare-*, smoke-*, clean-*, release scripts
├── public/                 # Static SVG assets (cloud, flow, title)
├── .github/workflows/build.yml
├── package.json            # npm scripts (see build protocol)
└── README.md
```

## The outputs Layer

- `outputs/installers/` — final MSI/EXE. These are **hundreds of MB** and are
  **excluded from git** (`.gitignore`). Distribute them as GitHub Release assets.
- `outputs/checksums/` — `SHA256SUMS.txt` and `artifact-manifest.json` reference a
  previous release's artifact inventory. These **are** committed so provenance is
  documented.

## Rules for File Placement

1. New source code goes in `app/`, following existing convention (`.ts` with Vite,
   Rust in `src-tauri/`).
2. New orchestration/context docs go in `For-AI/`.
3. New build artifacts (installers) go in `outputs/installers/` and stay gitignored.
4. New checksum/manifest outputs replace files in `outputs/checksums/` and are committed.
5. Never commit: `node_modules/`, `app/dist/`, `app/src-tauri/target/`,
   `app/src-tauri/gen/`, `app/src-tauri/binaries/`, `app/src-tauri/resources/gpt-repo-mcp/`,
   `outputs/installers/`.
6. Do not edit `For-AI/PROTOCOLS/` files casually; they encode decisions. Update them
   deliberately in the same change that changes the behavior they describe.