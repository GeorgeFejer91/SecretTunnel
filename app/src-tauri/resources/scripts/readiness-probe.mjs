#!/usr/bin/env node
// Readiness probe for Secret Tunnel. Executed by the bundled Node runtime
// from the launcher's readiness scheduler. Checks each gate and prints a
// single JSON document on stdout:
//
//   {
//     "localProcess":    { "status": "verified"|"failed"|"pending"|"unsupported", "detail": "..." },
//     "localEndpoint":   { ... },
//     "localProtocol":   { ... },
//     "publicTunnel":    { ... },
//     "publicProtocol":  { ... }
//   }
//
// A nonzero exit means the probe itself failed (missing binary, timeout,
// unroutable script); the launcher treats every gate as Failed with the
// stderr detail.

import { parseArgs } from "node:util";

const {
  values: { "local-url": localUrl, "local-mcp-url": localMcpUrl, "public-url": publicUrl, "public-mcp-url": publicMcpUrl, pids },
} = parseArgs({
  options: {
    "local-url": { type: "string" },
    "local-mcp-url": { type: "string" },
    "public-url": { type: "string" },
    "public-mcp-url": { type: "string" },
    pids: { type: "string" },
  },
});

const REQUEST_TIMEOUT_MS = 8_000;
const pidList = (pids ?? "")
  .split(",")
  .map((part) => Number(part.trim()))
  .filter((pid) => Number.isInteger(pid) && pid > 0);

const INITIALIZE_BODY = JSON.stringify({
  jsonrpc: "2.0",
  id: "readiness-probe",
  method: "initialize",
  params: {
    protocolVersion: "2025-03-26",
    capabilities: {},
    clientInfo: { name: "secret-tunnel", version: "0.1.0-readiness" },
  },
});

function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error?.code === "EPERM";
  }
}

function result(status, detail) {
  return { status, detail: String(detail ?? "") };
}

function failed(detail) {
  return result("failed", detail);
}

function unsupported(detail) {
  return result("unsupported", detail);
}

async function httpCheck(url, options) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
  try {
    return await fetch(url, { ...options, signal: controller.signal, redirect: "manual" });
  } finally {
    clearTimeout(timer);
  }
}

function mcpResultFromBody(bodyText, contentType, label) {
  if (!bodyText || bodyText.length === 0) {
    return failed(`${label}: empty response`);
  }
  // Streamable HTTP may return the initialize result as an SSE stream.
  if ((contentType ?? "").includes("text/event-stream")) {
    for (const line of bodyText.split("\n")) {
      if (!line.startsWith("data:")) continue;
      const data = line.slice(5).trim();
      if (!data) continue;
      try {
        const parsed = JSON.parse(data);
        if (parsed?.result?.serverInfo) return result("verified", `${label}: MCP initialize ok`);
      } catch {
        // ignore non-JSON SSE frames
      }
    }
    return failed(`${label}: no MCP initialize result in stream`);
  }
  try {
    const parsed = JSON.parse(bodyText);
    if (parsed?.result?.serverInfo && !parsed?.error) {
      return result("verified", `${label}: MCP initialize ok`);
    }
    if (parsed?.error) {
      return failed(`${label}: ${parsed.error.message ?? "MCP error"}`);
    }
    return failed(`${label}: unexpected JSON response`);
  } catch {
    return failed(`${label}: non-JSON response`);
  }
}

async function probeLocalProcess() {
  if (pidList.length === 0) {
    return result("pending", "no MCP process tracked yet");
  }
  const dead = pidList.filter((pid) => !alive(pid));
  if (dead.length === 0) {
    return result("verified", `MCP process alive (${pidList.join(", ")})`);
  }
  return failed(`MCP process exited (${dead.join(", ")})`);
}

async function probeEndpoint(name, url, label) {
  if (!url) return unsupported(`${label} not started`);
  let response;
  try {
    response = await httpCheck(url, { method: "GET", headers: { Accept: "application/json" } });
  } catch (error) {
    const reason = error?.name === "AbortError" ? "timeout" : error?.cause?.code ?? error?.message ?? "request failed";
    return failed(`${label}: ${reason}`);
  }
  if (!response.ok) {
    return failed(`${label}: HTTP ${response.status}`);
  }
  return result("verified", `${label}: HTTP ${response.status}`);
}

// Terminating the session is not optional housekeeping. `initialize` creates a
// real server-side session, and this probe runs on every readiness tick against
// both the local and the public URL. Left open, those sessions accumulate until
// the server reaches GPT_REPO_MAX_SESSIONS (default 100) and then answers every
// client - ChatGPT included - with "MCP session capacity reached", taking down
// the endpoint the probe exists to watch. Streamable HTTP uses DELETE with the
// session id as the termination signal.
async function terminateSession(url, sessionId) {
  if (!sessionId) return;
  try {
    await httpCheck(url, {
      method: "DELETE",
      headers: { "Mcp-Session-Id": sessionId, Accept: "application/json" },
    });
  } catch {
    // Best effort: a failed cleanup must not change the gate's verdict.
  }
}

async function probeProtocol(name, url, label) {
  if (!url) return unsupported(`${label} not started`);
  let response;
  try {
    response = await httpCheck(url, {
      method: "POST",
      headers: {
        Accept: "application/json, text/event-stream",
        "Content-Type": "application/json",
      },
      body: INITIALIZE_BODY,
    });
  } catch (error) {
    const reason = error?.name === "AbortError" ? "timeout" : error?.cause?.code ?? error?.message ?? "request failed";
    return failed(`${label}: ${reason}`);
  }
  const sessionId = response.headers.get("mcp-session-id");
  if (!response.ok) {
    await terminateSession(url, sessionId);
    return failed(`${label}: HTTP ${response.status}`);
  }
  const contentType = response.headers.get("content-type") ?? "";
  const bodyText = await response.text();
  const outcome = mcpResultFromBody(bodyText, contentType, label);
  await terminateSession(url, sessionId);
  return outcome;
}

// Health/endpoint gate hits the server's JSON health route, which also proves
// the responder is our bundled gpt-repo-mcp server rather than an unrelated
// process squatting on the port.
async function probeLocalEndpoint() {
  if (!localUrl) return unsupported("local endpoint not started");
  let response;
  try {
    response = await httpCheck(`${localUrl}/health`, { method: "GET", headers: { Accept: "application/json" } });
  } catch (error) {
    const reason = error?.name === "AbortError" ? "timeout" : error?.cause?.code ?? error?.message ?? "request failed";
    return failed(`local endpoint: ${reason}`);
  }
  if (!response.ok) {
    return failed(`local endpoint: HTTP ${response.status}`);
  }
  const bodyText = await response.text();
  try {
    const parsed = JSON.parse(bodyText);
    if (parsed?.ok === true && parsed?.name === "gpt-repo-mcp") {
      return result("verified", "local endpoint: health ok");
    }
    return failed("local endpoint: unexpected health payload");
  } catch {
    return failed("local endpoint: non-JSON health response");
  }
}

const gates = {
  localProcess: await probeLocalProcess(),
  localEndpoint: await probeLocalEndpoint(),
  localProtocol: await probeProtocol("localProtocol", localMcpUrl, "local protocol"),
  publicTunnel: await probeEndpoint("publicTunnel", publicUrl ? `${publicUrl}/health` : null, "public tunnel"),
  publicProtocol: await probeProtocol("publicProtocol", publicMcpUrl, "public protocol"),
};

process.stdout.write(`${JSON.stringify(gates)}\n`);