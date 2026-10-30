// Data view, real browser: the empty catalog paints an honest empty state,
// reached by a real rail click, errorless.
import assert from "node:assert/strict";
import { withPanel } from "./browser.mjs";

await withPanel(async (b) => {
  await b.waitFor("(() => !!document.querySelector('.rail-btn[data-view=\"data\"]'))()");
  await b.click(".rail-btn[data-view=\"data\"]");
  await b.waitFor("(() => location.hash === '#data')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");
  console.log("panel-data: ok");
}, { plugin: "data", view: "#data" });