// Client identities survive clearing traffic; their request counts describe retained rows.
// Drive the user's selected-client clear through real pointer events, then resume traffic
// without another handshake and clear everything. Only the isolated e2e home is mutated.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { withPanel } from "./browser.mjs";
import { api, BASE, mcp, toolText } from "./helpers.mjs";

await withPanel(async (b) => {
  const escape = async () => {
    await b.send("Input.dispatchKeyEvent", { type: "keyDown", key: "Escape" }, b.session);
    await b.send("Input.dispatchKeyEvent", { type: "keyUp", key: "Escape" }, b.session);
  };
  const servedHash = await b.eval("(async () => { const r = await fetch('/admin/js/traffic.js'); if (!r.ok) throw new Error('traffic asset refused');" +
    " const hash = await crypto.subtle.digest('SHA-256', await r.arrayBuffer());" +
    " return Array.from(new Uint8Array(hash), n => n.toString(16).padStart(2, '0')).join(''); })()");
  assert.equal(servedHash, createHash("sha256").update(readFileSync("crates/swiss-panel/src/admin_assets/js/traffic.js")).digest("hex"), "the browser receives the fresh Traffic emit");
  const clients = [];
  for (const name of ["traffic-clear-dsh", "traffic-clear-other"]) {
    const minted = api("POST", "/api/tokens", { label: name });
    assert.equal(minted.status, 0, "mint the isolated test client's token");
    assert.ok(minted.body.secret, "a token secret was returned");
    const secret = minted.body.secret;
    const initialized = await mcp("echo", secret, {
      jsonrpc: "2.0", id: 1, method: "initialize",
      params: { protocolVersion: "2025-03-26", capabilities: {}, clientInfo: { name, version: "1" } },
    });
    assert.equal(initialized.status, 200);
    clients.push({ name, secret });
  }
  const target = clients[0];
  const call = async () => {
    const reply = await mcp("echo", target.secret, {
      jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "echo", arguments: { msg: "traffic-count-regression" } },
    });
    assert.equal(reply.status, 200);
    assert.equal(toolText(reply.message).trim(), "traffic-count-regression", "the resumed call really succeeds");
  };
  await call();
  const countExpr = (name, count) => "(() => Array.from(document.querySelector('#trClient')?.options || []).some(o => o.value === " +
    JSON.stringify("n:" + name) + " && o.textContent === " + JSON.stringify(name + " 1 · " + count) + "))()";
  const chooseClient = async (name) => {
    await b.waitFor("(() => !!document.querySelector('#trClient + .dd'))()");
    const value = name ? "n:" + name : "";
    const changed = await b.eval("(() => document.querySelector('#trClient').value !== " + JSON.stringify(value) + ")()");
    const prior = await b.send("Runtime.evaluate", { expression: "document.querySelector('#trClient')" }, b.session);
    const index = await b.eval("(() => Array.from(document.querySelector('#trClient').options).findIndex(o => o.value === " + JSON.stringify(value) + "))()");
    assert.ok(index >= 0, "client is available in the shared dropdown");
    await b.click("#trClient + .dd");
    await b.visibleOk('.dd-menu[role="listbox"]');
    await b.click('.dd-menu[role="listbox"] [role="option"]:nth-child(' + (index + 1) + ')');
    await b.waitFor("(() => document.querySelector('#trClient').value === " + JSON.stringify(value) + ")()");
    if (changed) {
      // The native value changes before the server replies. Wait for the new head,
      // so the next real pointer event cannot land on a control while it moves.
      const deadline = Date.now() + 15_000;
      for (;;) {
        const state = await b.send("Runtime.callFunctionOn", {
          objectId: prior.result.objectId, functionDeclaration: "function() { return !this.isConnected; }", returnByValue: true,
        }, b.session);
        if (state.result.value) break;
        if (Date.now() > deadline) throw new Error("the selected client's server response did not repaint");
        await new Promise(resolve => setTimeout(resolve, 100));
      }
    }
    await b.send("Runtime.releaseObject", { objectId: prior.result.objectId }, b.session);
  };

  await b.navigate(BASE + "/?traffic-clear=1#mcps-traffic");
  await b.waitFor(countExpr(target.name, 2));
  await b.waitFor(countExpr(clients[1].name, 1));
  await b.click(".tl-sum");
  await b.waitFor("(() => document.querySelector('.tl-item.open .tl-body')?.textContent.includes('traffic-count-regression'))()");
  await b.visibleOk(".tl-item.open .tl-body");
  await b.click(".tl-sum");

  // The shared select, content head and event list agree with Remote Runs across the
  // supported widths, themes and languages. Every choice uses real pointer input.
  for (const width of [1440, 960]) {
    await b.send("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false }, b.session);
    for (const lang of ["en", "zh-CN"]) {
      if (await b.eval("(() => document.documentElement.lang)()") !== lang) await b.click("#langBtn");
      await b.waitFor("(() => document.documentElement.lang === " + JSON.stringify(lang) + ")()");
      for (const theme of ["light", "dark"]) {
        if (await b.eval("(() => document.documentElement.dataset.theme)()") !== theme) await b.click("#themeBtn");
        await b.waitFor("(() => !!document.querySelector('#trClient + .dd'))()");
        await b.visibleOk(".pane-head #trClient + .dd");
        assert.equal(await b.eval("(() => !!document.querySelector('.cli-row, .cli-head, #pane .sec-cap'))()"), false);
        await chooseClient(target.name);
        assert.equal(await b.eval("(() => document.querySelector('#trClient').getAttribute('aria-label'))()"), lang === "en" ? "Filter by client" : "按客户端筛选");
        await b.click('[data-filter="all"]');
        await b.waitFor("(() => document.querySelector('[data-filter=all]').getAttribute('aria-selected') === 'true')()");
        await b.click('[data-filter="actions"]');
        await b.waitFor("(() => document.querySelector('[data-filter=actions]').getAttribute('aria-selected') === 'true')()");
        await b.click("#trMore");
        await b.visibleOk('[role="menuitem"]');
        assert.equal(await b.eval("(() => document.querySelector('[role=menuitem]').textContent.trim())()"), lang === "en" ? "Clear client" : "清除客户端");
        await escape();
        await chooseClient("");
        assert.equal(await b.eval("(() => document.documentElement.scrollWidth <= innerWidth)()"), true, "no horizontal overflow");
      }
    }
  }
  if (process.env.SWISS_E2E_SCREENSHOT) {
    await b.click("#trClient + .dd");
    await b.visibleOk('.dd-menu[role="listbox"]');
    await b.screenshot(process.env.SWISS_E2E_SCREENSHOT);
    await escape();
  }
  if (await b.eval("(() => document.documentElement.lang)()") !== "en") await b.click("#langBtn");
  await b.waitFor("(() => document.documentElement.lang === 'en')()");
  await chooseClient(target.name);
  await b.click("#trMore");
  await b.visibleOk('[role="menuitem"]');
  assert.equal(await b.eval("(() => document.querySelector('[role=menuitem]').textContent.trim())()"), "Clear client");
  await b.click('[role="menuitem"]');
  await b.waitFor(countExpr(target.name, 0));
  await b.waitFor(countExpr(clients[1].name, 1));
  const cleared = api("GET", "/api/traffic?client=" + encodeURIComponent("n:" + target.name));
  assert.equal(cleared.status, 0);
  assert.equal(cleared.body.total, 0, "the count and actual records agree");
  assert.equal(cleared.body.clients.find(c => c.key === "n:" + target.name).count, 0);

  await call();
  await b.navigate(BASE + "/?traffic-clear=2#mcps-traffic");
  await b.waitFor(countExpr(target.name, 1));
  await b.waitFor(countExpr(clients[1].name, 1));
  await b.click("#trMore");
  await b.visibleOk('[role="menuitem"]');
  assert.equal(await b.eval("(() => document.querySelector('[role=menuitem]').textContent.trim())()"), "Clear");
  await b.click('[role="menuitem"]');
  await b.waitFor(countExpr(target.name, 0));
  await b.waitFor(countExpr(clients[1].name, 0));
  const all = api("GET", "/api/traffic");
  assert.equal(all.status, 0);
  assert.equal(all.body.total, 0);
  assert.ok(all.body.clients.every(c => c.count === 0));
  await b.waitFor("(() => !!document.querySelector('#pane .empty'))()");
  console.log("panel-traffic-clear: ok (selected client, preserved identity, resumed requests, clear all)");
}, { view: "#mcps-traffic" });
