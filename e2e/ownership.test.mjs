// Ownership safety regressions for the e2e harness boot/stop pair.
//
// The harness may only ever kill a gateway it spawned itself. These tests drive
// the REAL imported helpers.mjs (not a replica) through mocked Node builtins;
// they earned the helpers guard repair (boot refuses any listener it does not
// own, stop kills only our own live child) and now pin it. Zero side effects
// by design: every spawn/spawnSync/fetch/destructive-fs call goes through the
// mocks, any unexpected command or URL throws instead of reaching the real
// function, and every mock is restored in finally. node:test + node:assert
// only, no new deps.
import { test, after } from "node:test";
import assert from "node:assert/strict";
import { createRequire, syncBuiltinESMExports } from "node:module";
import { join } from "node:path";
import { homedir } from "node:os";

// Sanctioned e2e overlay, pinned BEFORE helpers.mjs is imported so its
// import-time GATE and its PORT/HOME/EXE/BASE bindings are deterministic. A
// parent SWISS_HOME (if one leaked into this process) is cleared first: the
// harness must never treat it as a production-home candidate.
// The overlay pinning below is the only env this file mutates; savedEnv holds
// the operator's original five values so the after() hook can restore them.
const savedEnv = {
  SWISS_HOME: process.env.SWISS_HOME,
  SWISS_E2E_KEEP_HOME: process.env.SWISS_E2E_KEEP_HOME,
  SWISS_E2E_PORT: process.env.SWISS_E2E_PORT,
  SWISS_E2E_HOME: process.env.SWISS_E2E_HOME,
  SWISS_E2E_EXE: process.env.SWISS_E2E_EXE,
};
delete process.env.SWISS_HOME;
delete process.env.SWISS_E2E_KEEP_HOME;
process.env.SWISS_E2E_PORT = "19998";
process.env.SWISS_E2E_HOME = join(
  process.env.LOCALAPPDATA || join(homedir(), "AppData", "Local"),
  "swiss-e2e-19998",
);
process.env.SWISS_E2E_EXE = join(process.cwd(), "target-test", "release", "swiss.exe");

const helpers = await import("./helpers.mjs");
const require = createRequire(import.meta.url);
const cp = require("node:child_process");
const fsMod = require("node:fs");

// After every case has run, put back exactly the five overlay variables the
// pinning block above replaced (undefined restores as absent).
after(() => {
  for (const key of Object.keys(savedEnv)) {
    if (savedEnv[key] === undefined) delete process.env[key];
    else process.env[key] = savedEnv[key];
  }
});

const LF = String.fromCharCode(10);

// The mock seat. listeners are fake netstat rows; the spawned handle is a
// tracked fake ChildProcess (real-Node shaped: exitCode and signalCode) whose
// identity is captured the moment the spawn exists, mirroring the helpers
// contract these tests pin.
function makeSeat() {
  const recorder = {
    netstatCalls: 0,
    taskkills: [],
    spawns: [],
    fetches: [],
    unexpected: [],
  };
  let listeners = [];
  let nextPid = 7000;
  let liveHandle = null;
  const seat = {
    recorder,
    setListeners(rows) { listeners = rows; },
    handle() { return liveHandle; },
    exitHandle(code) { if (liveHandle) liveHandle.exitCode = code; },
    install() {
      seat.realSpawnSync = cp.spawnSync;
      seat.realSpawn = cp.spawn;
      seat.realRmSync = fsMod.rmSync;
      seat.realFetch = globalThis.fetch;
      const renderNetstat = () => {
        const rows = listeners.map((row) =>
          "  TCP    127.0.0.1:" + helpers.PORT +
          "          0.0.0.0:0              LISTENING        " + row.pid);
        return rows.length ? rows.join(LF) + LF : "";
      };
      cp.spawnSync = (cmd, args, opts) => {
        if (cmd === "netstat") {
          recorder.netstatCalls += 1;
          return { status: 0, stdout: renderNetstat(), stderr: "" };
        }
        if (cmd === "taskkill") {
          recorder.taskkills.push(Number(args[1]));
          return { status: 0, stdout: "", stderr: "" };
        }
        recorder.unexpected.push("spawnSync " + cmd + " " + String(args));
        throw new Error("unexpected spawnSync in ownership test: " + cmd);
      };
      cp.spawn = (cmd, args, opts) => {
        if (cmd !== helpers.EXE || !args || args[0] !== "serve") {
          recorder.unexpected.push("spawn " + cmd + " " + String(args));
          throw new Error("unexpected spawn in ownership test: " + cmd);
        }
        nextPid += 1;
        const handle = {
          pid: nextPid,
          argv: args.join(" "),
          exitCode: null,
          signalCode: null,
          killed: false,
          killCount: 0,
          kill() { this.killCount += 1; this.killed = true; return true; },
          on() { return this; },
          once() { return this; },
          removeAllListeners() { return this; },
        };
        liveHandle = handle;
        recorder.spawns.push({ pid: handle.pid, argv: handle.argv });
        return handle;
      };
      fsMod.rmSync = (target, opts) => {
        recorder.unexpected.push("rmSync " + target);
        throw new Error("unexpected destructive fs call in ownership test: rmSync " + target);
      };
      globalThis.fetch = async (url, opts) => {
        recorder.fetches.push(String(url));
        if (String(url) === helpers.BASE + "/health") {
          // boot only checks r.ok; the body text is never parsed.
          return { ok: true, status: 200, text: async () => "ownership-mock-health" };
        }
        throw new Error("unexpected fetch in ownership test: " + url);
      };
      syncBuiltinESMExports(cp);
      syncBuiltinESMExports(fsMod);
    },
    restore() {
      cp.spawnSync = seat.realSpawnSync;
      cp.spawn = seat.realSpawn;
      fsMod.rmSync = seat.realRmSync;
      globalThis.fetch = seat.realFetch;
      syncBuiltinESMExports(cp);
      syncBuiltinESMExports(fsMod);
    },
  };
  return seat;
}

function cleanup(seat) {
  // Cleanup runs while the mocks are still installed: a real stop() would reach
  // the real netstat/taskkill. The port is emptied first so stop finds no pid.
  seat.setListeners([]);
  try { helpers.stop(); } catch {}
  seat.restore();
}

test("boot must refuse an unknown preexisting listener instead of killing it", async () => {
  const seat = makeSeat();
  seat.setListeners([{ pid: 4242 }]);
  seat.install();
  try {
    let refused = false;
    try {
      await helpers.boot({ fresh: false });
    } catch {
      refused = true;
    }
    assert.ok(refused, "boot must refuse a 19998 listener it does not own, not kill it");
    assert.deepEqual(seat.recorder.taskkills, [], "no taskkill may happen for an unknown listener");
    assert.deepEqual(seat.recorder.spawns, [], "no gateway may be spawned while a stranger owns the port");
    assert.deepEqual(seat.recorder.unexpected, []);
  } finally {
    cleanup(seat);
  }
});

test("stop must never kill a stranger listener that took the port after our fake boot", async () => {
  const seat = makeSeat();
  seat.setListeners([]);
  seat.install();
  try {
    await helpers.boot({ fresh: false });
    const handle = seat.handle();
    assert.ok(handle, "the fake boot must have produced a tracked handle");
    assert.ok(seat.recorder.fetches.every((u) => u === helpers.BASE + "/health"),
      "the harness may only fetch its own /health, saw: " + JSON.stringify(seat.recorder.fetches));
    // A stranger now owns the port; our child is alive but is not the listener.
    seat.setListeners([{ pid: 4242 }]);
    helpers.stop();
    assert.ok(!seat.recorder.taskkills.includes(4242),
      "stop must never taskkill a pid it did not spawn, got: " + JSON.stringify(seat.recorder.taskkills));
    assert.deepEqual(seat.recorder.unexpected, []);
  } finally {
    cleanup(seat);
  }
});

test("stop cleans up the listener our own live child owns", async () => {
  const seat = makeSeat();
  seat.setListeners([]);
  seat.install();
  try {
    await helpers.boot({ fresh: false });
    const handle = seat.handle();
    assert.ok(handle, "the fake boot must have produced a tracked handle");
    seat.setListeners([{ pid: handle.pid }]); // our own live child is the listener
    helpers.stop();
    for (const pid of seat.recorder.taskkills) {
      assert.ok(pid === handle.pid, "only the owned pid may be killed, got " + pid);
    }
    assert.ok(seat.recorder.taskkills.includes(handle.pid) || handle.killCount >= 1,
      "the owned gateway must actually be cleaned up");
    assert.deepEqual(seat.recorder.unexpected, []);
  } finally {
    cleanup(seat);
  }
});

test("an exited child pid reused by a stranger listener must not be killed", async () => {
  const seat = makeSeat();
  seat.setListeners([]);
  seat.install();
  try {
    await helpers.boot({ fresh: false });
    const handle = seat.handle();
    assert.ok(handle, "the fake boot must have produced a tracked handle");
    seat.exitHandle(1); // the child died; its pid is stale from this moment on
    seat.setListeners([{ pid: handle.pid }]); // a stranger now holds that reused pid
    helpers.stop();
    assert.ok(!seat.recorder.taskkills.includes(handle.pid),
      "a stale (exited) owned pid is not proof of ownership, got kills: "
        + JSON.stringify(seat.recorder.taskkills));
    assert.deepEqual(seat.recorder.unexpected, []);
  } finally {
    cleanup(seat);
  }
});
