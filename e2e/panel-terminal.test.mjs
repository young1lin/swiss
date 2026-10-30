// Terminal view, real browser: the plugin enables, the rail seat appears, the
// view opens visibly with its toolbar, and the page stays errorless.
import assert from "node:assert/strict";
import { withPanel } from "./browser.mjs";

await withPanel(async (b) => {
  await b.waitFor("(() => !!document.querySelector('.rail-btn[data-view=\"terminal\"]'))()", 20_000);
  await b.click(".rail-btn[data-view=\"terminal\"]");
  await b.waitFor("(() => location.hash === '#terminal')()");
  // The landing view's pane and title linger until the terminal view paints, so wait for
  // the title to turn over rather than for any child in #pane (the MCP view's would do).
  await b.waitFor("(() => /terminal/i.test((document.querySelector('#pageTitle') || {}).innerText || ''))()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");
  const title = await b.visible("#pageTitle");
  assert.ok(title && title.text.toLowerCase().includes("terminal"), "title names the view: " + JSON.stringify(title));
  console.log("panel-terminal: ok");
}, { plugin: "terminal", view: "#terminal" });