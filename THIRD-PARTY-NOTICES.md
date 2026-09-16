# Third-party notices

Secret Tunnel itself is MIT licensed — see [LICENSE](./LICENSE).

The installers redistribute the components below. They keep their own licences,
and those licences, not Secret Tunnel's, govern their use and redistribution.

## gpt-repo-mcp — MIT

The MCP server that exposes the selected folder and enforces what may be read or
written. It is the component that does the actual work, and the origin of the
idea.

- Project: <https://github.com/CAHN91/gpt-repo-mcp>
- Copyright (c) 2026 Promptiva AB
- Licence: MIT — full text at
  [`app/vendor/gpt-repo-mcp/LICENSE`](./app/vendor/gpt-repo-mcp/LICENSE)

A copy is vendored in this repository with one change, documented in
[`app/vendor/gpt-repo-mcp/VENDOR.md`](./app/vendor/gpt-repo-mcp/VENDOR.md) and
offered back upstream.

### Its runtime dependencies

The bundled server ships with its production npm dependencies (93 packages at
the time of writing): predominantly MIT, with ISC, BSD-2-Clause, BSD-3-Clause
and Apache-2.0 also represented. Each package retains its own licence text in
its directory inside the installed application, under
`resources/gpt-repo-mcp/node_modules/<package>/`.

## zrok — Apache-2.0

Provides the tunnel that makes the local server reachable from ChatGPT. Built on
OpenZiti by NetFoundry. The `zrok2` executable is redistributed unmodified.

- Project: <https://github.com/openziti/zrok>
- Licence: Apache License 2.0 — <https://www.apache.org/licenses/LICENSE-2.0>
- Version bundled: recorded in `app/src-tauri/binaries/zrok-source.json`, along
  with the SHA-256 of the official release archive it was extracted from

Apache-2.0 requires that this notice and a copy of the licence accompany
redistribution, and that any modified files be marked as changed. No zrok source
or binary is modified here.

## Node.js — MIT

Runs the MCP server. The `node` executable is redistributed unmodified.

- Project: <https://nodejs.org/>
- Licence: MIT, with the additional third-party licences Node itself bundles
  (OpenSSL, ICU, zlib and others) listed in the `LICENSE` file distributed with
  the Node.js release
- Version bundled: recorded in `app/src-tauri/binaries/node-source.json`, with
  the SHA-256 of the executable

## Tauri — MIT / Apache-2.0

The desktop application framework, used as a dependency rather than
redistributed as a separate program.

- Project: <https://tauri.app/>
- Licence: dual MIT or Apache-2.0 at your option

## Rust crates

The application's Rust dependencies are resolved by Cargo and recorded in
`app/src-tauri/Cargo.lock`. They are overwhelmingly MIT or Apache-2.0; the
lockfile is the authoritative record of exactly which versions are compiled in.
