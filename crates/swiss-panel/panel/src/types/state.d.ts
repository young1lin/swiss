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

/* The panel's shared state bag — ambient globals on purpose (docs/36 D6): one project-wide
   `state` object does not need an import ceremony in all 55 modules, and a .d.ts never
   emits, so no empty types.js can enter the served tree. Field comments carry the reasons
   over from the boot literal in util.ts and the per-view factories (data-view's dbFreshState,
   the tun/jobs literals) — the shape lives here now, the WHY stays with the field. */

/** One recorded action result on a row: what happened, whether it failed, when (time-of-day). */
interface LastAction {
  msg: string;
  err: boolean;
  at: string;
}

/** One paged tools/resources/prompts listing (detail.ts pageState; /api/mcps/:name/:kind). */
interface KindPageState {
  items: ApiMcpTool[] | ApiMcpResource[] | ApiMcpPrompt[];
  nextCursor: string | undefined;
  total: number | undefined;
  pageSize: number;
  cursors: string[];
  loading: boolean;
  loaded: boolean;
  error: string | null;
}

/** The detail pane's run tab (detail.ts openDetail): the selected tool, its last result, and
 *  the refill control's history state (run-history.ts owns the rendering). */
interface McpRunState {
  tool: string | null;
  result: unknown;
  running: boolean;
  hist: ApiJobRunRecord[] | null;
  histTool: string | null;
  histLoading: boolean;
  histOpen: boolean;
  histSelSeq: number | null;
  histFull: Record<number, unknown>;
  histQ: string;
}

/** The whole detail pane for the selected MCP (detail.ts openDetail builds it; the poll
 *  repaints it, editing/editVals pause that). `source` and `oauth` are absent-not-null
 *  while unknown — they mirror the /details answer. */
interface McpDetail {
  name: string;
  tab: string;
  config: Record<string, unknown> | null;
  source?: string;
  editing: boolean;
  editType: string | null;
  editVals: Record<string, unknown> | null;
  oauth?: "authorized" | "needs-auth";
  oauthBusy: boolean;
  run: McpRunState;
  calls: ApiMcpCallRow[] | null;
  stderr: string;
  callsOpen: Record<string, boolean>;
  callsPage: number;
  callsMore: boolean;
  callsFull: Record<string, ApiMcpCallFull>;
  callsQ: string;
  callsPendingPage: number | null;
  callsError: string;
  callsErrStatus: string;
  callsRetryTarget: number | null;
  callsRetryDir: string | null;
  callsSwitch: { dir: string } | null;
  callsRequest: number;
  callsActive: number;
  callsTree: Record<string, unknown>;
  tools: KindPageState;
  resources: KindPageState;
  prompts: KindPageState;
}

/** The tunnel view's slice (util.ts boot literal; tunnels.ts renders it, polling.ts loads). */
interface TunState {
  tab: "conns" | "rules";
  data: ApiTunnelsResponse | null;
  busy: Record<string, string>;
  keys: ApiTunnelsKeysResponse | null;
  dragging: string | null;
  draggingGroup: string | null;
  pendingGroup: string | null;
}

/** The Jobs view's slice (util.ts boot literal; jobs.js renders it, polling.js loads it). */
interface JobsState {
  data: ApiJobRow[];
  groups: string[];
  collapsed: Record<string, boolean>;
  busy: Record<string, string>;
  painted: string;
  hist: { name: string; runs: ApiJobRunRecord[] } | null;
  dragging: string | null;
  draggingGroup: string | null;
  pendingGroup: string | null;
}

/** One buffered row update in the Data grid (data-view.ts): pk identifies the row, changes
 *  holds column -> new value (null = clear to NULL). */
interface DbBufferedUpdate {
  pk: Record<string, unknown>;
  changes: Record<string, unknown>;
}

/** One server-side WHERE term in the Data grid (data-view.ts filters): column, operator,
 *  needle - AND-ed into the page query by the adapter. */
interface DbFilterTerm {
  column: string;
  op: string;
  value: string | null;
}

/** Per-connection grid geometry (docs/22 W2.1): saved column widths and hidden columns. */
interface DbGridConfig {
  widths: Record<string, number>;
  hidden: string[];
}

/** The Data view's whole state — the shape data-view.ts's dbFreshState() factory builds on
 *  every mount (views/data.ts unmount nulls state.db; see the factory comment there). */
interface DbState {
  conns: ApiDbConnectionRow[];
  conn: string | null;
  tables: ApiDbTableRow[];
  tablesTotal: number;
  tablesPage: number;
  tablesLimit: number;
  more: boolean;
  grep: string;
  schemaFilter: string;
  sort: string;
  sortDir: "asc" | "desc";
  table: string | null;
  schema: string | null;
  data: ApiDbDataPage | null;
  filters: DbFilterTerm[];
  pageSize: number;
  offset: number;
  order: string | null;
  dir: "asc" | "desc";
  loading: boolean;
  gridCfg: DbGridConfig;
  sqlPreview: boolean;
  updates: Record<string, DbBufferedUpdate>;
  deletes: Record<string, Record<string, unknown>>;
  inserts: { values: Record<string, unknown> }[];
  sel: Record<string, boolean>;
  selAnchor: number;
  focus: { r: number; c: number } | null;
  sqlOpen: boolean;
  sqlText: string;
  sqlResult: DbQueryReply | null;
  sqlResults: DbQueryReply[] | null;
  sqlTab: number;
  sqlBusy: boolean;
  history: string[];
  tab: "data" | "form" | "columns" | "indexes" | "ddl" | "fks";
  formIdx: number;
  redis: { keys: ApiDbRedisKeyRow[]; cursor: number; done: boolean; total: number } | null;
  redisKey: string | null;
  redisEdits: Record<string, unknown> | null;
  activity: boolean;
  activityRows: ApiDbActivityReply | null;
  redisType: string;
  redisError: boolean;
  detail: ApiDbTableDetail | null;
  detailBusy: boolean;
}

/** The one shared bag, booted by util.ts's literal. tokenGroups/tokenMembers/tokenViewSecret
 *  are optional because the Tokens page creates them on first load — the boot literal (whose
 *  bytes are frozen by docs/36 D9) predates them. */
interface PanelState {
  mcps: ApiMcpRow[];
  groups: string[];
  collapsed: Record<string, boolean>;
  selected: string | null;
  filter: string;
  detail: McpDetail | null;
  busy: Record<string, string>;
  lastAction: Record<string, LastAction>;
  mem: ApiMemoryInfo | null;
  menuOpen: boolean;
  info: ApiInfoResponse | null;
  tokens: ApiTokenRow[];
  activeSecret: string | null;
  traffic: ApiTrafficRow[];
  trafficClients: ApiTrafficClientRow[];
  trafficFilter: "actions" | "all";
  trafficClient: string | null;
  trafficPage: number;
  trafficMore: boolean;
  trafficTotal: number;
  trafficAll: number;
  trafficOpen: Record<string, boolean>;
  trafficFull: Record<string, ApiTrafficFull>;
  trafficSig: string | null;
  view: string;
  panelVersion: string | null;
  addGroup: string | null;
  draggingGroup: string | null;
  db: DbState | null;
  tun: TunState;
  jobs: JobsState;
  tokenGroups?: string[];
  tokenMembers?: Record<string, string[]>;
  tokenViewSecret?: string | null;
}
