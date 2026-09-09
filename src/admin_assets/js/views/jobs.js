import { $, state } from "../util.js";
import { jobsChipText, loadJobs } from "../polling.js";
import { probeJobs } from "../jobs.js";
import { pluginInventory } from "../page-registry.js";

/** On a gateway without the jobs subsystem (no inventory API and a failed probe) the view says
 *  so once instead of parking on its loading placeholder — the row the tab could still show. */
function unavailable() {
  $("pane").innerHTML =
    '<div class="empty"><div><h2>Jobs unavailable</h2>' +
    '<p class="hint">This gateway does not serve the jobs subsystem.</p></div></div>';
}

export async function mount() {
  if (!pluginInventory() && !(await probeJobs())) { unavailable(); return; }
  await loadJobs();
}
export function refresh() { return loadJobs(); }
export function poll() { return loadJobs(true); }
export function countText() { return jobsChipText(); }
export function unmount() { state.jobs.data = []; state.jobs.hist = null; state.jobs.painted = ""; }
