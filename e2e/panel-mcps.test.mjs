// MCP views, real browser: Servers (the default view) paints with its sidebar
// layout, Traffic opens too, errorless.
import { withPanel } from "./browser.mjs";

await withPanel(async (b) => {
  await b.waitFor("(() => location.hash === '#mcps')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");
  await b.navigate("http://127.0.0.1:" + (process.env.SWISS_E2E_PORT || "18999") + "/#traffic");
  await b.waitFor("(() => location.hash === '#traffic')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");
  console.log("panel-mcps: ok");
}, { view: "#mcps" });