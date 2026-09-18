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

/* Every /api/* response the panel reads, ambient globals on purpose (docs/36 D6/D11): each
   interface is copied field-for-field from the Rust struct that serializes it - the comment
   above a type names that producer. Optional fields below are absent-not-null on the wire
   (Node-era JSON.stringify semantics the Rust side preserves with skip_serializing_if), and
   that is the contract the panel code is allowed to lean on. A field the panel reads that is
   missing HERE means a panel bug - it gets recorded, never papered over with a loose type. */

/* --- host chrome (src/adminapi.rs) -------------------------------------------------------------- */

/** GET /api/info - adminapi.rs:533, the boot handshake (token env name + panel stamp). */
export interface ApiInfoResponse {
  tokenEnv: string;
  panelVersion: string;
  build: Record<string, unknown>;
}

/** GET /api/memory - swiss-host/src/mem.rs; polled for the footer readout. */
export interface ApiMemoryInfo {
  gatewayMb: number;
  heapUsedMb: number;
  heapTotalMb: number;
  externalMb: number;
  processCount: number;
  childrenPending: number;
  at: string;
  childrenMb?: number;
  measuredAt?: string;
}

/** GET /api/autostart - src/autostart.rs AutoStartState::to_json. */
export interface ApiAutoStartResponse {
  enabled: boolean;
  detail: string;
  command: string;
}

/* --- MCP (src/adminapi.rs, swiss-mcp) ------------------------------------------------------------ */

/** GET /api/mcps - adminapi.rs:997: both lists the sidebar renders, groups in sidebar order. */
export interface ApiMcpListResponse {
  mcps: ApiMcpRow[];
  groups: string[];
}

/** One MCP row - adminapi.rs:949 builds it; latency/lastCheck/reason/startedAt/oauth are
 *  absent until first value (the Node undefined-drop the comment at adminapi.rs:972 names). */
export interface ApiMcpRow {
  name: string;
  source: string;
  type: string;
  tag: string;
  description: string;
  lifecycle: string;
  state: string;
  /* Always present on the wire; optional here because the panel optimistically rewrites it
   * (or clears it) on a member move before the server's answer lands; the member PUT's
   * answer echoes null when the explicit entry was removed. */
  group?: string | null;
  latencyMs?: number;
  lastCheck?: string;
  reason?: string;
  startedAt?: string;
  oauth?: "authorized" | "needs-auth";
}

/** GET /api/mcps/{name}/details - adminapi.rs get_details; config is the masked def. */
export interface ApiMcpDetails {
  name: string;
  source: string;
  type: string;
  lifecycle: string;
  state: string;
  logs: string;
  config: Record<string, unknown>;
  tunnels: string[];
  reason?: string;
  oauth?: "authorized" | "needs-auth";
}

/** GET /api/mcps/{name}/{kind} (tools|resources|prompts) - adminapi.rs:2321: the page lives
 *  under the kind's own key; nextCursor/total are absent until the page carries them, and
 *  disabledTools (tools) / resourceEnabled (resources) ride their kinds only. */
export interface ApiMcpKindPage {
  pageSize: number;
  nextCursor?: string;
  total?: number;
  tools?: (ApiMcpTool | ApiMcpResource | ApiMcpPrompt)[];
  resources?: (ApiMcpTool | ApiMcpResource | ApiMcpPrompt)[];
  prompts?: (ApiMcpTool | ApiMcpResource | ApiMcpPrompt)[];
  disabledTools?: string[];
  resourceEnabled?: boolean;
}

/** The v2 view's trigger as def.rs trigger_json spells it: one discriminated union,
 *  exactly the three kinds the Rust side constructs. firstRun is Option - null when the
 *  interval rule leaves it to the default. */
export type ApiJobTrigger =
  | { kind: "manual" }
  | { kind: "interval"; everyMs: number; firstRun: string | null }
  | { kind: "cron"; expression: string; timezone: string };

/** A tools/list item, passed through from the MCP as-is - only the fields the panel renders. */
/** A tool's input schema as the panel reads it (the JSON-Schema subset that drives the Run
 *  form's generated argument fields: properties, required, enum, array item types). */
export interface ToolSchemaProp {
  type?: string;
  description?: string;
  enum?: unknown[];
  items?: { type?: string };
  [key: string]: unknown;
}

export interface ToolInputSchema {
  properties?: Record<string, ToolSchemaProp>;
  required?: string[];
  [key: string]: unknown;
}

export interface ApiMcpTool {
  name: string;
  description?: string;
  inputSchema?: ToolInputSchema;
  [key: string]: unknown;
}

/** One kind-page row, the three capabilities fused for the shared renderer (logs.ts kindBody):
 *  uri marks a resource, arguments a prompt, inputSchema a tool - the renderer branches on
 *  the kind, so one shape carries all three without a union at every property. */
export interface ApiMcpItem {
  name?: string;
  uri?: string;
  description?: string;
  arguments?: { name: string; required?: boolean }[];
  inputSchema?: ToolInputSchema;
  [key: string]: unknown;
}

/** A resources/list item (uri is the identity). */
export interface ApiMcpResource {
  uri: string;
  name: string;
  description?: string;
  mimeType?: string;
  [key: string]: unknown;
}

/** A prompts/list item. */
export interface ApiMcpPrompt {
  name: string;
  description?: string;
  arguments?: { name: string; description?: string; required?: boolean }[];
  [key: string]: unknown;
}

/** GET /api/mcps/{name}/calls - swiss-mcp/src/calls.rs read_calls. */
export interface ApiMcpCallPage {
  calls: ApiMcpCallRow[];
  page: number;
  pageSize: number;
  more: boolean;
}

/** One recorded tool call - swiss-mcp/src/calls.rs CallEntry (camelCase serde). client,
 *  preview, body and bodyGone are absent-not-null. */
export interface ApiMcpCallRow {
  seq: number;
  at: string;
  tool: string;
  via: string;
  client?: string;
  ok: boolean;
  ms: number;
  args: string;
  output: string;
  chars: number;
  preview?: boolean;
  body?: boolean;
  bodyGone?: boolean;
}

/** GET /api/mcps/{name}/calls/{seq} - read_call: the same CallEntry with the full reply. */
export type ApiMcpCallFull = ApiMcpCallRow;

/* --- traffic (swiss-mcp/src/traffic.rs) --------------------------------------------------------- */

/** GET /api/traffic - traffic.rs read_traffic: one newest-first page of the ring. */
export interface ApiTrafficPage {
  entries: ApiTrafficRow[];
  /* The whole ring's client fold rides every page (the panel keeps one state, not two). */
  clients?: ApiTrafficClientRow[];
  total: number;
  totalUnfiltered: number;
  page: number;
  pageSize: number;
  more: boolean;
}

/** One interaction row - traffic.rs TrafficEntry (camelCase serde) minus body/response
 *  (a page strips both) plus hasResponse. */
export interface ApiTrafficRow {
  seq: number;
  at: string;
  mcp: string;
  method: string;
  client?: string;
  clientName?: string;
  clientVersion?: string;
  params?: string;
  ok: boolean;
  ms: number;
  hasResponse: boolean;
}

/** GET /api/traffic/{seq} - traffic.rs read_traffic_entry: the raw redacted bodies, on expand. */
export interface ApiTrafficFull {
  /* gone: the ring rolled past this seq - the panel stores this marker for a 404 so the
   *  row says so instead of spinning forever. */
  gone?: boolean;
  body?: string;
  response?: string;
}

/** One folded client of the ring - traffic.rs read_clients (sorted by lastSeq). */
export interface ApiTrafficClientRow {
  key: string;
  label: string;
  tokens: string[];
  mcps: string[];
  count: number;
  lastAt: string;
  lastSeq: number;
}

/* --- tokens and the secret vault (src/adminapi.rs, swiss-host/src/token.rs) ---------------------- */

/** A token as the API ever shows it - swiss-host/src/token.rs PublicToken (no secret). */
export interface ApiTokenRow {
  id: string;
  label: string;
  createdAt: string;
}

/** GET /api/tokens - adminapi.rs:572: the management list plus the group scopes' names. */
export interface ApiTokensResponse {
  tokens: ApiTokenRow[];
  tokenEnv: string;
  groups: string[];
  tokenGroups: Record<string, string>;
}

/** POST /api/tokens - adminapi.rs:595: the one reply that carries a fresh secret. */
export interface ApiTokenCreated {
  id: string;
  label: string;
  secret: string;
  createdAt: string;
}

/** GET /api/tokens/{id}/secret - adminapi.rs:615 (copy-command support, never a listing). */
export interface ApiTokenSecret {
  id: string;
  label: string;
  secret: string;
}

/** GET /api/secrets - adminapi.rs:661: names and labels only; values never leave the vault
 *  (docs/19 D5). secretGroups is name -> sink-resolved group label. */
export interface ApiSecretsResponse {
  secrets: string[];
  rev: number;
  groups: string[];
  order: string[];
  secretGroups: Record<string, string>;
}

/** PUT/DELETE /api/secrets/{name} - adminapi.rs:716: the new rev after the write. */
export interface ApiSecretWriteResponse {
  rev: number;
}

/* --- the group-scope family (src/adminapi.rs:817+, swiss-host/src/groups.rs) --------------------- */

/** PUT /api/groups/{scope} - set the whole list; a dropped group's members fall to the first. */
export interface ApiGroupsSetResponse {
  groups: string[];
}

/** POST /api/groups/{scope}/rename - moved counts the explicit members that rode along. */
export interface ApiGroupsRenameResponse {
  groups: string[];
  moved: number;
}

/** PUT /api/groups/{scope}/members/{id} - group is null when the explicit entry was removed. */
export interface ApiGroupAssignResponse {
  group: string | null;
}

/** PUT /api/groups/{scope}/order - adminapi.rs:917: the flat order as now stored. */
export interface ApiGroupsOrderResponse {
  order: string[];
}

/* --- runs and actions (swiss-host/src/services/api.rs, runs.rs) ---------------------------------- */

/** GET /api/actions - services/api.rs:74: live capabilities, stopped providers absent. */
export interface ApiActionsResponse {
  actions: ApiActionRow[];
}

export interface ApiActionRow {
  type: string;
  title: string;
  provider: string;
  cancelable: boolean;
  schema: Record<string, unknown>;
}

/** GET /api/runs - services/api.rs:83. */
export interface ApiRunsResponse {
  runs: ApiRunRow[];
  capacity: { maxConcurrentRuns: number; maxQueuedRuns: number };
}

/** One run - swiss-host/src/services/runs.rs:312 RunView::to_json (the fields after endedAt
 *  are absent until they exist). output rides only the single-run GET. */
export interface ApiRunRow {
  runId: number;
  owner: string;
  label: string;
  action: string;
  state: string;
  queuedAt: string;
  startedAt?: string;
  endedAt?: string;
  ms?: number;
  pid?: number;
  exitCode?: number;
  timedOut?: boolean;
  canceled?: boolean;
  error?: string;
  chars?: number;
  outputTruncated?: boolean;
  meta?: Record<string, unknown>;
  output?: string;
}

/** POST /api/runs - services/api.rs:150: 202 with the queued run's row. */
export interface ApiRunSubmitted {
  runId: number;
  run: ApiRunRow;
}

/** GET /api/runs/{id}/output - services/api.rs run_output: the byte-cursor chunk the live
 *  reader and the run-history sheet share. */
export interface ApiRunOutputChunk {
  runId: number;
  state: string;
  cursor: number;
  nextCursor: number;
  output: string;
  truncated: boolean;
  terminal: boolean;
}

/* --- jobs (swiss-jobs/src/jobs) ----------------------------------------------------------------- */

/** GET /api/jobs - jobs/mod.rs:1230 all_views + the scope's group names. */
export interface ApiJobsResponse {
  jobs: ApiJobRow[];
  groups: string[];
}

/** One job row - def.rs:1103 to_v1_view + def.rs:1231 v2_view_fields + mod.rs:1200 runtime
 *  facts. everySec is absent for sub-second intervals; cron absent unless cron-triggered;
 *  v2 triggers leave both schedule fields absent rather than rounding a lie. */
export interface ApiJobRow {
  name: string;
  /* The v2 identity (docs/11 section 7.1): a human title and free-form labels on top of
   *  the id; optional because a v1-era config carries neither. */
  title?: string;
  labels?: string[];
  enabled: boolean;
  timeoutMs: number;
  command: string;
  everySec?: number;
  cron?: string;
  cwd?: string;
  env?: Record<string, string>;
  editableInV1: boolean;
  /* trigger/action are the VIEW shapes swiss-jobs def.rs trigger_json/action_json emit,
   * field-for-field (docs/37 M9 closed these two; the config-file spellings the v2 sheet
   * PUTs stay open in types/state.d.ts JobDef - unknown action fields must ride along). */
  trigger?: ApiJobTrigger;
  action?: { type: string; input: Record<string, unknown>; schemaVersion: number };
  source: string;
  group: string | null;
  actionAvailable: boolean;
  configRevision: number;
  lastRunAt?: string;
  lastOk?: boolean;
  running: boolean;
  nextDueAt?: string;
}

/** GET /api/jobs/{name}/runs - jobs/api.rs:263; nextBefore pages backwards. */
export interface ApiJobRunsResponse {
  runs: ApiJobRunRecord[];
  nextBefore?: number;
}

/** One recorded job run - jobs/api.rs ran_record: output is the clipped preview; runId is
 *  absent when the run never produced a tracked run (e.g. a refusal). */
export interface ApiJobRunRecord {
  at: string;
  trigger: string;
  ok: boolean;
  ms: number;
  outcome: string;
  /* the history meta line reads these when present (jobs-v2 historyMeta) */
  error?: string;
  reason?: string;
  missedCount?: number;
  timedOut?: boolean;
  canceled?: boolean;
  occurrenceKey?: string;
  attempt: number;
  attempts: number;
  runId?: number;
  pid?: number;
  exitCode?: number;
  preview?: boolean;
  output: string;
  chars: number;
}

/* --- tunnels (swiss-tunnels/src/tunnel) --------------------------------------------------------- */

/** GET /api/tunnels - tunnel/api.rs:409 manager.rows() + the two scope group lists + the
 *  live MCP name list (for the suggestion picker). */
export interface ApiTunnelsResponse {
  connections: ApiTunnelConnectionRow[];
  rules: ApiTunnelRuleRow[];
  ruleGroups: string[];
  connGroups: string[];
  mcps: string[];
}

/** One connection row - tunnel/manager.rs rows(): live fields layered on types.rs
 *  SshConnDef::to_json(). Secrets ride masked (proxyPassword) or absent (passphrase,
 *  password); group/reason/hostKey and the proxy/jump fields are absent when unset. */
export interface ApiTunnelConnectionRow {
  id: string;
  name: string;
  host: string;
  port: number;
  username: string;
  authType: string;
  keyPath?: string;
  hostKey?: string;
  /* group is absent when unset; the panel also stages a null locally before the assign
   *  round-trip lands the canonical name. */
  group?: string | null;
  state: string;
  reason?: string;
  ruleCount: number;
  activeRules: number;
  proxy?: string;
  proxyUsername?: string;
  proxyPassword?: string;
  jump?: string;
}

/** One rule row - tunnel/manager.rs rule_row_of(): types.rs RuleDef::to_json() plus the
 *  live runtime columns. portOwner is absent while the local port is free. */
export interface ApiTunnelRuleRow {
  id: string;
  name: string;
  connectionId: string;
  localPort: number;
  targetHost: string;
  targetPort: number;
  remark: string;
  autoReconnect: boolean;
  reconnectInterval: number;
  enabled: boolean;
  mcps: string[];
  group?: string | null;
  state: string;
  reason?: string;
  connectionName: string;
  startedAt?: string;
  reconnectedAt?: string;
  sockets: number;
  bytesIn: number;
  bytesOut: number;
  channelFailures: number;
  portOwner?: { pid: number; name: string };
  mcpRows: { name: string; state: string; known: boolean }[];
}

/** GET /api/tunnels/keys - tunnel/api.rs:437: private keys found under ~/.ssh. */
export interface ApiTunnelsKeysResponse {
  keys: { path: string; name: string }[];
  defaultPath: string;
}

/** GET /api/tunnels/browse - tunnel/api.rs DirListing::to_json; error set on a bad dir. */
export interface ApiTunnelsBrowseResponse {
  dir: string;
  parent?: string | null;
  entries: ApiTunnelsBrowseEntry[];
  error?: string;
}

/** One row of that listing: a folder or a candidate key file. */
export interface ApiTunnelsBrowseEntry {
  name: string;
  path: string;
  dir: boolean;
}

/** GET /api/tunnels/suggest/{port} - tunnel/api.rs:473: MCPs pointing at this local port. */
export interface ApiTunnelsSuggestResponse {
  port: number;
  mcps: unknown[];
}

/** POST/PUT /api/tunnels/connections - the masked echo the sheet reads back once. */
export interface ApiTunnelConnMutation {
  connection: ApiTunnelConnectionRow;
}

/** POST/PUT /api/tunnels/rules - the created/updated rule echoed back. */
export interface ApiTunnelRuleMutation {
  rule?: ApiTunnelRuleRow;
}

/* The connection sheet's working copy (tunnel-sheets openConnSheet): the row when editing,
 *  the seed literal when creating. port-target fields are strings only in the seed. */
export interface ConnSheetDraft {
  id?: string;
  name: string;
  host: string;
  port: number;
  username: string;
  authType: string;
  keyPath?: string;
  passphrase?: string;
  password?: string;
  proxy?: string;
  proxyUsername?: string;
  proxyPassword?: string;
  jump?: string;
}

/* The rule sheet's working copy (openRuleSheet): same idea - the row, or the seed with
 *  empty string ports where a saved row carries numbers. */
export interface RuleSheetDraft {
  id?: string;
  name: string;
  connectionId: string;
  localPort: number | string;
  targetHost: string;
  targetPort: number | string;
  remark?: string;
  autoReconnect: boolean;
  reconnectInterval: number;
  mcps?: string[];
}

/* --- the Data browser (swiss-data/src/dbbrowser_api.rs, dbbrowser.rs) --------------------------- */

/** GET /api/db - dbbrowser_api.rs:97 browsable_connections (ordered like the sidebar). */
export interface ApiDbConnectionsResponse {
  connections: ApiDbConnectionRow[];
}

export interface ApiDbConnectionRow {
  name: string;
  dialect: string;
  label: string;
  state: string;
  group: string;
}

/** GET /api/db/{name}/tables - dbbrowser_api.rs list_tables route. */
export interface ApiDbTablesResponse {
  tables: ApiDbTableRow[];
  total: number;
  page: number;
  limit: number;
  more: boolean;
}

export interface ApiDbTableRow {
  schema: string;
  name: string;
  /* optional: the FK column's jump (data-structure) opens a table with name+schema only */
  type?: string;
  approxRows?: number;
  size?: string;
}

/** GET /api/db/{name}/data - dbbrowser_api.rs read_table route (the stub at :1196 spells it). */
export interface ApiDbDataPage {
  schema: string;
  table: string;
  columns: ApiDbColumn[];
  rows: Record<string, unknown>[];
  total: number;
  offset: number;
  limit: number;
  nextPage: boolean;
  primaryKey: string[];
  editable: boolean;
  /* the server's own sentence when editable is false ("no primary key" and friends). */
  editNote?: string;
}

export interface ApiDbColumn {
  name: string;
  dataType: string;
  nullable: boolean;
  isPrimaryKey: boolean;
  defaultValue: string | null;
  comment: string | null;
}

/** GET /api/db/{name}/schema - describe_table: the Structure tabs' detail (W5.2's FK jump). */
export interface ApiDbTableDetail {
  schema: string;
  table: string;
  columns: ApiDbColumn[];
  primaryKey: string[];
  indexes: { name: string; unique: boolean; primary: boolean; columns: string[] }[];
  foreignKeys: ApiDbFkRow[];
  ddl: string;
}

/** One FK of the Structure tab's Foreign Keys list; the target name carries the jump. */
export interface ApiDbFkRow {
  name: string;
  column: string;
  refSchema: string;
  refTable: string;
  refColumn: string;
}

/** One console statement's reply - dbbrowser_api.rs query route; elapsedMs is attached by
 *  the :744 helper. Used for sqlResult AND each entry of sqlResults. */
export interface DbQueryReply {
  columns: string[];
  rows: Record<string, unknown>[];
  rowCount: number;
  elapsedMs?: number;
  /* the panel's own annotations on a stored result (not on the wire): the console flags an
   *  EXPLAIN-prefixed run and names each result tab; note carries the reply's shape hint. */
  explained?: boolean;
  tabLabel?: string;
  note?: string;
}

/** GET /api/db/{name}/key - one redis key's typed value view: type plus the type's own
 *  payload (value for string, entries for containers), the key's own name, length and ttl. */
export interface ApiDbRedisValue {
  key: string;
  type: string;
  value?: unknown;
  length?: number | null;
  ttl?: number;
  /* open by construction (docs/37 M9 keep): the value half is the redis type's own
   * payload - string / list / hash / set / zset each shape it differently - so only the
   * envelope is closed and the payload rides as unknown. */
  [key: string]: unknown;
}

/** GET /api/db/{name}/keys - the redis SCAN page (one page of the key grid). */
export interface ApiDbRedisKeysResponse {
  keys: ApiDbRedisKeyRow[];
  /* redis cursors travel as strings (big unsigned numbers the panel compares against "0"). */
  cursor: string;
  done: boolean;
  total: number;
}

export interface ApiDbRedisKeyRow {
  key: string;
  type: string;
  ttl?: number;
  /* open by keep (docs/37 M9): SCAN rows are built key-by-key from driver maps; the
   * panel reads the three listed fields and tolerates a driver adding one. */
  [key: string]: unknown;
}

/** GET /api/db/{name}/activity - the 5s Activity poll while the pane is open. */
export interface ApiDbActivityReply {
  rows: ApiDbActivityRow[];
}

/** One pg_stat_activity-style session row (swiss-data activity.rs); own marks the row this
 *  panel's own polling session is, seconds feeds the duration column. */
export interface ApiDbActivityRow {
  pid: number;
  user?: string | null;
  state?: string | null;
  wait?: string | null;
  blockedBy?: string | number | null;
  seconds?: number | null;
  query?: string | null;
  own?: boolean;
  /* open by keep (docs/37 M9): pg_stat_activity columns differ per engine (mysql shows
   * different state fields than pg); the listed fields are the rendered ones. */
  [key: string]: unknown;
}

/** POST /api/db/{conn}/completion - one candidate the console's suggest list shows. */
export interface ApiDbCompletionItem {
  label: string;
  kind: string;
  detail?: string;
}

export interface ApiDbCompletionReply {
  items: ApiDbCompletionItem[];
}

/* --- remote (swiss-remote/src/api.rs, target.rs) ------------------------------------------------ */

/** GET /api/remote/targets - api.rs:295: rows are target.rs:171 to_json. */
export interface ApiRemoteTargetsResponse {
  targets: ApiRemoteTargetRow[];
  groups: string[];
}

export interface ApiRemoteTargetRow {
  id: string;
  label: string;
  endpoint: string;
  workspaceRoot: string;
  shell: string;
  capabilities: string[];
  defaultTimeoutMs: number;
  group: string;
}

/** GET /api/remote/endpoints - api.rs:262: the transport's live endpoints + presence. */
export interface ApiRemoteEndpointsResponse {
  presence: string;
  provider: string | null;
  endpoints: { id: string; label: string; state: string }[];
}

/** GET /api/remote/runs - api.rs:145: the history page. Active rows carry the run's input. */
export interface ApiRemoteRunsResponse {
  runs: ApiRunRow[];
  active: (ApiRunRow & { input?: Record<string, unknown> })[];
  limits: { maxAgeMs: number; maxTotalBytes: number; maxRuns: number; maxOutputBytes: number };
  usage: { bytes: number; runs: number };
  nextBefore?: number;
}

/** GET /api/remote/runs/{id}/output - api.rs run_output: the shared chunk shape + total. */
export interface ApiRemoteRunOutput {
  runId: number;
  cursor: number;
  nextCursor: number;
  output: string;
  total: number;
  truncated: boolean;
  terminal: boolean;
}

/* --- terminal (src/plugins/terminal_api.rs, swiss-terminal) ------------------------------------- */

/** GET /api/terminal/targets - terminal/session.rs TargetsView (camelCase serde). */
export interface ApiTerminalTargets {
  local: {
    enabled: boolean;
    shell: string;
    shells: { program: string; label: string }[];
  };
  remote: {
    presence: string;
    reason?: string;
    targets: { id: string; label: string; host: string; port: number; username: string; state: string }[];
  };
}

/** GET /api/terminal/sessions - terminal_api.rs list_sessions: a bare array of SessionView. */
export type ApiTerminalSessionsResponse = ApiTerminalSessionRow[];

/** terminal/session.rs SessionView. recording is null when the session is not recorded. */
export interface ApiTerminalSessionRow {
  id: string;
  target: string;
  label: string;
  opened: string;
  bytesOut: number;
  attached: boolean;
  recording: string | null;
}

/** POST /api/terminal/sessions - session.rs Opened: the id plus the one-time ticket. */
export interface ApiTerminalOpened {
  id: string;
  ticket: string;
  recording: string | null;
}

/** POST /api/terminal/sessions/{id}/ticket - a fresh one-time ticket. */
export interface ApiTerminalTicket {
  ticket: string;
}

/* --- plugins (swiss-host/src/host) -------------------------------------------------------------- */

/** GET /api/plugins - engine.rs:457 inventory: rows plus every contributed page. */
export interface ApiPluginsResponse {
  revision: number;
  plugins: ApiPluginRow[];
  pages: ApiPluginPage[];
}

/** engine.rs:471 row() - lastError when there is one, requires/requiresMet as a pair and
 *  only when the plugin needs anything. */
export interface ApiPluginRow {
  id: string;
  kind: string;
  label: string;
  version: string;
  enabled: boolean;
  state: string;
  configRevision: number;
  pages: string[];
  lastError?: string;
  requires?: string[];
  requiresMet?: boolean;
}

/** host/descriptor.rs:55 PageDescriptor::to_json - the page registry's wire shape. */
export interface ApiPluginPage {
  id: string;
  pluginId: string;
  label: string;
  order: number;
  path: string;
  entry: string;
  sidebar: boolean;
  layout: string;
}

/** GET /api/plugins/{id}/config - host/api.rs get_config. */
export interface ApiPluginConfigResponse {
  revision: number;
  config: Record<string, unknown>;
  schema: Record<string, unknown> | null;
}

/** PUT /api/plugins/{id}/config - host/api.rs put_config: applied:false means the running
 *  instance has not taken the row yet; error carries the apply failure when it has not. */
export interface ApiPluginConfigPutResponse extends ApiPluginConfigResponse {
  warnings: string[];
  applied: boolean;
  error?: string;
}

/** PUT /api/plugins/{id} (enable/disable) - {revision, plugin: row}. */
export interface ApiPluginToggleResponse {
  revision: number;
  plugin: ApiPluginRow;
}

/* --- every admin error body ---------------------------------------------------------------------- */

/** The one error shape every route answers with - adminapi.rs admin_error. */
/** POST /api/mcpdefs/import - the .mcp.json importer's answer: what landed (rows by name)
 *  and what was refused (reasons live server-side; the toast counts only). */
export interface ApiMcpDefsImportResponse {
  imported?: { name: string }[];
  skipped?: unknown[];
}

export interface ApiErrorBody {
  error: string;
}
