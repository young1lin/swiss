// A minimal Chrome DevTools Protocol driver for the panel's browser e2e:
// no dependency beyond Node >= 22's global WebSocket. The browser is launched
// with a private user-data-dir and remote debugging on 19222 (never the
// production profile); only the PID this file spawned is ever killed. Real
// pointer input goes through Input.dispatchMouseEvent, never element.click().
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { api, BASE } from "./helpers.mjs";

export const CDP_PORT = process.env.SWISS_E2E_CDP_PORT || "19222";
export const browserAvailable = typeof WebSocket === "function";

function findBrowser() {
  const manual = process.env.SWISS_E2E_BROWSER;
  if (manual) return manual;
  const pf = process.env.ProgramFiles || "C:\\Program Files";
  const pfx86 = process.env["ProgramFiles(x86)"] || "C:\\Program Files (x86)";
  const local = process.env.LOCALAPPDATA || "";
  const candidates = [
    join(pf, "Google", "Chrome", "Application", "chrome.exe"),
    join(pfx86, "Google", "Chrome", "Application", "chrome.exe"),
    join(local, "Google", "Chrome", "Application", "chrome.exe"),
    join(pf, "Microsoft", "Edge", "Application", "msedge.exe"),
    join(pfx86, "Microsoft", "Edge", "Application", "msedge.exe"),
  ];
  for (const c of candidates) if (c && existsSync(c)) return c;
  return null;
}

// One-time session ticket through the CLI signer (app.rs session_ticket): the
// URL spends itself once and leaves the admin cookie behind. Bounded retry:
// a just-booted gateway can answer /health a hair before the session mints
// (observed once as a bare 502 with no error body); five short tries close it.
export async function ticketUrl() {
  let last = null;
  for (let i = 0; i < 5; i++) {
    const r = api("POST", "/api/session/ticket", "{}");
    if (r.status === 0 && r.body && r.body.url) return r.body.url;
    last = r;
    await new Promise((res) => setTimeout(res, 500));
  }
  throw new Error("ticket mint failed: " + (last ? last.stderr + last.stdout : "no attempt"));
}

export class Browser {
  static async launch() {
    if (!browserAvailable) throw new Error("Node < 22: no global WebSocket");
    const exe = findBrowser();
    if (!exe) throw new Error("no Chromium browser found; set SWISS_E2E_BROWSER to a chrome/msedge path");
    const profile = join(tmpdir(), "swiss-e2e-browser-" + process.pid);
    rmSync(profile, { recursive: true, force: true });
    mkdirSync(profile, { recursive: true });
    const child = spawn(exe, [
      "--headless=new",
      "--remote-debugging-port=" + CDP_PORT,
      "--user-data-dir=" + profile,
      "--no-first-run",
      "--no-default-browser-check",
      "--window-size=1440,900",
      "about:blank",
    ], { stdio: "ignore" });
    // The debug endpoint answers when the browser is up; poll it, not a sleep.
    const versionUrl = "http://127.0.0.1:" + CDP_PORT + "/json/version";
    let wsUrl = null;
    const deadline = Date.now() + 20_000;
    for (;;) {
      try {
        const r = await fetch(versionUrl);
        if (r.ok) { wsUrl = (await r.json()).webSocketDebuggerUrl; }
      } catch {}
      if (wsUrl) break;
      if (child.exitCode !== null) throw new Error("browser exited early with code " + child.exitCode);
      if (Date.now() > deadline) {
        try { child.kill(); } catch {}
        throw new Error("browser did not answer on " + versionUrl + " within 20s");
      }
      await new Promise((res) => setTimeout(res, 200));
    }
    const b = new Browser(child, profile);
    await b.connect(wsUrl);
    return b;
  }

  constructor(child, profile) {
    this.child = child;
    this.profile = profile;
    this.ws = null;
    this.nextId = 1;
    this.pending = new Map();
    this.handlers = new Map();
    this.errors = [];
  }

  connect(wsUrl) {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(wsUrl);
      this.ws = ws;
      ws.addEventListener("open", () => resolve());
      ws.addEventListener("error", () => reject(new Error("CDP websocket error: " + wsUrl)));
      ws.addEventListener("message", (ev) => {
        let msg = null;
        try { msg = JSON.parse(ev.data); } catch { return; }
        if (msg.id !== undefined && this.pending.has(msg.id)) {
          const { resolve: ok, reject: bad } = this.pending.get(msg.id);
          this.pending.delete(msg.id);
          if (msg.error) bad(new Error(msg.error.message + " (" + msg.error.code + ")"));
          else ok(msg.result);
          return;
        }
        if (msg.method && this.handlers.has(msg.method)) {
          for (const fn of this.handlers.get(msg.method)) fn(msg.params, msg.sessionId);
        }
      });
    });
  }

  on(method, fn) {
    if (!this.handlers.has(method)) this.handlers.set(method, []);
    this.handlers.get(method).push(fn);
  }

  send(method, params = {}, sessionId) {
    const id = this.nextId++;
    const payload = { id, method, params };
    if (sessionId) payload.sessionId = sessionId;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify(payload));
    });
  }

  // A tab of our own: created, attached (flatten), Page+Runtime+Log enabled.
  async newTab(url) {
    const { targetId } = await this.send("Target.createTarget", { url: url || "about:blank" });
    const { sessionId } = await this.send("Target.attachToTarget", { targetId, flatten: true });
    this.session = sessionId;
    await this.send("Page.enable", {}, sessionId);
    await this.send("Runtime.enable", {}, sessionId);
    await this.send("Log.enable", {}, sessionId);
    // Uncaught page exceptions are collected and asserted by the tests: a dead
    // page must fail the walk, not pass beside it.
    this.on("Runtime.exceptionThrown", (p) => { this.errors.push("exception: " + JSON.stringify(p.exceptionDetails).slice(0, 300)); });
    this.on("Log.entryAdded", (p) => {
      const e = p.entry || {};
      if (e.level === "error") this.errors.push("console.error: " + String(e.text).slice(0, 300));
    });
    return sessionId;
  }

  async navigate(url) {
    const loaded = new Promise((resolve) => {
      const done = () => { this.handlers.delete("Page.loadEventFired"); resolve(); };
      this.on("Page.loadEventFired", done);
      // A same-document hash navigation fires no load event, so the bound is
      // short: real readiness is the caller's waitFor, never this wait.
      setTimeout(done, 2_500);
    });
    await this.send("Page.navigate", { url }, this.session);
    await loaded;
  }

  // Evaluate an async () => ... expression in the page, value back.
  async eval(expression) {
    const r = await this.send("Runtime.evaluate", {
      expression,
      returnByValue: true,
      awaitPromise: true,
    }, this.session);
    if (r.exceptionDetails) throw new Error("page eval failed: " + JSON.stringify(r.exceptionDetails).slice(0, 300) + " in " + expression.slice(0, 120));
    return r.result ? r.result.value : undefined;
  }

  // Poll a () => boolean-ish expression until truthy or timeout.
  async waitFor(expression, timeoutMs = 15_000) {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      let v = null;
      try { v = await this.eval(expression); } catch {}
      if (v) return v;
      if (Date.now() > deadline) throw new Error("waitFor timed out: " + expression.slice(0, 160));
      await new Promise((res) => setTimeout(res, 200));
    }
  }

  // The visibility probe the panel rules demand: not just querySelector truth,
  // but hidden/display/rect - what a human would see.
  async visible(selector) {
    return await this.eval("(() => { const el = document.querySelector(" + JSON.stringify(selector) + ");" +
      " if (!el) return null; const rc = el.getBoundingClientRect(); const cs = getComputedStyle(el);" +
      " return { hidden: !!el.hidden, display: cs.display, w: Math.round(rc.width), h: Math.round(rc.height), text: (el.textContent || \"\").trim().slice(0, 80) }; })()");
  }

  async visibleOk(selector) {
    const v = await this.visible(selector);
    if (!v) throw new Error("not found: " + selector);
    if (v.hidden) throw new Error("hidden: " + selector);
    if (v.display === "none") throw new Error("display:none: " + selector);
    if (v.w <= 0 || v.h <= 0) throw new Error("zero rect: " + selector + " " + JSON.stringify(v));
    return v;
  }

  // A real pointer click at the element's center: hit-testing, overlays and
  // focus all take part, exactly like the user's hand.
  async click(selector) {
    const rc = await this.eval("(() => { const el = document.querySelector(" + JSON.stringify(selector) + ");" +
      " if (!el) return null; el.scrollIntoView({ block: \"center\" }); const r = el.getBoundingClientRect();" +
      " return { x: Math.round(r.x + r.width / 2), y: Math.round(r.y + r.height / 2) }; })()");
    if (!rc) throw new Error("click: no element for " + selector);
    const base = { type: "mousePressed", x: rc.x, y: rc.y, button: "left", clickCount: 1 };
    await this.send("Input.dispatchMouseEvent", base, this.session);
    await this.send("Input.dispatchMouseEvent", { ...base, type: "mouseReleased" }, this.session);
  }

  // Type into a form control: the click focuses with a real pointer event, then
  // Input.insertText commits the text in one go. The pointer discipline binds
  // clicks; typing may arrive like a paste.
  async type(selector, text) {
    await this.click(selector);
    await this.send("Input.insertText", { text }, this.session);
  }

  async press(key, text) {
    await this.send("Input.dispatchKeyEvent", { type: "keyDown", key, text: text || key }, this.session);
    await this.send("Input.dispatchKeyEvent", { type: "keyUp", key }, this.session);
  }

  async screenshot(path) {
    const { data } = await this.send("Page.captureScreenshot", { format: "png" }, this.session);
    const { writeFileSync } = await import("node:fs");
    writeFileSync(path, Buffer.from(data, "base64"));
  }

  async close() {
    // Only the PID this driver spawned; never a name, never the user's browser.
    try { if (this.ws) this.ws.close(); } catch {}
    try { this.child.kill(); } catch {}
    // The profile files are released asynchronously after exit on Windows:
    // wait for the exit, then retry the wipe on a short bounded loop.
    await new Promise((resolve) => {
      if (this.child.exitCode !== null) return resolve();
      const giveUp = setTimeout(resolve, 5_000);
      this.child.once("exit", () => { clearTimeout(giveUp); resolve(); });
    });
    for (let i = 0; i < 15; i++) {
      try { rmSync(this.profile, { recursive: true, force: true }); return; } catch {}
      await new Promise((res) => setTimeout(res, 200));
    }
  }
}

// The common panel walk: boot fresh, enable a plugin, open the browser, spend
// a ticket, wait for the rail, hand the driver over.
export async function withPanel(fn, { plugin = null, view = "#mcps" } = {}) {
  const { withGateway } = await import("./helpers.mjs");
  if (!browserAvailable) {
    console.log("browser: SKIPPED (Node " + process.version + " has no global WebSocket; need >= 22)");
    return;
  }
  await withGateway(async () => {
    const b = await Browser.launch();
    try {
      if (plugin) {
        const { ensurePlugin } = await import("./helpers.mjs");
        ensurePlugin(plugin);
      }
      await b.newTab("about:blank");
      await b.navigate(await ticketUrl());
      await b.waitFor("(() => document.querySelectorAll('.rail-btn').length > 0)()");
      await b.navigate(BASE + "/" + view);
      await fn(b);
      if (b.errors.length > 0) {
        throw new Error("page errors during the walk:\n  " + b.errors.join("\n  "));
      }
    } finally {
      await b.close();
    }
  }, { plugin });
}