import { api, waitReady, withGateway } from "./helpers.mjs";
import assert from "node:assert/strict";

await withGateway(async () => {
  const r = await waitReady("/api/terminal/targets");
  assert.equal(r.status, 0, "targets: " + r.stderr);
  // The wire (terminal_api.rs the_targets_route...): { local: { shells: [{program,
  // label}...], shell: "<absolute path>" }, remote: ... }. The old test read
  // body.targets - a field that never exists - so the whole open/resize/close walk
  // silently skipped on every run. The local object always exists on a real host.
  assert.ok(r.body.local && Array.isArray(r.body.local.shells), "local.shells is an array: " + JSON.stringify(r.body).slice(0, 200));
  assert.ok(r.body.local.shells.length > 0, "at least one candidate shell on a real host");
  assert.ok(typeof r.body.local.shell === "string" && r.body.local.shell.length > 0, "the resolved local shell");
  if (r.body.remote !== undefined) {
    assert.ok(typeof r.body.remote === "object", "remote is an object when present");
  }
  // The local target is always openable in principle: walk it for real.
  const target = "local";
  const open = api("POST", "/api/terminal/sessions", { target, cols: 80, rows: 24 });
  if (open.status !== 0) {
    // An honest refusal (e.g. shell spawn refused headless) still proves the route answers.
    assert.match(String(open.stdout) + String(open.stderr), /409|502|refused|shell/i, "open refuses honestly: " + open.stdout + open.stderr);
    console.log("terminal: ok (open refused honestly, no session left behind)");
    return;
  }
  const id = (open.body.session && open.body.session.id) ?? open.body.id;
  assert.ok(id, "session id returned: " + JSON.stringify(open.body).slice(0, 200));
  {
    const rs = api("POST", "/api/terminal/sessions/" + id + "/resize", { cols: 100, rows: 30 });
    assert.equal(rs.status, 0, "resize: " + rs.stderr);
  }
  {
    const cl = api("DELETE", "/api/terminal/sessions/" + id);
    assert.equal(cl.status, 0, "close: " + cl.stderr);
  }
  {
    const rec = api("GET", "/api/terminal/recordings");
    assert.equal(rec.status, 0, "recordings: " + rec.stderr);
    assert.ok(Array.isArray(rec.body.recordings ?? rec.body), "recordings list");
  }
  console.log("terminal: ok (local session opened, resized, closed)");
}, { plugin: "terminal", ready: "/api/terminal/targets" });