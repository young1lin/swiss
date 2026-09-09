import { state } from "../util.js";
import { loadTraffic, trafficReload } from "../traffic.js";
import { mcpChipText } from "../polling.js";
export function mount() { return trafficReload(true); }
export function refresh() { state.trafficSig = null; return loadTraffic(); }
export function poll() { return loadTraffic(); }
export function countText() { return mcpChipText(); }
export function unmount() { state.traffic = []; state.trafficFull = {}; state.trafficOpen = {}; state.trafficSig = null; }
