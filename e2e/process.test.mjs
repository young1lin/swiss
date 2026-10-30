import { api, HOME, waitReady, withGateway } from "./helpers.mjs";
import { sleep } from "./util.mjs";
import assert from "node:assert/strict";

const APP = "e2e-sleeper";
const MARK = "e2e-app-up";

/** Poll the app's row until its action settles (no `busy`) and `done(row)` holds. */
async function settle(done, what, timeoutMs = 40_000) {
  const deadline = Date.now() + timeoutMs;
  let row = null;
  for (;;) {
    const r = api("GET", "/api/process");
    row = (r.body.apps ?? []).find((a) => a.name === APP) ?? null;
    if (row && !row.busy && done(row)) return row;
    if (Date.now() > deadline) assert.fail(what + " did not settle: " + JSON.stringify(row));
    await sleep(250);
  }
}

/** Whether a pid is alive, asked of the OS directly - never of the gateway. */
function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    return err.code === "EPERM";
  }
}

await withGateway(async () => {
  const first = await waitReady("/api/process");
  assert.equal(first.status, 0, "process rows: " + first.stderr);
  assert.ok(first.body.gateway && Number.isInteger(first.body.gateway.pid), "gateway facts ride along");
  assert.ok(Array.isArray(first.body.apps), "apps list shape");
  for (const path of ["/api/process/children", "/api/process/history"]) {
    const r = api("GET", path);
    assert.equal(r.status, 0, path + ": " + r.stderr);
    const list = Array.isArray(r.body) ? r.body : Object.values(r.body).find(Array.isArray);
    assert.ok(Array.isArray(list), path + " returns a list");
  }
  // Unknown app actions fail honestly (404), never a silent 200.
  {
    const r = api("POST", "/api/process/apps/does-not-exist/start");
    assert.notEqual(r.status, 0, "unknown app start must fail");
    assert.match(String(r.stdout) + String(r.stderr), /404|not found|unknown/i);
  }

  // The golden path (SPEC §process.apps): a declared app with no ports is
  // started, tracked by its pid, its output captured, stopped by a hard kill,
  // and every outcome lands in the history. The app is this very node binary
  // (typed argv, no shell), working in the e2e home.
  {
    const app = {
      name: APP,
      dir: HOME,
      start: { program: process.execPath, args: ["-e", `console.log('${MARK}');setInterval(()=>{},1000)`] },
    };
    const r = api("PUT", "/api/plugins/process/config", { config: { apps: [app] } });
    assert.equal(r.status, 0, "save the app: " + r.stdout + r.stderr);
  }
  {
    // Saving config never runs anything: nothing is tracked yet.
    const row = await settle((row) => true, "the declared row");
    assert.equal(row.status, "unknown", "a declared app nobody started is unknown: " + JSON.stringify(row));
    assert.equal(row.stopIsKill, true, "no stop command means Stop is a hard kill");
  }
  let pid = null;
  {
    const r = api("POST", `/api/process/apps/${APP}/start`);
    assert.equal(r.status, 0, "start is accepted at once: " + r.stdout + r.stderr);
    const row = await settle((row) => row.lastAction?.action === "start", "the start");
    assert.equal(row.lastAction.ok, true, "the start succeeded: " + JSON.stringify(row.lastAction));
    assert.equal(row.status, "running", "a live tracked pid reads running: " + JSON.stringify(row));
    pid = row.pid;
    assert.ok(Number.isInteger(pid) && alive(pid), "the row's pid is a live process");
  }
  {
    // The capture file holds the start header and the app's own stdout.
    let lines = [];
    const deadline = Date.now() + 10_000;
    while (!lines.some((l) => l.includes(MARK)) && Date.now() < deadline) {
      const r = api("GET", `/api/process/apps/${APP}/log?tail=50`);
      lines = r.status === 0 ? r.body.lines ?? [] : [];
      if (!lines.some((l) => l.includes(MARK))) await sleep(250);
    }
    assert.ok(lines.some((l) => /--- .* start by panel ---/.test(l)), "the capture names the start: " + lines.join(" | "));
    assert.ok(lines.some((l) => l.includes(MARK)), "the app's stdout is captured: " + lines.join(" | "));
  }
  {
    // A second start on a running app is refused at once (409) naming the
    // live pid, and no second process is spawned.
    const r = api("POST", `/api/process/apps/${APP}/start`);
    assert.notEqual(r.status, 0, "a repeat start is refused");
    assert.match(r.stdout + r.stderr, new RegExp(`already running \\(pid ${pid}\\)`), "the refusal names the live pid");
    const row = await settle(() => true, "the row after the refusal");
    assert.equal(row.pid, pid, "still the same process");
  }
  {
    const r = api("POST", `/api/process/apps/${APP}/stop`);
    assert.equal(r.status, 0, "stop is accepted at once: " + r.stdout + r.stderr);
    const row = await settle((row) => row.lastAction?.action === "stop", "the stop");
    assert.equal(row.lastAction.ok, true, "the stop succeeded: " + JSON.stringify(row.lastAction));
    assert.notEqual(row.status, "running", "the app no longer reads running: " + JSON.stringify(row));
    const deadline = Date.now() + 5_000;
    while (alive(pid) && Date.now() < deadline) await sleep(100);
    assert.equal(alive(pid), false, "the OS confirms the process is gone");
  }
  {
    const r = api("GET", `/api/process/history?app=${APP}`);
    assert.equal(r.status, 0, "history: " + r.stderr);
    const entries = r.body.entries ?? [];
    const start = entries.find((e) => e.action === "start" && e.ok === true && e.pid === pid);
    const stop = entries.find((e) => e.action === "stop" && e.ok === true);
    assert.ok(start, "the start is in the history with its pid: " + JSON.stringify(entries));
    assert.equal(start.by, "panel", "an action with no `by` is attributed to the panel");
    assert.ok(stop, "the stop is in the history: " + JSON.stringify(entries));
  }
  console.log("process: ok");
}, { plugin: "process", ready: "/api/process" });
