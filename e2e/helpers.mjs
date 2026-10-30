// E2E harness for the swiss gateway (user constraints):
// SWISS_HOME = ~/swiss-e2e, SWISS_PORT = 18999 (dedicated e2e port; 19999 is production,
// 19998 is live-verify). Auth goes through the `swiss api` CLI, which signs with the
// home's CLI key (the session file is sealed; no key is ever printed here).
// The instance is killed only when the port's owning PID is our own live child -
// the harness never kills a stranger's process. Every module file boots
// a FRESH home so a leftover config can never leak state between modules.
import { spawn, spawnSync } from "node:child_process";
import { existsSync, rmSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

import { checkHome, checkPort, preflight } from "./safety.mjs";

export const PORT = process.env.SWISS_E2E_PORT || "18999";
export const HOME = process.env.SWISS_E2E_HOME || join(homedir(), "swiss-e2e");
export const BASE = `http://127.0.0.1:${PORT}`;
// The default exe is the isolated target-test build: the deploy source
// (target/release) and the running production binary (bin) are refused by the
// safety gate below, and a served exe is a locked file for the next link.
export const EXE = process.env.SWISS_E2E_EXE || join(process.cwd(), "target-test", "release", "swiss.exe");

// The safety context the pure checks need: production home candidates, the user
// profile, the workspace, and the live-verify snapshot marker. Resolved from the
// operator's world BEFORE any child env is derived, so our own overrides cannot
// fool the checks.
export function safetyContext() {
  const prod = [join(homedir(), ".swiss")];
  if (process.env.SWISS_HOME) prod.push(process.env.SWISS_HOME);
  return {
    prodHomes: prod,
    userProfile: homedir(),
    workspaceRoot: process.cwd(),
    protectedMarkers: ["swiss-test-home"],
  };
}

// Fail fast, at import time, on a dangerous overlay: a port that is not a
// sanctioned e2e port (19999 is production and is never a test target), a home
// this harness must never wipe, or the production deploy-source exe. Nothing
// boots, stops or deletes until the resolved overlay passes these checks.
const GATE = preflight({ port: PORT, home: HOME, exe: EXE }, safetyContext());
if (!GATE.ok) {
  throw new Error("e2e overlay refused: " + GATE.reasons.join("; "));
}

// Exported so a test can add a KNOWN-FAKE mock env value explicitly (a fake
// provider key) after import - never a real credential.
export const ENV = { ...process.env, SWISS_HOME: HOME, SWISS_PORT: PORT };

let child = null;

export function portPid() {
  // The only safe way to find the gateway: the PID LISTENING on the e2e port.
  const out = spawnSync("netstat", ["-ano", "-p", "tcp"], { encoding: "utf8" }).stdout || "";
  for (const line of out.split("\n")) {
    if (line.includes(`:${PORT}`) && /LISTENING/.test(line)) {
      const pid = Number(line.trim().split(/\s+/).pop());
      if (Number.isInteger(pid) && pid > 0) return pid;
    }
  }
  return null;
}

function killPid(pid) {
  if (pid == null) return;
  spawnSync("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore" });
}

export async function boot({ fresh = true } = {}) {
  // The harness only ever kills a gateway it spawned itself: a preexisting
  // listener on the e2e port - whoever owns it - is a hard refusal before any
  // wipe or spawn, never an auto-kill. Modules stop their own instance first.
  const stale = portPid();
  if (stale != null) {
    throw new Error(
      "e2e port " + PORT + " already has a listener (pid " + stale + "): refusing to boot; stop it by hand if it is yours",
    );
  }
  // SWISS_E2E_KEEP_HOME=1 keeps the e2e home between runs (README): a debugging
  // session's right. The default stays a fresh wipe so modules cannot leak state.
  if (fresh && !process.env.SWISS_E2E_KEEP_HOME && existsSync(HOME)) {
    // Every delete verifies its resolved absolute target first (tool policy): the
    // home must be a dedicated e2e home, never production, a live-verify
    // snapshot, or a system directory.
    const verdict = checkHome(HOME, safetyContext());
    if (!verdict.ok) throw new Error("refusing to wipe " + HOME + ": " + verdict.reasons.join("; "));
    rmSync(HOME, { recursive: true, force: true });
  }
  child = spawn(EXE, ["serve"], { env: ENV, stdio: "ignore", detached: false });
  const deadline = Date.now() + 30_000;
  for (;;) {
    try {
      const r = await fetch(`${BASE}/health`);
      if (r.ok) return;
    } catch {}
    if (Date.now() > deadline) throw new Error(`gateway did not answer /health on ${BASE} within 30s`);
    if (child.exitCode !== null) throw new Error(`swiss serve exited early with code ${child.exitCode}`);
    await new Promise((r) => setTimeout(r, 250));
  }
}

export function stop() {
  const verdict = checkPort(PORT);
  if (!verdict.ok) throw new Error("refusing to stop on port " + PORT + ": " + verdict.reasons.join("; "));
  // Kill only what we spawned: the port's owner may be taskkilled only when it
  // is exactly our own still-live child (no exit code, no signal code, pid
  // match). A foreign owner is reported and left alone, an exited child is
  // never signaled, and the tracked handle is cleared every time.
  const pid = portPid();
  if (child && child.exitCode === null && child.signalCode === null) {
    if (pid != null && pid === child.pid) {
      killPid(pid);
    } else if (pid != null) {
      console.warn("e2e stop: pid " + pid + " owns port " + PORT + ", not our live child - leaving it alone");
    }
    try { child.kill(); } catch {}
  } else if (pid != null) {
    console.warn("e2e stop: pid " + pid + " owns port " + PORT + " with no live child of ours - leaving it alone");
  }
  child = null;
}

/** Authed call through the CLI signer. { status: 0 } means a 2xx answer. */
export function api(method, path, body) {
  const args = ["api", method, path];
  if (body !== undefined) args.push(typeof body === "string" ? body : JSON.stringify(body));
  const r = spawnSync(EXE, args, { env: ENV, encoding: "utf8", timeout: 60_000 });
  if (r.error) throw r.error;
  let parsed = null;
  try { parsed = JSON.parse(r.stdout); } catch { parsed = r.stdout; }
  return { status: r.status, stdout: r.stdout, stderr: r.stderr || "", body: parsed };
}

/** Idempotently enable a plugin row; tolerates already-enabled and unknown ids. */
export function ensurePlugin(id) {
  return api("POST", `/api/plugins/${id}/enable`);
}

/** Poll an authed GET until it answers 2xx (plugins come up asynchronously). */
export async function waitReady(path, { timeoutMs = 15_000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const r = api("GET", path);
    if (r.status === 0) return r;
    if (Date.now() > deadline) return r; // let the caller's assert report it
    await new Promise((res) => setTimeout(res, 400));
  }
}

/**
 * One JSON-RPC frame to an MCP endpoint (`/mcp/<name>`), as an AI client sends it:
 * POST with the bearer, answered as JSON or as an SSE stream. Answers
 * `{status, message}` - `message` is the JSON-RPC reply matching the frame's id
 * (null for a refused request). The bearer is a secret: it never reaches an
 * error message or the console from here.
 */
export async function mcp(name, bearer, frame) {
  const headers = { "content-type": "application/json", accept: "application/json, text/event-stream" };
  if (bearer) headers.authorization = "Bearer " + bearer;
  const r = await fetch(`${BASE}/mcp/${name}`, { method: "POST", headers, body: JSON.stringify(frame) });
  const text = await r.text();
  const frames = (r.headers.get("content-type") || "").includes("text/event-stream")
    ? text.split("\n").filter((l) => l.startsWith("data:")).map((l) => l.slice(5).trim())
    : [text];
  let message = null;
  for (const raw of frames) {
    try {
      const parsed = JSON.parse(raw);
      if (parsed && parsed.id === frame.id) message = parsed;
    } catch {}
  }
  return { status: r.status, message };
}

/** A tools/call result's text content, joined. */
export function toolText(message) {
  const content = message?.result?.content ?? [];
  return content.filter((c) => c.type === "text").map((c) => c.text).join("\n");
}

export async function get(path) { // unauthenticated raw fetch (panel assets, /health)
  const r = await fetch(`${BASE}${path}`);
  return { status: r.status, headers: r.headers, text: await r.text() };
}

export async function withGateway(fn, { plugin = null, ready = null } = {}) {
  // A boot that fails mid-way (spawned but never healthy) must not leave the
  // half-started gateway behind: stop() runs on boot's failure path too.
  await boot({ fresh: true }).catch((err) => {
    stop();
    throw err;
  });
  try {
    if (plugin) ensurePlugin(plugin);
    return await (ready ? waitReady(ready).then(() => fn()) : fn());
  } finally { stop(); }
}
