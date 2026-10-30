// Process view, real browser: the apps surface paints on a home with no apps,
// the view is reached by a real rail click, errorless.
import assert from "node:assert/strict";
import { withPanel } from "./browser.mjs";

await withPanel(async (b) => {
  await b.waitFor("(() => !!document.querySelector('.rail-btn[data-view=\"process\"]'))()");
  await b.click(".rail-btn[data-view=\"process\"]");
  await b.waitFor("(() => location.hash === '#process')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");
  console.log("panel-process: ok");
}, { view: "#process" });