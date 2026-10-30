// The panel shell, end to end in a real browser: the one-time ticket spends,
// the rail paints, hash navigation reaches a view, and the page stays errorless.
import assert from "node:assert/strict";
import { withPanel } from "./browser.mjs";

await withPanel(async (b) => {
  // The rail painted with labeled seats (the plugin inventory loaded).
  const labels = await b.eval("(() => Array.from(document.querySelectorAll('.rail-btn .rail-label')).map((e) => e.textContent.trim()).filter(Boolean).join('|'))()");
  assert.ok(labels.length > 0, "rail labels painted: " + labels);
  // The default view is visible: title bar not hidden, pane populated and sized.
  await b.visibleOk("#pageTitle");
  const pane = await b.visible("#pane");
  assert.ok(pane && !pane.hidden && pane.w > 0, "pane visible: " + JSON.stringify(pane));
  assert.ok(pane.h > 0, "pane has height: " + JSON.stringify(pane));
  // A real click on the terminal-less builtin seat: data (order 40) is always registered.
  await b.waitFor("(() => !!document.querySelector('.rail-btn[data-view=\"data\"]'))()");
  await b.click(".rail-btn[data-view=\"data\"]");
  await b.waitFor("(() => location.hash === '#data')()");
  assert.equal(await b.eval("(() => location.hash)()"), "#data", "the click navigated");
  await b.visibleOk("#pageTitle");
  console.log("panel-shell: ok");
});