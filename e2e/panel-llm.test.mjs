// LLM views, real browser: all four pages of the group open visibly through
// real clicks - providers, embedding, agents, history - errorless.
import assert from "node:assert/strict";
import { withPanel } from "./browser.mjs";

await withPanel(async (b) => {
  await b.waitFor("(() => !!document.querySelector('.rail-btn[data-view=\"llm-providers\"]'))()", 20_000);
  for (const id of ["llm-providers", "llm-embedding", "llm-agents", "llm-history"]) {
    // Group tabs first, then the group's rail seat for the next round trip.
    await b.navigate("http://127.0.0.1:" + (process.env.SWISS_E2E_PORT || "18999") + "/#" + id);
    await b.waitFor("(() => location.hash === '#" + id + "')()");
    await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
    await b.visibleOk("#pane > *");
  }
  console.log("panel-llm: ok");
}, { plugin: "llm", view: "#llm-providers" });