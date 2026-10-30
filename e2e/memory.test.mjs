import { api, mcp, toolText, withGateway } from "./helpers.mjs";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// The repo an agent works in lives OUTSIDE the e2e home: the Swiss home is
// never a project (SPEC §memory), so a checkout under it is refused on bind.
// A fresh temp dir per run, removed when the module ends.
const repo = mkdtempSync(join(tmpdir(), "swiss-e2e-repo-"));
mkdirSync(join(repo, ".git"));

await withGateway(async () => {
  // Project lifecycle: create, list, delete — the smallest honest walk of the
  // knowledge store on a real SQLite file in the e2e home. Delete is typed
  // confirmation: the confirm parameter must carry the project id itself.
  const name = "e2e-project-" + Date.now();
  let id = null;
  {
    const r = api("POST", "/api/knowledge/projects", { name });
    assert.equal(r.status, 0, "create project: " + r.stdout + r.stderr);
    assert.ok(r.body.project && r.body.project.id, "project id returned");
    id = r.body.project.id;
    assert.equal(r.body.project.name, name);
  }
  {
    const r = api("GET", "/api/knowledge/projects");
    assert.equal(r.status, 0, "list projects: " + r.stderr);
    const all = r.body.projects ?? r.body;
    assert.ok(Array.isArray(all) && all.some((p) => p.id === id), "created project listed");
  }
  {
    const r = api("GET", "/api/knowledge/status");
    assert.equal(r.status, 0, "status: " + r.stderr);
    assert.equal(r.body.mcp?.path, "/mcp/memory", "the status names the MCP path");
  }

  // The golden path an agent walks (SPEC §memory.mcp): the panel and an AI
  // client read and write the SAME notes. A directory bound to the project is
  // how the client's cwd finds it; a note typed in the panel is found over
  // /mcp/memory, and a note an agent saves there is found by the panel's search.
  // Lexical only: no embedding model is configured (the word tiers answer).
  {
    const r = api("POST", `/api/knowledge/projects/${id}/roots`, { path: repo });
    assert.equal(r.status, 0, "bind the repo: " + r.stdout + r.stderr);
  }
  const panelTitle = "Retries reuse the idempotency key quokka";
  {
    const r = api("POST", "/api/knowledge/records", {
      project: id, kind: "decision", title: panelTitle, content: "So a replayed request is applied once.",
    });
    assert.equal(r.status, 0, "save from the panel: " + r.stdout + r.stderr);
    assert.match(r.body.record?.id ?? "", /^m-/, "a note id comes back");
  }
  const minted = api("POST", "/api/tokens", { label: "e2e-memory" });
  assert.equal(minted.status, 0, "token create: " + minted.stderr);
  const bearer = minted.body.secret;
  {
    const r = await mcp("memory", bearer, {
      jsonrpc: "2.0", id: 1, method: "tools/call",
      params: { name: "memory_search", arguments: { cwd: repo, query: "quokka" } },
    });
    assert.equal(r.status, 200, "memory_search answers");
    assert.ok(!r.message?.result?.isError, "the search succeeded: " + JSON.stringify(r.message));
    assert.ok(toolText(r.message).includes(panelTitle), "the panel's note is found over MCP: " + toolText(r.message));
  }
  const agentTitle = "The ledger flush runs before the wombat snapshot";
  {
    const r = await mcp("memory", bearer, {
      jsonrpc: "2.0", id: 2, method: "tools/call",
      params: {
        name: "memory_save",
        arguments: { cwd: repo, kind: "pitfall", title: agentTitle, content: "Otherwise the snapshot misses the last batch." },
      },
    });
    assert.equal(r.status, 200, "memory_save answers");
    assert.ok(!r.message?.result?.isError, "the save succeeded: " + JSON.stringify(r.message));
  }
  {
    const r = api("GET", `/api/knowledge/search?project=${id}&q=wombat&semantic=off`);
    assert.equal(r.status, 0, "panel search: " + r.stderr);
    const items = r.body.items ?? [];
    assert.ok(JSON.stringify(items).includes(agentTitle), "the agent's note is found by the panel: " + JSON.stringify(items));
    assert.ok(!JSON.stringify(items).includes(panelTitle), "the search is by its words, not everything");
  }
  api("DELETE", "/api/tokens/" + minted.body.id);

  {
    const r = api("DELETE", "/api/knowledge/projects/" + id + "?confirm=" + encodeURIComponent(id));
    assert.equal(r.status, 0, "delete project: " + r.stdout + r.stderr);
  }
  {
    const r = api("GET", "/api/knowledge/projects");
    const all = r.body.projects ?? r.body;
    assert.ok(!all.some((p) => p.id === id), "project gone after delete");
  }
  console.log("memory: ok");
}, { plugin: "memory", ready: "/api/knowledge/status" }).finally(() => rmSync(repo, { recursive: true, force: true }));
