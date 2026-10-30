// Remote views, real browser: the group's rail seat (Targets, pages[0]) is
// clicked for real, then the Runs tab is reached by hash, both panes visible,
// errorless.
import { withPanel } from "./browser.mjs";

await withPanel(async (b) => {
  await b.waitFor("(() => !!document.querySelector('.rail-btn[data-view=\"remote\"]'))()", 20_000);
  await b.click(".rail-btn[data-view=\"remote\"]");
  await b.waitFor("(() => location.hash === '#remote')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");
  await b.navigate("http://127.0.0.1:" + (process.env.SWISS_E2E_PORT || "18999") + "/#remote-runs");
  await b.waitFor("(() => location.hash === '#remote-runs')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");
  console.log("panel-remote: ok");
}, { plugin: "remote", view: "#remote" });