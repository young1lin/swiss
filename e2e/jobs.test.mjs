import { api, waitReady, withGateway } from "./helpers.mjs";
import assert from "node:assert/strict";
import { sleep } from "./util.mjs";

await withGateway(async () => {
  await waitReady("/api/jobs");
  // BUG#1 API path (the exact save -> edit data flow the panel sheet drives):
  // a weekly-cron job must round trip and stay editable.
  const name = "e2e-weekly";
  {
    const put = api("PUT", "/api/jobs/" + name, {
      command: 'cmd /c echo swiss-e2e-weekly',
      cron: "30 8 * * 1-5",
      enabled: true,
    });
    assert.equal(put.status, 0, "create weekly job: " + put.stderr);
    assert.equal(put.body.job.cron, "30 8 * * 1-5", "cron echoes back");
  }
  { // The edit that used to crash the panel sheet: read back, change the time, save again.
    const got = api("GET", "/api/jobs");
    const row = got.body.jobs.find((j) => j.name === name);
    assert.ok(row, "weekly job listed after save");
    const edit = api("PUT", "/api/jobs/" + name, {
      command: 'cmd /c echo swiss-e2e-weekly',
      cron: "45 8 * * 1-5",
      enabled: true,
    });
    assert.equal(edit.status, 0, "edit weekly job: " + edit.stderr);
    assert.equal(edit.body.job.cron, "45 8 * * 1-5", "edited cron echoes back");
  }
  // An interval job actually fires and records a run.
  {
    const put = api("PUT", "/api/jobs/e2e-tick", {
      command: 'cmd /c echo tick',
      everySec: 2,
      enabled: true,
    });
    assert.equal(put.status, 0, "create interval job: " + put.stderr);
    const run = api("POST", "/api/jobs/e2e-tick/run", { async: true });
    assert.equal(run.status, 0, "trigger run: " + run.stderr);
    assert.ok(run.body.runId !== undefined && run.body.runId !== null, "runId returned (0 is a valid id)");
    // Verified run view keys: runId/state/exitCode/output (services/runs.rs to_json).
    let done = null;
    for (let i = 0; i < 40 && !done; i++) {
      await sleep(500);
      const r = api("GET", "/api/runs/" + run.body.runId);
      const view = r.body && (r.body.run ?? r.body);
      const state = view && view.state;
      if (view && state && state !== "queued" && state !== "running") done = view;
    }
    assert.ok(done, "run reached a terminal state via /api/runs/{id}");
    assert.equal(done.exitCode, 0, "run exit code, error: " + (done.error || ""));
    assert.ok(String(done.output || "").includes("tick"), "run output captured");
  }
  // Cleanup: remove what we created.
  for (const n of [name, "e2e-tick"]) {
    const del = api("DELETE", "/api/jobs/" + n);
    assert.equal(del.status, 0, "delete " + n + ": " + del.stderr);
  }
  const after = api("GET", "/api/jobs");
  assert.ok(!after.body.jobs.some((j) => j.name === name || j.name === "e2e-tick"), "jobs cleaned up");
  console.log("jobs: ok");
});
