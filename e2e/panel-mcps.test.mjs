// MCP views, real browser: Servers (the default view) paints with its sidebar
// layout, Traffic opens too, errorless. Then the desktop first frame
// (mcp-overview.ts): renderPane paints it only when desktopMode() holds -
// __SWISS_DESKTOP__ true plus an ipc bridge on window (desktop.ts). The walk
// stubs exactly that contract after the last full load, then a same-document
// hash round-trip repaints the pane; the built-in echo server (always
// registered, mcp.test.mjs) must appear as an Open row, and that Open button
// must really take the pane to echo's detail.
import assert from "node:assert/strict";
import { withPanel } from "./browser.mjs";

await withPanel(async (b) => {
  await b.waitFor("(() => location.hash === '#mcps')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");
  await b.navigate("http://127.0.0.1:" + (process.env.SWISS_E2E_PORT || "18999") + "/#traffic");
  await b.waitFor("(() => location.hash === '#traffic')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");

  // The desktop window's two globals; set after the full load above so the
  // reload cannot wipe them, and the pane is repainted by the hash round-trip.
  await b.eval("(() => { window.__SWISS_DESKTOP__ = true; window.ipc = { postMessage(){} }; return true; })()");
  await b.eval("(() => { location.hash = '#mcps'; return true; })()");
  await b.waitFor("(() => !!document.getElementById('mcpOverview'))()", 10_000);
  await b.visibleOk("#mcpOverview");
  assert.match(await b.eval("(() => document.getElementById('mcpOverview').textContent)()"), /echo/);
  // The overview's one action per row: Open takes the pane to that server.
  await b.click("#mcpOverview [data-overview-server] button");
  await b.waitFor("(() => !document.getElementById('mcpOverview'))()", 10_000);
  assert.match(await b.eval("(() => document.querySelector('#pane').textContent)()"), /echo/);
  console.log("panel-mcps: ok (views + desktop overview)");
});
