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

/* The panel's domain shapes. The file is named for what it used to be: docs/36 D6 put the
   shared `state` bag's type here as an ambient global, R3 made it an ordinary module, and
   R4 split the bag itself into seven slices that each own their record — so there is no
   PanelState any more, and nothing in here is state. What is left is the shapes those
   slices and their renderers pass around: one MCP's detail pane, a paged listing, the jobs
   forms' models, the Data view's edit buffers and filter terms. A .d.ts still never emits,
   so no empty types.js can enter the served tree.

   Renaming the file to types/domain.ts is R6's, not a slice's — it touches every importer
   and would bury the state split in import churn. */

import type { ApiDbActivityRow, ApiDbColumn, ApiDbConnectionRow, ApiDbDatabase, ApiDbDataPage, ApiDbFkRow, ApiDbRedisKeyRow, ApiDbRedisValue, ApiDbStreamEntry, ApiDbStreamGroupRow, ApiDbTableRow, ApiDbTableDetail, ApiMcpCallRow, ApiMcpItem, ApiMcpRevisionRow, ApiMcpTunnelDep, DbQueryReply } from "./api.js";
/** One recorded action result on a row: what happened, whether it failed, when (time-of-day). */
export interface LastAction {
  msg: string;
  err: boolean;
  at: string;
}

/** The three listable capability kinds - the /api/mcps/{name}/{kind} path segment and the
 *  three McpDetail slots that hold their pages. util.ts KINDS is the runtime list. */
export type McpKind = "tools" | "resources" | "prompts";

/** One paged tools/resources/prompts listing (detail.ts pageState; /api/mcps/:name/:kind). */
export interface KindPageState {
  items: ApiMcpItem[];
  nextCursor: string | undefined;
  total: number | undefined;
  pageSize: number;
  cursors: string[];
  loading: boolean;
  loaded: boolean;
  error: string | null;
  /* tools-only: rows hidden from clients, and the resources master switch. */
  disabled?: string[];
  resourceEnabled?: boolean;
}

/** The detail pane's run tab (detail.ts openDetail): the selected tool, its last result, and
 *  the refill control's history state (run-history.ts owns the rendering). */
export interface McpRunState {
  tool: string | null;
  result: unknown;
  running: boolean;
  /* hist holds the tool-call listing rows (/calls), which are ApiMcpCallRow-shaped -
   *  not job runs, despite the file that renders them. */
  hist: ApiMcpCallRow[] | null;
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
export interface McpDetail {
  name: string;
  /* One of the pane's six tabs: the three McpKind slots below, or run / config / logs. The
   *  kind pages are reached as d[kind] after an isMcpKind(d.tab) guard (util.ts), not through
   *  an open signature. */
  tab: string;
  config: Record<string, unknown> | null;
  source?: string;
  editing: boolean;
  /* editMode ("edit" | "replace") joins the edit state at startEdit time, not in the openDetail
   *  literal; revisions join on the config tab's first load. */
  editMode?: string | null;
  editType: string | null;
  editVals: Record<string, unknown> | null;
  revisions?: ApiMcpRevisionRow[];
  /* /details' tunnel forwards for this MCP (the config tab chips count them); absent until
   *  the first /details answer lands. */
  tunnels?: ApiMcpTunnelDep[];
  /* The calls tab's search debounce handle (run-history.ts queueHistSearch); joins at
   *  runtime, not in the openDetail literal. The union spans the browser build (number)
   *  and the node-typed test tsconfig (Timeout). */
  callsQTimer?: ReturnType<typeof setTimeout> | number;
  oauth?: "authorized" | "needs-auth";
  oauthBusy: boolean;
  run: McpRunState;
  calls: ApiMcpCallRow[] | null;
  stderr: string;
  callsOpen: Record<string, boolean>;
  callsPage: number;
  callsMore: boolean;
  /* The panel stores just the full output string per seq (the wire row is parsed on arrival). */
  callsFull: Record<string, string>;
  callsQ: string;
  callsPendingPage: number | null;
  callsError: string;
  callsErrStatus: string;
  callsRetryTarget: number | null;
  callsRetryDir: string | null;
  /* A page switch's anchor intent (docs/32 B2): direction, keyboard drive, pager top. */
  callsSwitch: { dir: string | null; fromKey?: boolean; pagerTop?: number } | null;
  callsRequest: number;
  callsActive: number;
  callsTree: Record<string, Record<string, boolean>>;
  tools: KindPageState;
  resources: KindPageState;
  prompts: KindPageState;
}

/* The jobs plugin's config row (jobs.ts openV2Sheet): the wire form of
 *  swiss-jobs/src/jobs/def.rs JobsConfig, read-modify-PUT with the revision just read. The
 *  parser refuses any key outside this list (def.rs parse_at check_known), so the shape is
 *  closed on both sides; every field is optional on the wire because the parser defaults it. */
export interface JobConfigRow {
  schemaVersion?: number;
  maxConcurrentRuns?: number;
  maxQueuedRuns?: number;
  retention?: { days?: number; maxBytesPerJob?: number; maxHistoryBytes?: number };
  groups?: string[];
  definitions?: Record<string, JobDef>;
}

/* The closed enums of a v2 definition, spelled as def.rs enum_str accepts them. The
 *  Advanced sheet's selects are rendered FROM these lists (jobs-v2.ts JOB_*), so the form
 *  cannot offer a value the server refuses - the 2026-09 "aligned" firstRun did exactly that. */
export type JobTriggerKind = "manual" | "interval" | "cron";
export type JobFirstRun = "after-interval" | "immediate";
export type JobOverlap = "skip" | "queue-one";
export type JobMisfire = "skip" | "run-once";
export type JobBackoff = "fixed" | "exponential";
export type JobRetryOn = "failure" | "timeout";
export type JobCapture = "tail" | "none";

/* A definition's trigger (def.rs parse_trigger): kind picks which of the other keys are
 *  legal - interval takes everyMs / firstRun, cron takes expression / timezone (only "local"
 *  is accepted), manual takes nothing. Flat rather than a discriminated union because the
 *  form reads every slot off whatever trigger it loaded before it knows the kind. */
export interface JobTrigger {
  kind: JobTriggerKind;
  everyMs?: number;
  firstRun?: JobFirstRun;
  expression?: string;
  timezone?: "local";
}

/* One v2 job definition (docs/11 section 3) - the wire form of def.rs JobDefinition, keyed
 *  by id in JobConfigRow.definitions (the id is the map key, never a field: the parser's
 *  known-field list does not include it). Closed: def.rs parse_definition check_known
 *  refuses any other key, so nothing "rides along" - a key this form does not own cannot
 *  exist in a row the server accepted. */
export interface JobDef {
  title?: string;
  labels?: string[];
  disabled?: boolean;
  group?: string | null;
  trigger?: JobTrigger;
  action?: { type: string; input?: Record<string, unknown>; schemaVersion?: number };
  timeoutMs?: number;
  overlap?: JobOverlap;
  misfire?: JobMisfire;
  retry?: { maxAttempts?: number; delayMs?: number; backoff?: JobBackoff; retryOn?: JobRetryOn[] };
  output?: { capture?: JobCapture; maxBytes?: number };
}

/* formValues()' flat output (jobs.ts openV2Sheet): every form control's value as a string,
 *  assembled into a definition by formToV2. */
export interface JobFormValues {
  title: string;
  labels: string;
  kind: JobTriggerKind;
  everyMs: string;
  firstRun: JobFirstRun;
  cron: string;
  timeoutMs: string;
  disabled: boolean;
  overlap: JobOverlap;
  misfire: JobMisfire;
  retryMax: string;
  retryDelayMs: string;
  retryBackoff: JobBackoff;
  retryOn: JobRetryOn[];
  capture: JobCapture;
  maxBytes: string;
  actionType: string;
}

/* The schedule builder's live state (jobs.ts schedFromJob/schedToBody): one discriminated
 *  bag whose fields only the active mode reads. */
export interface JobSched {
  mode: "interval" | "daily" | "weekly" | "monthly" | "cron";
  every?: number;
  unit?: string;
  time?: string;
  days?: number[];
  day?: number;
  cron?: string;
}

/** One buffered insert row (the grid's edit buffer). */
export interface DbInsert {
  values: Record<string, unknown>;
}

/** The per-type plan the redis value view renders by (data-browsers.ts DB_REDIS_TYPES). */
export interface DbRedisTypeCfg {
  cols: string[];
  edit: string[];
  ins: string[];
  thing: string;
  deletable: boolean;
  add: string;
}

/** The redis value view's edit buffer: field-addressed updates/deletes and new rows,
 *  folded by dbRedisCommands into the pipeline Commit posts. */
export interface DbRedisEdits {
  key: string;
  type: string;
  updates: Record<string, unknown>;
  deletes: Record<string, unknown>;
  inserts: Record<string, unknown>[];
}

/* The DDL sheet's model (data-ddl.ts): one row of the columns mini-grid (prefilled rows
 *  render disabled - only adds are in the W4.6 minimal set). */
export interface DdlRow {
  name: string;
  type: string;
  nullable: boolean;
  default: string;
  comment: string;
  isNew: boolean;
}

/** The payload half of the (op, payload) body /api/db/:name/ddl-preview and /ddl both take -
 *  what swiss-host/src/dbbrowser.rs build_ddl_create reads per op: table (+ schema) on every
 *  op; create_table columns / primary / comment; add_column columns; create_index index /
 *  columns / unique. The sheet builds it from its form (data-ddl.ts dbDdlFormPayload), it
 *  never round-trips a server object, so nothing else can be in it. */
export interface DbDdlPayload {
  schema?: string;
  table?: string;
  index?: string;
  /* the index flavor lists plain column names; the table/column flavors list row objects. */
  columns?: ({ name?: string; type?: string; nullable?: boolean; default?: string; comment?: string } | string)[];
  /* create_table only, and the sheet does not offer it yet: the primary-key column names. */
  primary?: string[];
  unique?: boolean;
  comment?: string;
}

/** One open DDL sheet's whole mutable context; rebuilt per open, nulled on close. */
export interface DdlSheetState {
  kind: "table" | "column" | "index";
  dialect: string;
  conn: string;
  schema: string;
  schemas: string[];
  table: string;
  oldColumns: ApiDbColumn[];
  rows: DdlRow[];
  indexName: string;
  indexCols: string[];
  unique: boolean;
  comment: string;
  lastSql: string | null;
  lastPayload: DbDdlPayload | null;
  seq: number;
  timer: ReturnType<typeof setTimeout> | null;
  /* indexTouched joins at the first hand edit of the suggested index name. */
  indexTouched?: boolean;
}

/** One Structure tab's table spec (data-structure.ts): row is method-syntax so the three
 *  differently-typed row builders all fit it. */
export interface DbDetailSpec {
  head: string[];
  row(r: unknown): (string | { text: unknown; cls?: string; fk?: ApiDbFkRow })[];
  rows: unknown[];
  meta: string;
}

/** One form field as data-form.ts paints it: buffered value where pending, original where
 *  not; an insert's unset column is undefined (never null). */
export interface DbFormField {
  name: string;
  type: string;
  orig: unknown;
  value: unknown;
  pending: boolean;
}

/** The cell editor's row context (data-grid.ts passes it): orig is the value as loaded,
 *  pk the row's primary-key shape for the header line. */
export interface DbCellMeta {
  orig: unknown;
  pk: unknown;
}

/** One buffered row update in the Data grid (data-view.ts): pk identifies the row, changes
 *  holds column -> new value (null = clear to NULL). */
export interface DbBufferedUpdate {
  pk: Record<string, unknown>;
  changes: Record<string, unknown>;
}

/** One server-side WHERE term in the Data grid (data-view.ts filters): column, operator,
 *  needle - AND-ed into the page query by the adapter. */
export interface DbFilterTerm {
  column: string;
  op: string;
  /* a pushed cell filter carries the cell's raw value (number/boolean included). */
  value: string | number | boolean | null;
}

/** Per-connection grid geometry (docs/22 W2.1): saved column widths and hidden columns. */
export interface DbGridConfig {
  widths: Record<string, number>;
  hidden: string[];
}

/* --- the Data view's split record (docs/42 §2) --------------------------------------------------- */
/* The 49-field DbState mixed two domains that change at different times: what belongs to
   the CONNECTION (the sidebar's list, its filters, what the console remembers) and what
   belongs to the OPEN OBJECT (the grid's page, its filters, its buffered edits). docs/42
   splits the record along that line — every field of the old record lands in exactly one
   of the shapes below, and the division is disjoint (17 connection + 30 tab + 2 retired
   = 49; the "tab" count includes the kind discriminant itself).

   DbTab is a discriminated union ON PURPOSE: after `if (t.kind !== "table") return;`
   the table fields are visible and every other kind's fields are not, so a read of the
   wrong domain is a typecheck error, not a browser bug. Collapsing the four kinds into
   one interface with optional fields would switch that safety net off. */

/** One open object's tab kind: a table grid, the console, a redis key's value view, or
 *  the activity monitor. */
export type DbTabKind = "table" | "sql" | "key" | "activity";

/** What every tab carries whatever it shows: its own loading flag, its own checked-row
 *  map (the grid's keys and the result grid's "q"-prefixed keys share it), the keyboard
 *  focus cell, and the pending-bar preview toggle — the bar belongs to the tab, so its
 *  open/closed preview state does too. */
export interface DbTabBase {
  kind: DbTabKind;
  loading: boolean;
  sel: Record<string, boolean>;
  selAnchor: number;
  focus: { r: number; c: number } | null;
  sqlPreview: boolean;
  /* The strip's least-recently-used clock (docs/42 D3): a monotonic stamp bumped when the
     tab is created and every time it is activated. Eviction reads it and nothing else — a
     wall clock would tie the cap to how fast the operator works. */
  touched: number;
  /* The operator's own name for this tab (the card right-click's Rename): shown on the
     card, in the overflow menu and in every confirm INSTEAD of the derived title. Empty
     or absent falls back to the derived title. It is a label, never identity — closes,
     dedupe and switches still address the object underneath. */
  custom?: string;
}

/** A table (or view) opened in the row grid: its page, its filters, its buffered edits,
 *  and the pane segment showing (data | form | columns | indexes | fks | ddl until the
 *  six fold into four with docs/42 T4). */
export interface DbTableTab extends DbTabBase {
  kind: "table";
  table: string | null;
  schema: string | null;
  data: ApiDbDataPage | null;
  filters: DbFilterTerm[];
  pageSize: number;
  offset: number;
  order: string | null;
  dir: string;
  updates: Record<string, DbBufferedUpdate>;
  deletes: Record<string, Record<string, unknown>>;
  inserts: DbInsert[];
  pane: string;
  formIdx: number;
  detail: ApiDbTableDetail | null;
  detailBusy: boolean;
  /* The 409 observer's mark: which buffered row and columns lost a commit race. */
  conflict: { key: string; columns: string[] } | null;
}

/** The console as an open object (docs/42 T2): the text being written, the reply strip,
 *  and which reply is showing. One statement per result tab (docs/22 W4.3). */
export interface DbSqlTab extends DbTabBase {
  kind: "sql";
  sqlText: string;
  sqlResult: DbQueryReply | null;
  sqlResults: DbQueryReply[] | null;
  resultTab: number;
  sqlBusy: boolean;
}

/** A redis key opened in the value view (docs/22 W3.3): the shown key, its decoded
 *  value, and the buffered typed-value edits Commit would pipeline. */
export interface DbKeyTab extends DbTabBase {
  kind: "key";
  redisKey: string | null;
  redisValue: ApiDbRedisValue | null;
  redisEdits: DbRedisEdits | null;
  /* docs/45 S2: the stream view's grown row cache — the newest window plus every Load-
   * earlier page prepended, newest-first throughout. Null until the first window paints
   * (the value view's own loading state covers that); reset whenever the key re-reads,
   * because a fresh newest window is the truth and history re-walks from it. */
  redisStreamRows?: ApiDbStreamEntry[] | null;
  redisStreamMore?: boolean | null;
  /* docs/45 S3: Follow — the live-edge poller. redisStreamFollow is the switch,
   *  redisStreamEvery the tick interval in ms (1 s default, 2 s / 5 s options),
   *  redisStreamGroups/redisStreamGroupsOpen the consumer-group fold and its toggle.
   *  redisStreamLen/redisStreamLenAt sample the rate readout (dXLEN/dt); a tick error
   *  nulls them so the next sample never reads a stale baseline. */
  redisStreamFollow?: boolean;
  redisStreamEvery?: number;
  redisStreamGroups?: ApiDbStreamGroupRow[] | null;
  redisStreamGroupsOpen?: boolean;
  redisStreamLen?: number | null;
  redisStreamLenAt?: number | null;
  redisStreamRate?: string | null;
  /* docs/45 §2.3: the not-pinned holdback. redisStreamPending pools the pages a tick
   *  fetched while the operator reads history — nothing inserts, the table holds still,
   *  the pill counts. redisStreamGap is the more=true flag on a live-edge page (a
   *  middle chunk was skipped); redisStreamErr is why Follow stopped, if it stopped. */
  redisStreamPending?: ApiDbStreamEntry[] | null;
  redisStreamGap?: boolean;
  redisStreamErr?: string | null;
}

/** The activity monitor as an open object (docs/22 W3.2, docs/42 T2): the last
 *  /activity answer the 5s poll re-renders. */
export interface DbActivityTab extends DbTabBase {
  kind: "activity";
  activityRows: ApiDbActivityRow[] | null;
}

export type DbTab = DbTableTab | DbSqlTab | DbKeyTab | DbActivityTab;

/** What a tab is opened FOR (docs/42 T2): the address `dbOpenTab` dedupes on before it
 *  builds anything. A table's identity is schema+table+filters — the FK jump's filtered
 *  view of a table is a different object than the same table unfiltered, which is exactly
 *  why the jump opens a tab of its own instead of rewriting the one in front of the user
 *  (docs/22 W5.2). `sql` and `activity` carry no address: there is one console and one
 *  activity monitor per connection, so a second open activates the first. */
export type DbTabSpec =
  | { kind: "table"; table: string; schema: string | null; filters?: DbFilterTerm[] }
  | { kind: "key"; key: string }
  | { kind: "sql" }
  | { kind: "activity" };

/** The connection-scoped half of the old record: the sidebar's list state, the pickers,
 *  the grid geometry (per-connection by construction, docs/22 W2.1) and what the console
 *  remembers ACROSS consoles — the query history and the favorites, which belong to the
 *  connection, not to one scratchpad. Shared by every open tab; reset when the connection
 *  changes.
 *
 *  T1's transitional block is gone (docs/42 T2): the console's text and replies live on
 *  DbSqlTab and the activity rows on DbActivityTab, so `sqlOpen` and `activity: boolean`
 *  have nothing left to flag — an open console IS an open tab. */
export interface DbConnState {
  conns: ApiDbConnectionRow[];
  conn: string | null;
  tables: ApiDbTableRow[];
  tablesTotal: number;
  /* docs/43 addendum: the tree fetches the WHOLE catalog (no pager), so "more" is the one
   * honest truncation flag left - the fetch cap clipped the list and grep is the way past.
   * treeShown remembers how many rows each section band has painted past the render cap,
   * keyed by "schema/section" (empty schema for MySQL's root bands); the note row grows it
   * by one batch per click and only offers "show fewer" at the end. It is display memory,
   * reset on connection/database switches, never identity. */
  more: boolean;
  treeShown: Record<string, number>;
  grep: string;
  schemaFilter: string;
  sort: string;
  sortDir: string;
  gridCfg: DbGridConfig;
  history: string[];
  favorites: string[];
  /* The SCAN cursor is the wire's string (redis cursors are big unsigned numbers the
     panel compares against "0" — never a JS number). */
  redis: { keys: ApiDbRedisKeyRow[]; cursor: string; done: boolean; total: number } | null;
  redisType: string;
  redisError: boolean;
  /* docs/43 M3: the database axis. database is the SELECTED one ("" = the connection's
     configured default — the byte-identical path); databases is the lazy catalog, null
     until the selector is first opened ([] = the dialect has no axis → no database row). */
  database: string;
  databases: ApiDbDatabase[] | null;
}

