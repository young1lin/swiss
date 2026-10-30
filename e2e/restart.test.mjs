import { api, boot, ensurePlugin, HOME, mcp, stop, waitReady } from "./helpers.mjs";
import assert from "node:assert/strict";

// Restart persistence across modules: what an operator saved before a gateway
// restart is all there after it, on the same home - the plugins they enabled,
// the client token their AI tools hold, the notes in memory, the apps the
// process page declares. Each module proves its own round trips elsewhere;
// this file proves none of them lives only in a process's memory.

const APP = "e2e-kept";
const TITLE = "The archive job skips the platypus shard";
let projectId = null;
let tokenId = null;
let bearer = null;

try {
  await boot({ fresh: true });
  ensurePlugin("memory");
  ensurePlugin("process");
  assert.equal((await waitReady("/api/knowledge/status")).status, 0, "memory serves");
  assert.equal((await waitReady("/api/process")).status, 0, "process serves");

  const minted = api("POST", "/api/tokens", { label: "e2e-kept" });
  assert.equal(minted.status, 0, "token create: " + minted.stderr);
  ({ id: tokenId, secret: bearer } = minted.body);

  const project = api("POST", "/api/knowledge/projects", { name: "e2e-kept-" + Date.now() });
  assert.equal(project.status, 0, "create project: " + project.stdout + project.stderr);
  projectId = project.body.project.id;
  const note = api("POST", "/api/knowledge/records", {
    project: projectId, kind: "decision", title: TITLE, content: "It is rebuilt nightly instead.",
  });
  assert.equal(note.status, 0, "save note: " + note.stdout + note.stderr);

  const app = { name: APP, dir: HOME, start: { program: process.execPath, args: ["--version"] } };
  const saved = api("PUT", "/api/plugins/process/config", { config: { apps: [app] } });
  assert.equal(saved.status, 0, "save the app: " + saved.stdout + saved.stderr);
} finally {
  stop();
}

try {
  await boot({ fresh: false });
  assert.equal((await waitReady("/api/knowledge/status")).status, 0, "memory serves after the restart");
  assert.equal((await waitReady("/api/process")).status, 0, "process serves after the restart");
  {
    const r = api("GET", "/api/plugins");
    const rows = r.body.plugins ?? [];
    for (const id of ["memory", "process"]) {
      assert.equal(rows.find((p) => p.id === id)?.enabled, true, id + " is still enabled");
    }
  }
  {
    // The token an AI client holds still opens /mcp - a restart never logs clients out.
    const r = await mcp("echo", bearer, { jsonrpc: "2.0", id: 1, method: "tools/list" });
    assert.equal(r.status, 200, "the token minted before the restart still authenticates");
  }
  {
    const r = api("GET", `/api/knowledge/search?project=${projectId}&q=platypus&semantic=off`);
    assert.equal(r.status, 0, "search: " + r.stderr);
    assert.ok(JSON.stringify(r.body.items ?? []).includes(TITLE), "the note survived the restart: " + JSON.stringify(r.body));
  }
  {
    const r = api("GET", "/api/process");
    const row = (r.body.apps ?? []).find((a) => a.name === APP);
    assert.ok(row, "the declared app survived the restart: " + JSON.stringify(r.body.apps));
    // A bare `serve` is not a daemon: nothing starts on its own (SPEC §process.apps).
    assert.equal(row.status, "unknown", "a restart starts no app by itself: " + JSON.stringify(row));
  }
  api("DELETE", "/api/tokens/" + tokenId);
  console.log("restart: ok");
} finally {
  stop();
}
