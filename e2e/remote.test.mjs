import { api, waitReady, withGateway } from "./helpers.mjs";
import assert from "node:assert/strict";

// BUG#2 context: the runs surface must stay honest on a home with no remote history.
// The stdin seal itself is covered by the Rust integration test (tests/remote_history.rs,
// an_exec_stdin_is_sealed_beside_the_record_and_unseals_on_show); driving a real SSH exec
// from e2e requires a target, so that part is opt-in via SWISS_E2E_SSH_TARGET (see README).
await withGateway(async () => {
  {
    const r = await waitReady("/api/remote/targets");
    assert.equal(r.status, 0, "targets: " + r.stderr);
    assert.ok(Array.isArray(r.body.targets), "targets is a list");
  }
  {
    const r = api("GET", "/api/remote/endpoints");
    assert.equal(r.status, 0, "endpoints: " + r.stderr);
  }
  {
    const r = api("GET", "/api/remote/runs");
    assert.equal(r.status, 0, "runs: " + r.stderr);
    assert.ok(Array.isArray(r.body.runs), "runs is a list");
    // What "kept" means rides along: the limits in force and the usage they hold.
    assert.ok(r.body.limits && r.body.limits.maxTotalBytes, "limits ride along");
    assert.ok(r.body.usage && typeof r.body.usage.bytes === "number", "usage rides along");
    if (r.body.nextBefore !== undefined) {
      assert.equal(typeof r.body.nextBefore, "string", "nextBefore cursor shape");
    }
  }
  // Unknown run id: 404 through the CLI (non-zero exit), not a 500.
  {
    const r = api("GET", "/api/remote/runs/does-not-exist");
    assert.notEqual(r.status, 0, "unknown run must not be a success");
    assert.match(String(r.stdout) + String(r.stderr), /404|not found/i);
  }
  // The README's opt-in promise, implemented: with SWISS_E2E_SSH_TARGET set
  // (user@host), one real exec runs through the actions surface and its stdin
  // must come back stored - the sealed-stdin regression, live. Without the env
  // this block is skipped (the Rust integration test covers the seal offline).
  const sshTarget = process.env.SWISS_E2E_SSH_TARGET;
  if (sshTarget) {
    const started = api("POST", "/api/actions", {
      action: "remote.exec",
      input: { target: sshTarget, argv: ["echo", "swiss-e2e"], stdin: "e2e-stdin-marker\n" },
    });
    assert.equal(started.status, 0, "exec started: " + started.stderr);
    const runId = started.body.id ?? started.body.runId ?? (started.body.run && started.body.run.id);
    assert.ok(runId, "run id returned: " + JSON.stringify(started.body).slice(0, 200));
    let run = null;
    for (let i = 0; i < 60; i++) {
      const r = api("GET", "/api/runs/" + runId);
      if (r.status === 0 && String(r.body.state ?? r.body.status ?? "").match(/succeeded|failed|done/)) { run = r.body; break; }
      await new Promise((res) => setTimeout(res, 500));
    }
    assert.ok(run, "the exec run reached a terminal state");
    assert.match(String(run.state ?? run.status ?? ""), /succeeded|done/i, "exec succeeded: " + JSON.stringify(run).slice(0, 300));
    const shown = api("GET", "/api/runs/" + runId + "?output=0");
    assert.equal(shown.status, 0, "run detail: " + shown.stderr);
    const detail = JSON.stringify(shown.body);
    assert.ok(/stdinStored|stdin/.test(detail), "the stdin is stored beside the record");
  }
  console.log("remote: ok" + (sshTarget ? " (live SSH exec walked)" : ""));
}, { plugin: "remote", ready: "/api/remote/targets" });
