import { api, get, mcp, toolText, waitReady, withGateway } from "./helpers.mjs";
import assert from "node:assert/strict";

await withGateway(async () => {
  {
    const r = await waitReady("/api/mcps");
    assert.equal(r.status, 0, "mcps: " + r.stderr);
    const list = r.body.mcps ?? r.body;
    assert.ok(Array.isArray(list), "mcps list shape");
    // The built-in echo MCP is always present (SPEC §mcp).
    const ids = list.map((m) => m.name ?? m.id).filter(Boolean);
    assert.ok(ids.includes("echo"), "echo MCP listed, got: " + ids.join(","));
  }
  // The MCP bearer space (/mcp/*) is separate from the admin session: no credentials
  // at all must fail closed.
  {
    const r = await get("/mcp/echo");
    assert.ok(r.status === 401 || r.status === 404, "no-bearer /mcp fails closed, got " + r.status);
  }

  // The golden path an AI client walks: a token minted on the admin side is the
  // one bearer /mcp accepts; a tool answers through it; the traffic log names
  // the call; revoking the token closes the door again. The secret stays in
  // this process - never printed, never in an assert message.
  const minted = api("POST", "/api/tokens", { label: "e2e-client" });
  assert.equal(minted.status, 0, "token create: " + minted.stderr);
  const { id: tokenId, secret } = minted.body;
  assert.ok(tokenId && typeof secret === "string" && secret.length >= 16, "a token id and a secret come back");
  {
    const r = await mcp("echo", "not-" + secret.slice(4), { jsonrpc: "2.0", id: 1, method: "tools/list" });
    assert.equal(r.status, 401, "a wrong bearer is refused before the MCP sees the frame");
  }
  {
    const r = await mcp("echo", secret, { jsonrpc: "2.0", id: 2, method: "tools/list" });
    assert.equal(r.status, 200, "tools/list answers with the minted bearer");
    const names = (r.message?.result?.tools ?? []).map((t) => t.name);
    assert.deepEqual(names, ["echo"], "the echo MCP offers exactly its one tool");
  }
  const marker = "e2e-ping-" + Date.now();
  {
    const r = await mcp("echo", secret, {
      jsonrpc: "2.0", id: 3, method: "tools/call", params: { name: "echo", arguments: { msg: marker } },
    });
    assert.equal(r.status, 200, "tools/call answers");
    assert.ok(!r.message?.result?.isError, "the call succeeded: " + JSON.stringify(r.message));
    assert.equal(toolText(r.message).trim(), marker, "the tool echoes what it was given");
  }
  {
    const r = api("GET", "/api/traffic?name=echo");
    assert.equal(r.status, 0, "traffic: " + r.stderr);
    const entries = r.body.entries ?? [];
    const call = entries.find((e) => e.method === "tools/call");
    assert.ok(call, "the traffic log names the tools/call: " + JSON.stringify(entries.map((e) => e.method)));
    assert.equal(call.hasResponse, true, "the reply was captured beside the request");
  }
  {
    const r = api("DELETE", "/api/tokens/" + tokenId);
    assert.equal(r.status, 0, "token revoke: " + r.stderr);
    const after = await mcp("echo", secret, { jsonrpc: "2.0", id: 4, method: "tools/list" });
    assert.equal(after.status, 401, "a revoked token no longer authenticates");
  }
  console.log("mcp: ok");
}, { ready: "/api/mcps" });
