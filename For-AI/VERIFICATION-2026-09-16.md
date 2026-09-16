# Verification record — 2026-09-16

Windows 11 Home 22631, release build, installed to `D:\Users\<user>\AppData\Local\Secret Tunnel`.
Every result below was observed on this machine; nothing here is inferred from a passing unit test.

## Starting point

`0b20bdd` did not compile, and the installed app opened a window and started nothing.

## Defects found and fixed

### 1. The crate did not build (`9d7f40f`)

- `readiness.rs` — `ProbeEntry::kind` was `&'static str` on a struct deriving `Deserialize`,
  which cannot satisfy an arbitrary `'de` lifetime. Now `String`.
- `process.rs` — the readiness context provider is a `move` closure, so it consumed the probe
  script and node paths that `real_probe_fn` then used again. The closure gets its own copies.
- `download_helper.rs` — the online-installer helper imports symbols that do not exist. It is
  excluded behind an off-by-default `online-installer` feature. **The online installer is still
  unimplemented; excluding it did not finish it.**

### 2. A hand-edited `settings.json` stopped the app dead (`9d7f40f`)

The production settings file was not valid JSON: a UTF-8 BOM (`ef bb bf`) plus Windows paths
written with single backslashes, where `\U` is an invalid escape. Every load failed, so no
services started, and `lib.rs` discarded the error with `let _ =` so nothing was shown anywhere.

The loader now strips a BOM and doubles lone separator backslashes inside string literals, but
only after a normal parse has already failed, so valid documents are never reinterpreted. A
recovered file is rewritten once through the write-capable path. **Recovery preserves `zrokName`
and `publicPathToken`, so the stable public URL does not change.** Unreadable content reports
`settings_unreadable` and the file is left alone rather than reset — it is the only copy of that
URL. Launch failures are now logged as `Auto-start failed:`, which the UI already surfaces.

### 3. The readiness probe took the endpoint down (`332d572`)

`probeProtocol` called MCP `initialize` against both the local and the public URL on every tick
and never terminated the sessions. The bundled server caps concurrent sessions at
`GPT_REPO_MAX_SESSIONS` (default 100, 30 minute idle TTL), so two leaked sessions per tick
exhausted the pool. After that the server answered **every** caller with:

```
HTTP 503  {"code":-32001,"message":"MCP session capacity reached"}
```

This is what made ChatGPT's "Error creating connector" fail. Observed live: the endpoint served
normally at startup and returned 503 to both the public URL and `127.0.0.1` about nine minutes
later, while the app still displayed "Running" and the MCP process was healthy and listening.

The probe now captures `mcp-session-id` and sends the Streamable HTTP `DELETE` termination on
both the success and the non-OK path.

### 4. `tasklist` exit codes are not a liveness signal — this wedged the app

`process_alive()` trusted `tasklist`'s exit status, with a comment claiming it "returns exit code
0 when the process exists, 1 when it does not". Measured on this machine:

```
tasklist /FI "PID eq 2624"    -> "secret-tunnel.exe","2624",...     exit 0
tasklist /FI "PID eq 999999"  -> INFO: No tasks are running ...     exit 0
```

It exits **0 either way**. So `process_alive()` reported every pid as alive forever,
`confirmed_exited()` could never return true, every cleanup was "unconfirmed", and the lifecycle
landed in `CleanupFailed` — which blocks all replacements until an explicit Stop and which the
supervisor skips. The app was left with no MCP server, no tunnel, and no visible reason.

This is why a **first start worked but changing the folder wedged the app**: the first start has
nothing to retire, so it never exercises the broken confirmation.

Liveness is now parsed from the CSV row instead of the exit code.

### 5. Changing the folder rebuilt the whole tunnel

Reconfigure ran the same path as Start, which retires *both* children. Only the MCP server reads
the workspace configuration; the zrok share forwards to a fixed local port and knows nothing
about it. Tearing it down drops every connected client and re-registers the share for no reason.

`LifecycleEndpoint` gained `reconfigure_services`, defaulting to a full restart. `ServiceEndpoint`
overrides it: when the zrok name is unchanged and a tunnel child is live, only the MCP child is
replaced. **The public URL stays valid across a folder change.**

## What was verified live

| Check | Result |
|---|---|
| `cargo test` | 48 passed, 0 failed (stable across 4 consecutive runs) |
| Release build + NSIS bundle | exit 0 |
| Corrupt settings recovery | byte-for-byte copy of the real file recovered, identity unchanged |
| Local MCP stack | `initialize`, `tools/list` (46 tools), sentinel file read back |
| Public MCP over real TLS | `initialize` + `tools/list` + sentinel through `*.shares.zrok.io` |
| Wrong path token | HTTP 404 |
| Write in read-only mode | refused |
| Graceful close | app exits, both children reaped, port released |
| Endpoint stability after probe fix | 12/12 samples HTTP 200 over ~6 min |
| Tunnel survives reconfigure | zrok pid identical before and after |

## Still open

1. **Status is not truthful.** `Coordinator::effective_state()` returns stored state, not an
   observed one, and `StatusDto.cleanupBlockedReason` is never read by the frontend. A dead
   endpoint still displays "Running". This hid defects 3 and 4 for hours.
2. **Shares leak on exit.** `stop_services` terminates the zrok child but never deletes the share,
   so the URL 502s between runs until the next start clears it.
3. **End-to-end folder switch is not yet verified through the GUI.** The narrow reconfigure path
   is covered by `reconfigure_takes_the_narrow_path_not_a_full_restart`, and the tunnel process
   was observed surviving, but a full click-through (switch folder, then read the *new* folder's
   sentinel through the unchanged public URL) has not been completed.
4. Coordinator and readiness scheduler are still not on the production start path.
5. Online installer, `prepare-release.mjs`, `installer-online.nsi` remain unimplemented sketches.

## Regression tests added

```text
settings::recovers_hand_edited_settings_without_changing_identity
settings::repaired_settings_are_rewritten_as_valid_json
settings::valid_settings_are_not_repaired
settings::unrecoverable_settings_report_an_error_instead_of_resetting
process::tasklist_no_match_line_is_not_a_live_process
process::process_liveness_tracks_a_real_child
lifecycle::reconfigure_takes_the_narrow_path_not_a_full_restart
```

## Note for future agents

Two of the five defects above were caused by trusting a plausible-sounding assumption about an
external tool rather than measuring it: that `tasklist` signals liveness through its exit code,
and that opening an MCP session has no cost. Both produced silent, delayed failures that unit
tests could not have caught. Measure the tool's actual behaviour on the machine.
