// Two walks. First the second-language pass (panel proof-of-life): the 文/A
// button (#langBtn, i18n.ts) flips the whole panel to zh-CN.
// documentElement.lang flips AND some painted string actually reads Chinese
// (rail labels stay English by design - they are gateway-served wireLabel
// text, SPEC panel.i18n - so the CJK proof is body-wide). Errorless throughout.
//
// Then the System page's bundled-skills section (views/system.ts): every
// bundled skill paints as a row with per-destination install state and an
// install/update action. The install button itself is NOT clicked: it writes
// into the operator's real profile roots (~/.claude and peers), which sit
// OUTSIDE the isolated e2e home - planting files there is not this suite's
// to do. The served list painting is the walk's scope.
import assert from "node:assert/strict";
import { withPanel } from "./browser.mjs";

await withPanel(async (b) => {
  await b.waitFor("(() => !!document.querySelector('#langBtn'))()");
  assert.equal(await b.eval("(() => document.documentElement.lang)()"), "en", "starts in English");
  await b.visibleOk("#langBtn");
  await b.click("#langBtn");
  await b.waitFor("(() => document.documentElement.lang === 'zh-CN')()", 10_000);
  // The flip is real paint, not a tag: some string on the page reads Chinese.
  await b.waitFor("(() => /[\u4e00-\u9fff]/.test(document.body.textContent || ''))()", 10_000);
  // The toggle's own title swaps sides in zh mode (i18n.ts: the way back).
  const title = await b.eval("(() => document.querySelector('#langBtn').title)()");
  assert.equal(title, "Switch to English", "the way back is titled in zh mode: " + JSON.stringify(title));
  console.log("panel-settings: ok (zh pass)");
});

await withPanel(async (b) => {
  await b.waitFor("(() => location.hash === '#system')()");
  await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
  await b.visibleOk("#pane > *");
  // The skills section: at least the bundled skill, each location with its
  // install action visible (data-skill rows, data-skill-install buttons).
  await b.waitFor("(() => !!document.querySelector('[data-skill-install]'))()", 10_000);
  await b.visibleOk("[data-skill-install]");
  const rows = await b.eval("(() => document.querySelectorAll('[data-skill]').length)()");
  const actions = await b.eval("(() => document.querySelectorAll('[data-skill-install]').length)()");
  assert.ok(rows >= 1, "bundled skills listed, got " + rows + " row(s)");
  assert.ok(actions >= 1, "install actions listed, got " + actions + " button(s)");
  console.log("panel-settings: ok (system skills: " + rows + " skill(s), " + actions + " action(s))");
}, { view: "#system" });
