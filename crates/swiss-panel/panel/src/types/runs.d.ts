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

/* Ambient shapes owned by the runs/remote modules (run.ts, run-history.ts,
   views/remote.ts, views/remote-runs.ts) - docs/36 D6/D11. The cross-module families stay in
   types/{api,dom,state}.d.ts; a shape only one of these four files reads lives here so no
   shared file has to change for their migration. Wire shapes are field-for-field from the
   Rust struct that serializes them, like api.d.ts; panel-side shapes say which module owns
   the literal. */

/* --- run.ts / run-history.ts -------------------------------------------------------------------- */

/** POST /api/mcps/{name}/resource (readResource): one resource read's reply. text is the
 *  body, mimeType rides when the source sent one, ms is the round trip. */
interface ApiMcpResourceRead {
  ok: boolean;
  text?: string;
  mimeType?: string;
  ms?: number;
}

/** The result literal runTool stamps on d.run.result (renderRunResult reads it back).
 *  McpRunState keeps the field unknown because only this module writes it. */
interface McpRunResult {
  ok: boolean;
  text: string;
  ms?: number | null;
}

/** One parked definition revision as configBody renders it (d.revisions rows): state.d.ts
 *  types the rows open-ended; this names the three fields the revision list reads. type is
 *  required here only to index TYPE_LABELS - an older row without one falls to "?" at
 *  runtime exactly as before. */
interface McpRevisionRow {
  type: string;
  at?: string;
  note?: string;
  [key: string]: unknown;
}

/** One tunnel forward a /details answer carries for this MCP (tunnelDepsHtml). state.d.ts
 *  types the field unknown[] because this module is its only reader; the fields are the
 *  rule row's own (tunnel/manager.rs rule_row_of minus the live counters it does not read). */
interface McpTunnelDepRow {
  name: string;
  localPort: number;
  targetHost: string;
  targetPort: number;
  state: string;
  reason?: string;
  stalePool?: boolean;
  [key: string]: unknown;
}

/** The masked MCP config as configTarget reads it: a Record<string, unknown> on the wire
 *  (d.config); the fields the adapter branches touch, named so the reads stay checked. */
interface McpConfigLike {
  type?: string;
  command?: string;
  url?: string;
  baseUrl?: string;
  host?: string;
  db?: string;
  database?: string;
  mode?: string;
  [key: string]: unknown;
}

/* --- views/remote.ts ---------------------------------------------------------------------------- */

/** One live remote endpoint row (/api/remote/endpoints' list, api.rs:262). */
interface RemoteEndpointRow {
  id: string;
  label: string;
  state: string;
}

/** A remote target row as the Targets page holds it: the wire row with group widened to
 *  the null a member-removal writes back locally before the server's answer lands
 *  (ApiRemoteTargetRow.group is string on the wire). */
type RemoteTargetRow = Omit<ApiRemoteTargetRow, "group"> & { group: string | null };

/** The Add/Edit sheet's POST body (save): id only on a create (the URL carries it on edit). */
interface RemoteTargetBody {
  id?: string;
  label: string;
  endpoint: string;
  group: string | null;
  workspaceRoot: string;
  capabilities: string[];
  shell: string;
}

/* --- views/remote-runs.ts ----------------------------------------------------------------------- */

/** The input half of a remote run row: argv for exec, source/to for sync, remote/to for
 *  pull, path for cat/write, target and cwd on any kind. */
interface RemoteRunInput {
  argv?: unknown[];
  source?: string;
  to?: string;
  remote?: string;
  path?: string;
  target?: string;
  cwd?: string;
  [key: string]: unknown;
}

/** One /api/remote/runs row as the Runs page reads it: ApiRunRow plus the input the active
 *  rows carry and the record's capped-output pair. api.d.ts types input only on the
 *  response's active half; the page passes both halves through one row type. */
interface ApiRemoteRunRow extends ApiRunRow {
  input?: RemoteRunInput;
  outputCapped?: boolean;
  tail?: string;
  meta?: { target?: string; [key: string]: unknown };
}

/** One opened recorded row's fetched body (bodies[runId]): the growing text, the next
 *  read's cursor and the whole-run total. gone marks a run the record rolled past. */
interface RemoteRunBody {
  text: string;
  next: number;
  total: number;
  gone?: boolean;
}

/** One opened live row's followed output (live[runId]): what has been shown, and where the
 *  next pull continues from. */
interface RemoteLiveBody {
  text: string;
  cursor: number;
}

/* --- pane click delegation (both remote pages) --------------------------------------------------- */

/* Both remote pages route ONE #pane onclick by event.target.closest. The house pattern the
   other delegating pages settled on (plugins.ts, terminal.ts) applies here too: the handler
   param is annotated MouseEvent and each probe reads event.target!.closest!<HTMLElement>(sel)
   - non-null assertions only, no parenthesised casts, so the emitted bytes stay put. */

