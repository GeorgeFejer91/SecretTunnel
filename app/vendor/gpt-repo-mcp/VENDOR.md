# Vendored: gpt-repo-mcp

This directory is **not original work**. It is a vendored copy of the
`gpt-repo-mcp` MCP server, which is the component that actually exposes a folder
to ChatGPT. Secret Tunnel is the launcher and tunnel around it.

| | |
|---|---|
| Original project | <https://github.com/CAHN91/gpt-repo-mcp> |
| Author | Promptiva AB |
| Licence | MIT — see [LICENSE](./LICENSE) |
| Upstream commit | `986f2135f00959f8e0d214ed8d173a7054f4cea1` |
| Fork carrying the change below | <https://github.com/GeorgeFejer91/gpt-repo-mcp/tree/read-only-tool-surface> (`93df9e2`) |

The MIT licence and the original copyright notice are retained in `LICENSE` and
must stay with any copy or redistribution of this directory.

## Why it is vendored

The build previously cloned upstream at build time. That made the shipped
installer depend on whatever upstream `HEAD` happened to be, and it silently
diverged from the binary that had actually been tested: the working, verified
build on the developer's machine contained an uncommitted local fix that no
clean clone would ever produce. Vendoring makes the bundled server exactly the
reviewed source, recorded in git alongside the launcher that ships it.

## The one modification

`src/register.ts` honours `GPT_REPO_READ_ONLY_SURFACE=1` by filtering the tool
catalogue to tools whose annotations declare `readOnlyHint`.

Without it, "Read" mode still registered every tool, so `tools/list` advertised
21 write-capable tools — including `repo_write_file`, `repo_write_commit` and
`repo_apply_patchset` — to a client that had been told it had read-only access.
Secret Tunnel's Read mode depends entirely on this filter being applied.

The change is pushed to the fork above so it can be offered upstream as a pull
request; it is not a permanent divergence.

## Updating this copy

1. Pull the change into the fork and rebase it on the newer upstream commit.
2. Re-copy `src/`, `package.json`, `package-lock.json`, `tsconfig.json` and
   `LICENSE` here.
3. Update `upstreamCommit` and `forkCommit` in `VENDOR.json`; they are recorded
   into the runtime manifest that ships inside the installer, so they must
   describe the code actually bundled.
4. Run `npm run prepare:runtime` and `npm run smoke:mcp` from `app/`.

Tests, docs, scripts and CI config are deliberately not vendored — only what is
needed to build `dist/server.js`. Run the full suite in the fork instead.
