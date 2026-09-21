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

/* Ambient shapes owned by the runs/remote modules (run.ts, run-history.ts,
   views/remote.ts, views/remote-runs.ts) - docs/36 D6/D11. The cross-module families stay in
   types/{api,dom,state}.d.ts; a shape only one of these four files reads lives here so no
   shared file has to change for their migration. Wire shapes are field-for-field from the
   Rust struct that serializes them, like api.d.ts; panel-side shapes say which module owns
   the literal. */

/* --- run.ts / run-history.ts -------------------------------------------------------------------- */

/** POST /api/mcps/{name}/resource (readResource): one resource read's reply. text is the
 *  body, mimeType rides when the source sent one, ms is the round trip. */
import type { ApiMcpRevisionRow, ApiMcpTunnelDep, ApiRemoteTargetRow } from "./api.js";
export interface ApiMcpResourceRead {
  ok: boolean;
  text?: string;
  mimeType?: string;
  ms?: number;
}

/** The result literal runTool stamps on d.run.result (renderRunResult reads it back).
 *  McpRunState keeps the field unknown because only this module writes it. */
export interface McpRunResult {
  ok: boolean;
  text: string;
  ms?: number | null;
}

/** One parked definition revision as configBody renders it (d.revisions rows) - the
 *  /revisions wire row, one name for the reader (types/api.d.ts ApiMcpRevisionRow). */
export type McpRevisionRow = ApiMcpRevisionRow;

/** One tunnel forward a /details answer carries for this MCP (tunnelDepsNode) - the
 *  /details wire row, one name for the reader (types/api.d.ts ApiMcpTunnelDep). */
export type McpTunnelDepRow = ApiMcpTunnelDep;

/** The masked MCP config as configTarget reads it: a Record<string, unknown> on the wire
 *  (d.config, every adapter's own def shape behind mask_def); the fields the adapter
 *  branches touch, named so the reads stay checked. port is the mysql / redis defs' own
 *  (a number in the def, a string when the form last wrote it). */
export interface McpConfigLike {
  type?: string;
  command?: string;
  url?: string;
  baseUrl?: string;
  host?: string;
  port?: number | string;
  db?: string;
  database?: string;
  mode?: string;
}

/* --- views/remote.ts ---------------------------------------------------------------------------- */

/** One live remote endpoint row (/api/remote/endpoints' list, api.rs:262). */
export interface RemoteEndpointRow {
  id: string;
  label: string;
  state: string;
}

/** A remote target row as the Targets page holds it: the wire row with group widened to
 *  the null a member-removal writes back locally before the server's answer lands
 *  (ApiRemoteTargetRow.group is string on the wire). */
export type RemoteTargetRow = Omit<ApiRemoteTargetRow, "group"> & { group: string | null };

/** The Add/Edit sheet's POST body (save): id only on a create (the URL carries it on edit). */
export interface RemoteTargetBody {
  id?: string;
  label: string;
  endpoint: string;
  group: string | null;
  workspaceRoot: string;
  capabilities: string[];
  shell: string;
}

import type { ApiRunRow } from "./api.js";

/* --- views/remote-runs.ts ----------------------------------------------------------------------- */

/** The input half of a remote run row: the validated action input, recorded with env
 *  reduced to envKeys (history.rs sanitized_input). The keys are the union of the five
 *  capabilities' field lists (swiss-remote/src/actions.rs EXEC_FIELDS / SYNC_FIELDS /
 *  PULL_FIELDS and the cat / write pairs); target is on every kind, the rest per kind:
 *  exec argv/cwd/timeoutMs/envKeys, sync source/exclude/verbose/to, pull remote/to/verbose,
 *  cat remote, write remote/content. */
export interface RemoteRunInput {
  target?: string;
  argv?: string[];
  envKeys?: string[];
  cwd?: string;
  timeoutMs?: number;
  source?: string;
  exclude?: string[];
  verbose?: boolean;
  to?: string;
  remote?: string;
  content?: string;
}

/** A remote run's meta as actions.rs stamps it: target/endpoint on every kind, the sync
 *  report counters and outputTruncated per kind. A type alias, not an interface, so it
 *  stays assignable to ApiRunRow's Record<string, unknown> meta. */
export type RemoteRunMeta = {
  target?: string;
  endpoint?: string;
  outputTruncated?: boolean;
  scanned?: number;
  uploaded?: number;
  skipped?: number;
  bytes?: number;
  dirs?: number;
  files?: number;
  remote?: string;
  to?: string;
};

/** One /api/remote/runs row as the Runs page reads it: ApiRunRow plus the input the active
 *  rows carry and the record's capped-output pair. api.d.ts types input only on the
 *  response's active half; the page passes both halves through one row type. */
export interface ApiRemoteRunRow extends ApiRunRow {
  input?: RemoteRunInput;
  outputCapped?: boolean;
  tail?: string;
  meta?: RemoteRunMeta;
}

/** One opened recorded row's fetched body (bodies[runId]): either the output read so far
 *  (growing text, next read's cursor, whole-run total) or the gone marker a failed read
 *  leaves when the record rolled past the run - never a mix of the two. */
export type RemoteRunBody = { text: string; next: number; total: number } | { gone: true };

/** One opened live row's followed output (live[runId]): what has been shown, and where the
 *  next pull continues from. */
export interface RemoteLiveBody {
  text: string;
  cursor: number;
}

/* --- pane click delegation (both remote pages) --------------------------------------------------- */

/* Both remote pages route ONE #pane onclick by event.target.closest. The house pattern the
   other delegating pages settled on (plugins.ts, terminal.ts) applies here too: the handler
   param is annotated MouseEvent and each probe reads event.target!.closest!<HTMLElement>(sel)
   - non-null assertions only, no parenthesised casts, so the emitted bytes stay put. */

