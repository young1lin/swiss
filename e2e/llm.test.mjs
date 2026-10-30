import { api, waitReady, withGateway } from "./helpers.mjs";
import assert from "node:assert/strict";

await withGateway(async () => {
  // Read side only: no provider key on the e2e home, so live calls must refuse
  // honestly (clean error), never hang or 500.
  {
    const r = await waitReady("/api/llm/status");
    assert.equal(r.status, 0, "status: " + r.stderr);
    // The real wire (llm_api.rs status): serving, providers, embedding, agents,
    // catalog, defaultPrompts, telemetry. The old assert ended in "or any object"
    // - vacuous. A shape drift fails here now.
    assert.equal(typeof r.body.serving, "boolean", "serving is a boolean");
    assert.ok(Array.isArray(r.body.providers), "providers is a list");
    assert.ok(r.body.telemetry && typeof r.body.telemetry === "object", "telemetry status object");
  }
  {
    const r = api("GET", "/api/llm/history/calls");
    assert.equal(r.status, 0, "history calls: " + r.stderr);
  }
  console.log("llm: ok");
}, { plugin: "llm", ready: "/api/llm/status" });
