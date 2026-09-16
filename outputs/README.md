# outputs — Orchestration Outputs

This folder holds the **products** of building Secret Tunnel.

## installers/

- `Secret Tunnel_0.1.0_x64_en-US.msi` (Windows, offline WebView2 embedded)
- `Secret Tunnel_0.1.0_x64-setup.exe` (Windows NSIS setup)

These files are **gitignored**: they are several hundred MB and exceed GitHub's
per-file limit. They exist locally for installation and are distributed as **GitHub
Release assets**. Rebuild them anytime with `npm.cmd run tauri build` (see
`../For-AI/PROTOCOLS/build-process.md`).

## checksums/

- `SHA256SUMS.txt` — SHA-256 of each tracked artifact in the previous release bundle.
- `artifact-manifest.json` — deterministic inventory (path, bytes, sha256).

These two files ARE committed so artifact provenance stays documented.

## Files

- `secret-tunnel-flow-preview.gif` — marketing/overview animation for the project.