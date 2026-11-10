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
import { patchJobs, probeJobs } from "../jobs.js";
import { busOn } from "../bus/bus.js";
import { coalescedRead } from "../bus/live.js";
import { Module, NoticeSchema } from "../bus/pb/swiss/bus/v1/bus_pb.js";
import { HostSub, RunChangedSchema, RunsId } from "../bus/pb/swiss/bus/v1/host_pb.js";
                                                                    
import { JobsListId, JobsSub } from "../bus/pb/swiss/bus/v1/jobs_pb.js";
import { pluginInventory } from "../page-registry.js";
import { clearJobsView } from "../job-state.js";
import { tr } from "../i18n.js";
import { emptyNode } from "../ui/page.js";

/** On a gateway without the jobs subsystem (no inventory API and a failed probe) the view says
 *  so once instead of parking on its loading placeholder — the row the tab could still show.
 *  The library's one empty-state shape (SPEC §panel.design), the last two ui.css tokens in a view. */
function unavailable() {
  fill($("pane"), emptyNode({
    icon: "clock",   // the sprite's own clock (i-calendar does not exist)
    title: tr("jobs.jobsUnavailable"),
    hint: tr("jobs.gatewayServeJobsSubsystem"),
  }));
}

let stopList                      = null;
let stopRuns                      = null;
let jobsRead                                        = null;

/** JOBS.LIST, and HOST.RUNS whose owner is jobs, coalesced into one list read (SPEC §jobs.panel). */
export function followJobs()       {
  if (stopList) return;
  const read = coalescedRead(() => loadJobs(true));
  jobsRead = read;
  stopList = busOn(
    { module: Module.JOBS, sub: JobsSub.LIST, key: "" },
    JobsListId.JOBS_LIST_CHANGED,
    NoticeSchema,
    () => { read.push(); },
  );
  stopRuns = busOn(
    { module: Module.HOST, sub: HostSub.RUNS, key: "" },
    RunsId.RUNS_CHANGED,
    RunChangedSchema,
    (msg            ) => { if (msg.owner === "jobs") read.push(); },
  );
}

export async function mount() {
  if (!pluginInventory() && !(await probeJobs())) { unavailable(); return; }
  followJobs();
  await loadJobs();
}
export function refresh() { return loadJobs(); }
/** Relative last/next, from the rows already held. No request (SPEC §jobs.panel). */
export function clock()       { patchJobs(); }
export function countText() { return jobsChipText(); }
export function unmount() {
  clearJobsView();
  if (stopList) stopList();
  if (stopRuns) stopRuns();
  stopList = null;
  stopRuns = null;
  if (jobsRead) jobsRead.stop();
  jobsRead = null;
}
