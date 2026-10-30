// The second-language pass (panel proof-of-life): the 文/A button (#langBtn,
// i18n.ts) flips the whole panel to zh-CN. documentElement.lang flips AND some
// painted string actually reads Chinese (rail labels stay English by design -
// they are gateway-served wireLabel text, SPEC panel.i18n - so the CJK proof
// is body-wide). Errorless throughout.
import assert from "node:assert/strict";
import { withPanel } from "./browser.mjs";

await withPanel(async (b) => {
  await b.waitFor("(() => !!document.querySelector('#langBtn'))()");
  assert.equal(await b.eval("(() => document.documentElement.lang)()"), "en", "starts in English");
  await b.visibleOk("#langBtn");
  await b.click("#langBtn");
  await b.waitFor("(() => document.documentElement.lang === 'zh-CN')()", 10_000);
  // The flip is real paint, not a tag: some string on the page reads Chinese.
  await b.waitFor("(() => /[\\u4e00-\\u9fff]/.test(document.body.textContent || ''))()", 10_000);
  // The toggle's own title swaps sides in zh mode (i18n.ts: the way back).
  const title = await b.eval("(() => document.querySelector('#langBtn').title)()");
  assert.equal(title, "Switch to English", "the way back is titled in zh mode: " + JSON.stringify(title));
  console.log("panel-settings: ok (zh pass)");
});