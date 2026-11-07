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
import { BASE } from "./helpers.mjs";

await withPanel(async (b) => {
  await b.navigate(BASE + "/?navigation-default=1");
  await b.waitFor("(() => location.hash === '#mcps')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");
  await b.navigate("http://127.0.0.1:" + (process.env.SWISS_E2E_PORT || "18999") + "/#traffic");
  await b.waitFor("(() => location.hash === '#mcps-traffic')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");
  assert.equal(await b.eval("(() => document.querySelector('#pageTabs a[aria-current=page]').getAttribute('href'))()"), "#mcps-traffic");
  await b.click('#pageTabs a[href="#mcps-tokens"]');
  await b.waitFor("(() => location.hash === '#mcps-tokens')()");
  await b.visibleOk("#pane > *");
  await b.navigate("http://127.0.0.1:" + (process.env.SWISS_E2E_PORT || "18999") + "/#tokens");
  await b.waitFor("(() => location.hash === '#mcps-tokens')()");
  await b.visibleOk("#pane > *");
  await b.click('#pageTabs a[href="#mcps-traffic"]');
  await b.waitFor("(() => location.hash === '#mcps-traffic')()");
  await b.visibleOk("#pane > *");

  // A fresh document must resolve a canonical child route, not fall back to Servers.
  await b.navigate(BASE + "/?navigation-cold=1#mcps-tokens");
  await b.waitFor("(() => !!document.querySelector('#tkCreate'))()");
  assert.equal(await b.eval("(() => location.hash)()"), "#mcps-tokens");

  for (const width of [1440, 960]) {
    await b.send("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false }, b.session);
    for (const lang of ["en", "zh-CN"]) {
      if (await b.eval("(() => document.documentElement.lang)()") !== lang) await b.click("#langBtn");
      await b.waitFor("(() => document.documentElement.lang === " + JSON.stringify(lang) + ")()");
      for (const theme of ["light", "dark"]) {
        if (await b.eval("(() => document.documentElement.dataset.theme)()") !== theme) await b.click("#themeBtn");
        for (const [route, control] of [["mcps", ".sidebar"], ["mcps-traffic", "#trMore"], ["mcps-tokens", "#tkCreate"]]) {
          await b.click('#pageTabs a[href="#' + route + '"]');
          await b.waitFor("(() => location.hash === " + JSON.stringify("#" + route) + ")()");
          await b.waitFor("(() => { const el = document.querySelector(" + JSON.stringify(control) + ");" +
            " const tab = document.querySelector('#pageTabs a[aria-current=page]');" +
            " return tab && tab.getAttribute('href') === " + JSON.stringify("#" + route) +
            " && el && !el.hidden && el.getBoundingClientRect().height > 0; })()");
          await b.visibleOk(control);
          await b.visibleOk('.rail-btn[data-group="mcp"][aria-current="true"]');
          assert.equal(await b.eval("(() => document.documentElement.scrollWidth <= innerWidth)()"), true, "no horizontal overflow");
        }
      }
    }
  }

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
