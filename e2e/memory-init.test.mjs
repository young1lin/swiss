// Live: a whole-project memory init against a real model (SPEC §memory.init), env-gated.
// The repo's .env names the chat model (SWISS_LIVE_LLM_*) and, optionally, the embedding
// model (SWISS_LIVE_EMBED_*); without the chat model the file skips. A layered Java shop
// with SQL scripts beside src/ is mapped with a redraw, then read in slices: the map must
// come out as business domains (never `src` or a bare layer; `sql` may stay a technical area
// beside them, SPEC §memory.map), notes must land, and with
// an embedding model they must be indexed - lexical only without one.
import { LIVE } from "./live-env.mjs";
import { api, ENV, EXE, waitReady, withGateway } from "./helpers.mjs";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";

if (!LIVE.llm) {
  console.log("memory-init: SKIP - no SWISS_LIVE_LLM_BASE_URL / SWISS_LIVE_LLM_MODEL in .env (e2e/README.md)");
  process.exit(0);
}

const repo = mkdtempSync(join(tmpdir(), "swiss-e2e-java-"));
const put = (rel, text) => {
  mkdirSync(dirname(join(repo, rel)), { recursive: true });
  writeFileSync(join(repo, rel), text);
};
const base = "src/main/java/com/acme/shop";
const layers = {
  controller: ["OrderController", "PaymentController", "CustomerController"],
  service: ["OrderService", "PaymentService", "CustomerService"],
  repository: ["OrderRepository", "PaymentRepository", "CustomerRepository"],
  model: ["Order", "Payment", "Customer"],
};
for (const [layer, names] of Object.entries(layers)) {
  for (const name of names) {
    put(`${base}/${layer}/${name}.java`, `package com.acme.shop.${layer};\n\npublic class ${name} {\n}\n`);
  }
}
put(`${base}/service/OrderService.java`, [
  "package com.acme.shop.service;",
  "",
  "public class OrderService {",
  "    /** Orders above 10000 wait for a manual review before payment is captured. */",
  "    public void place(Order order) {",
  "        if (order.total() > 10_000) { order.holdForReview(); return; }",
  "        payments.capture(order.id(), order.idempotencyKey());",
  "    }",
  "}",
  "",
].join("\n"));
put(`${base}/service/PaymentService.java`, [
  "package com.acme.shop.service;",
  "",
  "public class PaymentService {",
  "    /** A retried capture must reuse the idempotency key, or the gateway charges twice. */",
  "    public void capture(String orderId, String idempotencyKey) {}",
  "}",
  "",
].join("\n"));
for (const name of ["schema", "data", "upgrade_v2"]) put(`sql/${name}.sql`, "CREATE TABLE orders (id BIGINT PRIMARY KEY, total BIGINT);\n");
put("README.md", "# Shop\n\nAn online shop: orders, payments and customers.\n");
spawnSync("git", ["init", "-q"], { cwd: repo });
spawnSync("git", ["-c", "user.email=e2e@example.com", "-c", "user.name=e2e", "add", "."], { cwd: repo });
spawnSync("git", ["-c", "user.email=e2e@example.com", "-c", "user.name=e2e", "commit", "-qm", "the shop"], { cwd: repo });

/** One CAS edit of a plugin row through the host config API. */
function putRow(plugin, change) {
  const cur = api("GET", `/api/plugins/${plugin}/config`);
  assert.equal(cur.status, 0, `read the ${plugin} row: ${cur.stderr}`);
  const config = (cur.body && cur.body.config) || {};
  change(config);
  const r = api("PUT", `/api/plugins/${plugin}/config`, { revision: cur.body.revision, config });
  assert.equal(r.status, 0, `apply the ${plugin} row: ${r.stderr}`);
  assert.ok(!(r.body && r.body.error), `the ${plugin} row applied: ${JSON.stringify(r.body)}`);
}

function swiss(args, timeout = 900_000) {
  const r = spawnSync(EXE, ["memory", ...args, "--cwd", repo], { env: ENV, encoding: "utf8", timeout });
  if (r.error) throw r.error;
  return { status: r.status, out: (r.stdout || "") + (r.stderr || "") };
}

try {
  await withGateway(async () => {
    assert.equal((await waitReady("/api/llm/status")).status, 0, "the llm plugin serves");
    putRow("llm", (config) => {
      const entry = (p, kind) => ({
        style: p.style, baseUrl: p.baseUrl, ...(p.apiKey ? { apiKey: p.apiKey } : {}),
        models: [kind ? { id: p.model, kind } : { id: p.model }],
      });
      config.providers = { live: entry(LIVE.llm) };
      if (LIVE.embed) config.providers.liveembed = entry(LIVE.embed, "embedding");
      config.agents = { init: { runtime: "chat", provider: "live", model: LIVE.llm.model, timeoutMs: 600_000 } };
      if (LIVE.embed) config.embedding = { provider: "liveembed", model: LIVE.embed.model };
    });
    putRow("memory", (config) => { config.sync = { agent: "init", transcripts: "off", auto: false }; });

    // The plan asks no model: the slices, and the index the notes would land in.
    const plan = swiss(["init", "--full", "--plan", "--json"]);
    assert.equal(plan.status, 0, plan.out);
    // `--json` prints the plan pretty, over many lines: the object runs from the first
    // line that opens one to the last line that closes one.
    const planned = JSON.parse(plan.out.slice(plan.out.search(/^\{/m), plan.out.lastIndexOf("\n}") + 2));
    assert.ok(planned.knowledge.slices >= 1, "the sources are cut into slices: " + plan.out);
    assert.equal(planned.embedding.state === "off", !LIVE.embed, "the plan says the index: " + JSON.stringify(planned.embedding));

    const run = swiss(["init", "--full", "--redraw", "--wait", "--max-asks", "4", "--lang", "zh"]);
    console.log(run.out);
    assert.equal(run.status, 0, run.out);
    assert.match(run.out, /knowledge: \d+ of \d+ slice\(s\) asked/, "the run says its slices");
    assert.match(run.out, LIVE.embed ? /embedding: (ready|indexing|stale)/ : /embedding: off/, "the run says the index");

    // The project init registered for the checkout, found by its name: the list is sorted by
    // name and the home may hold others, so the first row is not necessarily this one.
    const listed = api("GET", "/api/knowledge/projects").body;
    const projects = listed.projects ?? listed;
    const project = projects.find((p) => p.name === basename(repo));
    assert.ok(project && project.id, "init registered the checkout: " + projects.map((p) => p.name).join(", "));
    const domains = api("GET", "/api/knowledge/domains?project=" + project.id).body.items.map((d) => d.path);
    console.log("domains:", domains.join(", "));
    assert.ok(!domains.some((p) => ["src", "main", "java", "controller", "service", "repository", "model"].includes(p)),
      "no top-level domain is a source wrapper or a bare layer: " + domains);
    const business = ["order", "payment", "customer"].filter((w) => domains.some((p) => p.toLowerCase().includes(w)));
    assert.ok(business.length >= 2, "the map names the business: " + domains);

    const notes = api("GET", "/api/knowledge/records?project=" + project.id + "&limit=200").body.items;
    assert.ok(notes.length >= 1, "the slices came to notes");
    if (LIVE.embed) {
      const status = api("GET", "/api/knowledge/embedding").body.status;
      assert.ok(status.indexed >= 1, "the notes are indexed: " + JSON.stringify(status));
    }
  });
} finally {
  rmSync(repo, { recursive: true, force: true });
}
