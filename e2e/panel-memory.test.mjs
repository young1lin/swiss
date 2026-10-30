// Memory view, real browser: plugin enabled, rail seat reached by a real click,
// the projects surface paints visibly on an empty home, errorless. Then the
// auto-update entry (SPEC §memory.panel): a project's head button says the daily
// review is off, opens its sheet visibly, a real-click save turns the review on
// and the head, the stored settings and the Review bar say so; off again; and
// the sheet read once more in Chinese.
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { withPanel } from "./browser.mjs";
import { api, BASE, waitReady } from "./helpers.mjs";

// The checkout the project is bound to, outside the e2e home: the sheet reads the
// project's sync, which needs a directory reachable on this machine.
const repo = mkdtempSync(join(tmpdir(), "swiss-e2e-auto-"));
mkdirSync(join(repo, ".git"));

async function walk(b) {
  await b.waitFor("(() => !!document.querySelector('.rail-btn[data-view=\"memory\"]'))()", 20_000);
  await b.click(".rail-btn[data-view=\"memory\"]");
  await b.waitFor("(() => location.hash === '#memory')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");

  // A project of its own: the global scope has no daily review, so no button.
  await waitReady("/api/knowledge/projects");
  const made = api("POST", "/api/knowledge/projects", { name: "e2e-auto" });
  assert.equal(made.status, 0, "project create: " + made.stderr + made.stdout);
  const id = made.body.project.id;
  const bound = api("POST", "/api/knowledge/projects/" + id + "/roots", { path: repo });
  assert.equal(bound.status, 0, "bind the directory: " + bound.stderr + bound.stdout);
  await b.navigate(BASE + "/#mcps");
  await b.waitFor("(() => !!document.querySelector('.rail-btn[data-view=\"memory\"]'))()", 20_000);
  await b.click(".rail-btn[data-view=\"memory\"]");
  // The page keeps the project it showed (Global, on the empty home): pick the new one.
  const row = "#memRail [data-mproject=\"" + id + "\"]";
  await b.waitFor("(() => !!document.querySelector(" + JSON.stringify(row) + "))()");
  await b.click(row);
  await b.waitFor("(() => !!document.querySelector('#memAutoOpen'))()");
  const head = await b.visibleOk("#memAutoOpen");
  assert.equal(head.text, "Auto-update · off", "the head says the review is off: " + JSON.stringify(head));

  // The sheet opens visibly; the time shows only once the switch is on.
  await b.click("#memAutoOpen");
  await b.waitFor("(() => !document.querySelector('#sheet').hidden && !!document.querySelector('#memAutoStatus'))()");
  await b.visibleOk("#sheet .sheet");
  assert.equal(await b.eval("(() => document.querySelector('#sheet h2').textContent)()"), "Auto-update");
  await b.visibleOk("#memAutoStatus");
  await b.visibleOk("#memAutoHooks");
  assert.equal((await b.visible("#memAutoReviewAt")).w, 0, "the time hides while the review is off");
  await b.click("#memAutoReviewOn");
  await b.visibleOk("#memAutoReviewAt");
  await b.visibleOk("#memAutoReviewRuns");
  // A time control takes no typed text: set it as a picker would.
  await b.eval("(() => { const el = document.querySelector('#memAutoReviewAt'); el.value = '21:30';" +
    " el.dispatchEvent(new Event('input', { bubbles: true })); el.dispatchEvent(new Event('change', { bubbles: true })); })()");
  await b.click("#memAutoSave");
  await b.waitFor("(() => document.querySelector('#sheet').hidden)()");
  await b.waitFor("(() => (document.querySelector('#memAutoOpen') || {}).textContent === 'Auto-update · daily 21:30')()");
  const saved = api("GET", "/api/knowledge/projects/" + id + "/settings");
  assert.equal(saved.status, 0, "settings read: " + saved.stderr);
  assert.equal(saved.body.settings.autoReview, "21:30", "the PATCH reached the store: " + saved.stdout);

  // The Review bar says the review's time and opens the sheet again.
  await b.click("[data-mtab=review]");
  await b.waitFor("(() => /Daily review at 21:30/.test((document.querySelector('#memReviewAuto') || {}).textContent || ''))()");
  await b.visibleOk("#memReviewAuto button");
  await b.click("#memReviewAuto button");
  await b.waitFor("(() => !document.querySelector('#sheet').hidden && !!document.querySelector('#memAutoReviewOn'))()");
  assert.equal(await b.eval("(() => document.querySelector('#memAutoReviewOn').checked)()"), true);

  // Off again: the time cleared in the store, the head back to off.
  await b.click("#memAutoReviewOn");
  await b.click("#memAutoSave");
  await b.waitFor("(() => document.querySelector('#sheet').hidden)()");
  await b.waitFor("(() => (document.querySelector('#memAutoOpen') || {}).textContent === 'Auto-update · off')()");
  assert.equal(api("GET", "/api/knowledge/projects/" + id + "/settings").body.settings.autoReview, null);

  // The second-language pass: the head and the sheet read Chinese.
  await b.click("#langBtn");
  await b.waitFor("(() => document.documentElement.lang === 'zh-CN')()", 10_000);
  await b.waitFor("(() => (document.querySelector('#memAutoOpen') || {}).textContent === '自动更新 · 未开启')()");
  await b.click("#memAutoOpen");
  await b.waitFor("(() => !document.querySelector('#sheet').hidden && document.querySelector('#sheet h2').textContent === '自动更新')()");
  await b.visibleOk("#memAutoStatus");
  console.log("panel-memory: ok (auto-update walked, zh pass)");
}

try {
  await withPanel(walk, { plugin: "memory", view: "#memory" });
} finally {
  rmSync(repo, { recursive: true, force: true });
}
