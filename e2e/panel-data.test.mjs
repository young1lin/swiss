// Data view, real browser: the empty catalog paints an honest empty state,
// reached by a real rail click, errorless. Then the analysis ledger
// (data-analysis.ts): its head button needs a CURRENT connection, so the walk
// registers one lazy mysql def aimed at a port nothing will ever answer. The
// ledger's list/create routes are pure store work (analysis_api.rs - they
// never touch the database), so the whole flow walks against a down
// connection, which is the honest case a fresh machine is in.
import assert from "node:assert/strict";
import { withPanel } from "./browser.mjs";
import { api } from "./helpers.mjs";

await withPanel(async (b) => {
  await b.waitFor("(() => !!document.querySelector('.rail-btn[data-view=\"data\"]'))()");
  await b.click(".rail-btn[data-view=\"data\"]");
  await b.waitFor("(() => location.hash === '#data')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");

  // One connection in the registry: the view takes it as current by itself
  // (data-view.ts loadDbView picks the registry's first when none is). The
  // def lands AFTER this page's one /api/db read, and nothing re-reads it on
  // its own - so the walk leaves the view and comes back, the user's way of
  // taking a connection that just appeared: the remount runs loadDbView again
  // and the head lands the Analyses button. No sidebar dance needed.
  const added = api("POST", "/api/mcps", JSON.stringify({
    name: "e2e-mysql", type: "mysql", enabled: true, lazy: true,
    host: "127.0.0.1", port: 1, user: "e2e", password: "e2e", database: "e2e",
  }));
  assert.equal(added.status, 0, "register e2e-mysql: " + added.stderr);
  await b.eval("(() => { location.hash = '#mcps'; return true; })()");
  await b.waitFor("(() => location.hash === '#mcps')()");
  await b.eval("(() => { location.hash = '#data'; return true; })()");
  await b.waitFor("(() => !!document.querySelector('button[title=\"Analyses\"]'))()", 10_000);
  await b.visibleOk('button[title="Analyses"]');
  await b.click('button[title="Analyses"]');
  // The sheet OPENS VISIBLY (panel proof-of-life: hidden false AND painted
  // words), with the honest empty state on a ledger that holds nothing yet.
  await b.waitFor("(() => !!document.querySelector('#sheet') && !document.querySelector('#sheet').hidden)()");
  assert.match(await b.eval("(() => document.querySelector('#sheet').textContent)()"), /No analyses yet/);
  // New analysis -> the create form; Create plants the first task and opens
  // it as the connection's own SQL tab (dbAnalysisOpen).
  await b.click("#sheet .btn.primary");
  await b.waitFor("(() => !!document.querySelector('#sheet textarea'))()");
  await b.type('#sheet input[type="text"]', "e2e walk");
  await b.type("#sheet textarea", "select 1");
  await b.click("#sheet .btn.primary");
  await b.waitFor("(() => !document.querySelector('#sheet') || document.querySelector('#sheet').hidden)()", 10_000);
  await b.waitFor("(() => (document.body.textContent || '').includes('e2e walk'))()", 10_000);
  console.log("panel-data: ok (empty catalog + analysis walk)");
}, { plugin: "data", view: "#data" });
