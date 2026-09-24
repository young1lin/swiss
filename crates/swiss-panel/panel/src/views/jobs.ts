/*
 * Copyright 2026 young1lin
 * 
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 * 
 *     https://www.apache.org/licenses/LICENSE-2.0
 * 
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

import { $ } from "../util.js";
import { fill } from "../h.js";
import { jobsChipText, loadJobs } from "../polling.js";
import { probeJobs } from "../jobs.js";
import { pluginInventory } from "../page-registry.js";
import { clearJobsView } from "../job-state.js";
import { tr } from "../i18n.js";
import { emptyNode } from "../ui/page.js";

/** On a gateway without the jobs subsystem (no inventory API and a failed probe) the view says
 *  so once instead of parking on its loading placeholder — the row the tab could still show.
 *  The library's one empty-state shape (docs/18 V7), the last two ui.css tokens in a view. */
function unavailable() {
  fill($("pane"), emptyNode({
    icon: "clock",   // the sprite's own clock (i-calendar does not exist)
    title: tr("jobs.jobsUnavailable"),
    hint: tr("jobs.gatewayServeJobsSubsystem"),
  }));
}

export async function mount() {
  if (!pluginInventory() && !(await probeJobs())) { unavailable(); return; }
  await loadJobs();
}
export function refresh() { return loadJobs(); }
export function poll() { return loadJobs(true); }
export function countText() { return jobsChipText(); }
export function unmount() { clearJobsView(); }
