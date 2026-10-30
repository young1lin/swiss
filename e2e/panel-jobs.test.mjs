// Jobs view, real browser: the schedule surface paints, errorless.
import { withPanel } from "./browser.mjs";

await withPanel(async (b) => {
  await b.waitFor("(() => !!document.querySelector('.rail-btn[data-view=\"jobs\"]'))()");
  await b.click(".rail-btn[data-view=\"jobs\"]");
  await b.waitFor("(() => location.hash === '#jobs')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");
  console.log("panel-jobs: ok");
}, { view: "#jobs" });