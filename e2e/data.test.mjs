import { api, waitReady, withGateway } from "./helpers.mjs";
import assert from "node:assert/strict";

await withGateway(async () => {
  // Fresh home: the database catalog is honestly empty, unknown db names 404, and the
  // browser surface fails closed without credentials.
  {
    const r = await waitReady("/api/db");
    assert.equal(r.status, 0, "catalog: " + r.stderr);
    // The real wire on a fresh home (swiss-data's dbbrowser_api): { connections: [] } - the
    // browser catalog (primary/current/databases) appears only when a database is
    // connected. The honest empty catalog is no connections at all.
    assert.ok(Array.isArray(r.body.connections), "connections is a list: " + JSON.stringify(r.body).slice(0, 120));
    assert.equal(r.body.connections.length, 0, "the fresh home has no database connections");
  }
  {
    const r = api("GET", "/api/db/e2e-unknown/tables");
    assert.notEqual(r.status, 0, "unknown db must not succeed");
    assert.match(String(r.stdout) + String(r.stderr), /404|not found|unknown/i);
  }
  console.log("data: ok");
}, { plugin: "data", ready: "/api/db" });
