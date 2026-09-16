# Secret Tunnel — Build Performance Protocol

Where build time actually goes in this project, what has already been done about
it, and the rules that keep it from regressing. Every number here was measured
on this repository, not estimated.

## Measured baseline

From the `v0.1.0` tag build, macOS Apple Silicon, cold runner, no cache:

| Step | Time | Share |
| --- | --- | --- |
| `npm run tauri build` (release compile + bundle) | 330s | 58% |
| `Rust checks` (`cargo check` + `cargo test`, debug) | 143s | 25% |
| `npm run prepare:runtime` (zrok download, MCP build) | 48s | 8% |
| Everything else (checkout, npm ci, smokes, upload) | ~40s | 7% |
| **Total** | **~9m 33s** | |

**About 83% of a job is Rust compilation**, and it happens twice: once in debug
for the checks and tests, then again from scratch in release with LTO for the
bundle. The two profiles share no artifacts.

Multiply by three platforms and this is the whole cost of a build.

## What is already in place

### Dependency caching

`Swatinem/rust-cache` persists the cargo registry and `target` directory,
**keyed per platform** (`key: ${{ matrix.artifact }}`) so the three jobs never
share a cache, with `cache-all-crates: true` so the release profile benefits
too. `setup-node`'s npm cache covers **both** lockfiles — the launcher's and the
vendored MCP server's, since `prepare:runtime` installs and builds that one too.

Expect the large win on the *second* and later runs. A cold cache still pays
close to the baseline above.

### Superseding redundant runs

```yaml
concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: ${{ !startsWith(github.ref, 'refs/tags/') }}
```

Without this, several pushes in quick succession each start a full
three-platform matrix and they compete for the limited free-tier macOS runners,
which makes the build someone is actually waiting on the slowest in the queue.
On the first day this repo had four concurrent runs and twelve jobs building
commits nobody cared about any more.

**Tag builds are deliberately exempt.** They produce the release artifacts, so
cancelling one because a later commit landed would leave a published tag with no
installers behind it.

## Rules

1. **Measure before optimising.** `gh run view <id> --json jobs` gives per-step
   durations. The table above came from it. Guessing at what is slow in a build
   with a 250 MB bundled runtime is a good way to optimise the wrong 7%.
2. **Never cache a verified binary.** zrok is downloaded and checked against a
   pinned SHA-256 on every run. Saving 20 seconds is not worth the possibility
   of shipping an unverified executable to users.
3. **Keep the warning count at zero.** A build with 26 warnings is a build where
   nobody notices the 27th. See the section below.
4. **Do not add a platform without evidence.** Linux was dropped from the matrix
   rather than left declared, because it had never been built or run. Declared
   support with no CI behind it is a claim, not a fact.
5. **Do not shortcut provenance to save time.** Attaching artifacts built from a
   different commit to a tag breaks the one guarantee a release makes. If a
   release is slow, wait for it.

## Warnings are a build-quality signal

The crate compiles with **zero warnings**, and it should stay that way. Reaching
zero meant treating three different kinds of warning differently, and the
distinction matters more than the count:

- **Real defects** — a redundant `mut`, an unused import, a dead test helper, a
  struct using camelCase field names where `#[serde(rename_all)]` belongs.
  Fixed properly.
- **Cross-platform bugs** — the kind that only appear on a platform you do not
  develop on. A test asserting `folder_unavailable` for `C:\nonexistent\...`
  passed on Windows and failed on macOS, where that string is a *relative* path.
- **Deliberate, load-bearing "dead" code** — annotated with `#[allow(dead_code)]`
  **and a comment explaining why**, never silenced blindly.

The third category contains a trap worth remembering. `AppState::profile_lock`
is never read, because it is an RAII guard: dropping a `ProfileLock` releases the
OS-level exclusive lock. Deleting that field to satisfy the compiler would have
released the per-profile lock while the app runs and let a second instance start
against the same profile. **A dead-code warning is a question, not an
instruction.** Ask what holds the value before removing it.

## If more speed is needed

Roughly in order of value for effort:

1. **Verify the cache is hitting.** Look for a restored-cache line in the job
   log. A key that changes every run is a cache that never helps.
2. **Stop compiling twice.** `cargo test --release` would share artifacts with
   `tauri build` at the cost of slower test compilation. Worth measuring, not
   assuming.
3. **`cargo nextest`** for faster test execution — though at ~1.1s locally, test
   *execution* is not the bottleneck here; test *compilation* is.
4. **Split checks from packaging.** Run `fmt`/`check`/`test` on one platform for
   pull requests, and the full three-platform bundle only on `main` and tags.
   This is the biggest structural win available and the obvious next step if
   build time becomes painful again.
5. **Trim the bundle.** The installers are 257 MB (Windows) and ~80 MB (macOS),
   dominated by bundled Node and zrok. The unfinished online installer was an
   attempt at this; it is not a build-speed problem, it is a download-size one.
