import { api, get, withGateway } from "./helpers.mjs";
import assert from "node:assert/strict";

await withGateway(async () => {
  // /health: documented open surface, static shape.
  {
    const h = await get("/health");
    assert.equal(h.status, 200);
    const body = JSON.parse(h.text);
    assert.equal(body.ok, true);
    assert.ok(body.build && typeof body.build.hash === "string" && body.build.hash.length > 0);
    console.log("health ok, build " + body.build.hash.slice(0, 8));
  }
  // The admin session gate fails closed: no key, no cookie => 401 on /api/*.
  {
    const r = await get("/api/jobs");
    assert.ok(r.status === 401 || r.status === 403, "unauthenticated /api must fail closed, got " + r.status);
  }
  // /api/info through the CLI signer: the panel boot contract.
  {
    const r = api("GET", "/api/info");
    assert.equal(r.status, 0, "swiss api exit code");
    assert.ok(r.body.panelVersion, "panelVersion present");
    assert.ok(r.body.build && r.body.build.hash, "build hash present");
    const again = api("GET", "/api/info");
    assert.equal(again.body.panelVersion, r.body.panelVersion, "stamp stable within one build");
  }
  // Plugin registry rows are served (the nine plugins; the panel shell is not a row).
  {
    const r = api("GET", "/api/plugins");
    assert.equal(r.status, 0);
    const ids = (r.body.plugins || []).map((p) => p.id);
    for (const id of ["mcp", "jobs", "process", "terminal", "remote", "llm", "memory", "tunnels", "data"]) {
      assert.ok(ids.includes(id), "plugin row missing: " + id);
    }
  }
  console.log("host: ok");
});
