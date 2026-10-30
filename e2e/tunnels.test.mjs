import { api, waitReady, withGateway } from "./helpers.mjs";
import assert from "node:assert/strict";

await withGateway(async () => {
  // Read side only: a tunnel CRUD walk needs a reachable SSH server, which the e2e
  // home deliberately does not have. The list endpoints must answer honestly empty.
  // The panel's poll reads ONE round trip: GET /api/tunnels carries the rows plus
  // ruleGroups/connGroups (api.rs:405-428); the /connections and /rules paths are
  // write routes, and the old /api/tunnels/groups family is retired.
  const base = await waitReady("/api/tunnels");
  assert.equal(base.status, 0, "base list: " + base.stderr);
  const connList = base.body.connections ?? base.body.conns ?? [];
  const ruleList = base.body.rules ?? [];
  assert.ok(Array.isArray(connList), "connections list shape: " + JSON.stringify(Object.keys(base.body)));
  assert.ok(Array.isArray(ruleList), "rules list shape");
  // One round trip carries the groups too (api.rs): both are real arrays, and a
  // fresh home carries exactly the implicit default group (store.rs groups_of:
  // the default group is implicit, never stored, never absent).
  assert.ok(Array.isArray(base.body.ruleGroups), "ruleGroups is a list");
  assert.ok(Array.isArray(base.body.connGroups), "connGroups is a list");
  assert.deepEqual(base.body.ruleGroups, ["default"], "fresh home: only the implicit default rule group");
  assert.deepEqual(base.body.connGroups, ["default"], "fresh home: only the implicit default connection group");
  console.log("tunnels: ok");
}, { plugin: "tunnels", ready: "/api/tunnels" });
