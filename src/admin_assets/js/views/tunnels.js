import { state } from "../util.js";
import { loadList, loadTunnels, tunData } from "../polling.js";
export function mount() { return loadTunnels(); }
export function refresh() { return loadTunnels(); }
export async function poll() { await loadList(); await loadTunnels(true); }
export function countText() { var d = tunData(); return d.rules.length + " rules · " + d.rules.filter(function (r) { return r.state === "up"; }).length + " active"; }
export function unmount() { state.tun.data = null; state.tun.keys = null; state.tun.dragging = null; }
