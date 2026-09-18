/*
 * Copyright 2026 The swiss authors
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

import type { ApiJobRow, ApiJobRunRecord } from "./types/api.js";

/* The jobs domain owns its state (docs/37 R4, slice 5 of 7): the rows, the scope's group family
   and fold map, the per-row verb in flight, the structural signature the poll compares before it
   rebuilds, the paged run history the sheet renders, and the drag/staging slots.

   A leaf module, same reason as tunnel-state: jobs.ts renders and jobs.ts imports polling.ts,
   which loads — hosting the record in either closes a cycle. This file imports types only. */
const jobs: {
  data: ApiJobRow[];
  groups: string[];
  collapsed: Record<string, boolean>;
  busy: Record<string, boolean>;
  painted: string;
  hist: { name: string; runs: ApiJobRunRecord[] } | null;
  dragging: string | null;
  draggingGroup: string | null;
  pendingGroup: string | null;
} = {
  data: [],              // rows from /api/jobs
  groups: ["default"],   // the scope's group names, from /api/jobs' top level (docs/20 G4)
  collapsed: {},         // jobs fold map; group name -> true (localStorage, groups.ts)
  busy: {},              // name -> a run is in flight
  painted: "",           // structural signature of the drawn list; a change means the rows move
  hist: null,            // the run-history sheet's paged records for ONE job
  dragging: null,        // name of the ROW being dragged — polls must not rebuild under it
  draggingGroup: null,   // name of the GROUP HEADER being dragged — same rebuild freeze
  pendingGroup: null,    // group chosen via a header "+", preselected in the sheet it opens
};

/** The rows, as the last /api/jobs answer left them. */
export function jobRows(): ApiJobRow[] { return jobs.data; }
export function setJobRows(rows: ApiJobRow[]): void { jobs.data = rows; }

/** The scope's group names. An older gateway answers none: the single default group. */
export function jobGroupNames(): string[] { return jobs.groups; }
export function setJobGroupNames(names: string[]): void { jobs.groups = names; }

/** The fold map, handed out by reference — the groups component writes group keys into it. */
export function jobFolds(): Record<string, boolean> { return jobs.collapsed; }
export function setJobFolds(map: Record<string, boolean>): void { jobs.collapsed = map; }

/** One row with a run in flight: the dot says "running" and the row must not start another. */
export function jobIsBusy(name: string): boolean { return !!jobs.busy[name]; }
export function setJobBusy(name: string): void { jobs.busy[name] = true; }
export function clearJobBusy(name: string): void { delete jobs.busy[name]; }

/** The structural signature of the painted list. The 6s poll compares before it rebuilds, so a
 *  refresh that changes nothing cannot collapse a group or drop the row under the pointer. */
export function paintedJobsSig(): string { return jobs.painted; }
export function setPaintedJobsSig(sig: string): void { jobs.painted = sig; }

/** The run-history sheet's records, for one job at a time. startJobHistory resets the buffer when
 *  the sheet opens on a different job or without a cursor; appendJobRuns adds one fetched page. */
export function jobHistory(): { name: string; runs: ApiJobRunRecord[] } | null { return jobs.hist; }
export function startJobHistory(name: string): void { jobs.hist = { name: name, runs: [] }; }
export function jobHistoryIsFor(name: string): boolean { return !!jobs.hist && jobs.hist.name === name; }
export function appendJobRuns(runs: ApiJobRunRecord[]): void {
  if (!jobs.hist) return;
  jobs.hist.runs = jobs.hist.runs.concat(runs);
}

/** The two drag slots. While either is set a poll defers its rebuild — see patchJobs. */
export function jobDragging(): string | null { return jobs.dragging; }
export function setJobDragging(name: string | null): void { jobs.dragging = name; }
export function jobDraggingGroup(): string | null { return jobs.draggingGroup; }
export function setJobDraggingGroup(name: string | null): void { jobs.draggingGroup = name; }

/** The group a header "+" staged for the sheet it is about to open. */
export function setJobPendingGroup(group: string | null): void { jobs.pendingGroup = group; }
/** Read it and clear it in one move — the sheet consumes the staged group exactly once. */
export function takeJobPendingGroup(): string | null {
  const group = jobs.pendingGroup;
  jobs.pendingGroup = null;
  return group;
}

/** Leaving the Jobs page: drop the rows, the history buffer and the paint signature, so a return
 *  paints from a fresh load. The fold map and the group family are settings, and survive. */
export function clearJobsView(): void {
  jobs.data = [];
  jobs.hist = null;
  jobs.painted = "";
}
