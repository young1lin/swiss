import { KINDS, state } from "../util.js";
import { loadList, mcpChipText } from "../polling.js";
import { loadCalls, loadMeta, loadPage, pageState } from "../detail.js";
import { renderPane } from "../pane.js";
import { histClose } from "../run-history.js";
export async function mount() { await loadList(); renderPane(); }
export async function poll() { await loadList(); if (state.detail && state.detail.tab === "logs") await loadCalls(state.detail.name); }
export async function refresh() {
  await poll();
  var d = state.detail;
  if (!d) return;
  await loadMeta(d.name);
  if (KINDS.indexOf(d.tab) >= 0) { d[d.tab] = pageState(); await loadPage(d.name, d.tab); }
}
export function countText() { return mcpChipText(); }
export function unmount() { histClose(); }
