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

/* The panel's domain shapes. The file is named for what it used to be: docs/36 D6 put the
   shared `state` bag's type here as an ambient global, R3 made it an ordinary module, and
   R4 split the bag itself into seven slices that each own their record — so there is no
   PanelState any more, and nothing in here is state. What is left is the shapes those
   slices and their renderers pass around: one MCP's detail pane, a paged listing, the jobs
   forms' models, the Data view's edit buffers and filter terms. A .d.ts still never emits,
   so no empty types.js can enter the served tree.

   Renaming the file to types/domain.ts is R6's, not a slice's — it touches every importer
   and would bury the state split in import churn. */

import type { ApiDbColumn, ApiDbFkRow, ApiMcpCallRow, ApiMcpItem, ApiMcpRevisionRow, ApiMcpTunnelDep } from "./api.js";
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

