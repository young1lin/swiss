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

/* The llm plugin's wire (SPEC §llm): the row the three pages edit through the host's config
 * API, and what GET /api/llm/status shows beside it. Credentials are only ever the
 * `${secret://name}` / `${ENV}` references the row holds. */

export type LlmStyle = "openai" | "anthropic";
export type LlmRuntime = "builtin" | "chat" | "claude" | "pi" | "db";
export type LlmProtocol = "grpc" | "http/protobuf" | "http/json";
export type LlmModelKind = "chat" | "embedding";

/** One model a provider lists: its id on the wire and what it is for (`chat` when unset), and
 *  what its vendor states about it - every one optional, stored only when it says something. */
export interface LlmModel {
  id: string;
  kind?: LlmModelKind;
  /** What a menu shows for it; the id when unset. */
  name?: string;
  /** Tokens it reads in one request (a chat model's). */
  contextWindow?: number;
  /** Tokens it writes at most in one answer: an agent asking more is cut to this. */
  maxOutput?: number;
  /** It reads images beside text (false when unset). */
  image?: boolean;
  /** It takes a thinking effort (true when unset): false sends it none. */
  thinking?: boolean;
}

/** A model as the status shows it: the stored entry with every field filled in. */
export interface LlmModelRow {
  id: string;
  kind: LlmModelKind;
  name?: string | null;
  contextWindow?: number | null;
  maxOutput?: number | null;
  image?: boolean;
  thinking?: boolean;
}

/** One `providers` entry as the row holds it. The first chat model is an agent's default,
 *  the first embedding model the embedding tier's. */
export interface LlmProviderEntry {
  style: LlmStyle;
  baseUrl: string;
  models?: LlmModel[];
  apiKey?: string;
  proxy?: string;
}

/** The `embedding` block as the row holds it: `provider` names an openai-style entry, or "off". */
export interface LlmEmbeddingBlock {
  provider: string;
  model?: string;
  dimensions?: number;
  batch?: number;
  timeoutMs?: number;
  queryPrefix?: string;
  documentPrefix?: string;
  minSimilarity?: number;
}

/** How hard a builtin or chat agent's model thinks (SPEC §llm.agents effort). */
export type LlmEffort = "low" | "medium" | "high" | "max";

/** One `agents` entry as the row holds it; which fields apply depends on the runtime. */
export interface LlmAgentEntry {
  runtime?: LlmRuntime;
  provider?: string;
  model?: string;
  maxTurns?: number;
  maxTokens?: number;
  /** builtin/chat: the most one request carries, in KiB; unset is 1024. */
  maxContextKiB?: number;
  /** builtin/chat: unset leaves the model's own default. */
  effort?: LlmEffort;
  /** builtin: the vault names its shell commands may reference - a name, group:<group> or *; unset grants none. */
  secrets?: string[];
  /** Tasks one consumer runs at once: 4 for builtin/chat, 1 for claude/pi when unset. */
  concurrency?: number;
  command?: string;
  env?: Record<string, string>;
  timeoutMs?: number;
  report?: boolean;
  systemPrompt?: string;
  /** Unset takes every tool a task offers; `[]` none. */
  tools?: string[];
}

/** The `telemetry` block: agent runs as OTLP logs in Claude Code's event vocabulary. */
export interface LlmTelemetryBlock {
  enabled?: boolean;
  protocol?: LlmProtocol;
  endpoint: string;
  headers?: Record<string, string>;
  serviceName?: string;
  logPrompts?: boolean;
}

/** The whole row (`enabled` is the host's own switch). */
export interface LlmRow {
  enabled?: boolean;
  providers?: Record<string, LlmProviderEntry>;
  embedding?: LlmEmbeddingBlock;
  agents?: Record<string, LlmAgentEntry>;
  telemetry?: LlmTelemetryBlock;
}

/** A provider as the status shows it: `url` is the chat endpoint the wire posts to. */
export interface LlmProviderRow {
  name: string;
  style: LlmStyle;
  baseUrl: string;
  models: LlmModelRow[];
  apiKey: string | null;
  proxy: string | null;
  url: string;
}

/** The embedding block in force, defaults filled; `remote`: text leaves this machine. */
export interface LlmEmbeddingRow {
  provider: string;
  model: string;
  batch: number;
  timeoutMs: number;
  queryPrefix: string;
  documentPrefix: string;
  remote: boolean;
  dimensions?: number;
  minSimilarity?: number;
  fingerprint: string;
}

/** An agent as it would run: the model resolved, and who reports its runs - swiss itself, the
 *  child (a Claude Code given the OTEL env), or nobody. */
export interface LlmAgentRow {
  name: string;
  runtime: LlmRuntime;
  provider: string | null;
  model: string | null;
  timeoutMs: number;
  report: boolean;
  reports: "swiss" | "child" | "off";
  maxTokens?: number;
  /** builtin/chat: the request budget in force, in KiB. */
  maxContextKiB?: number;
  maxTurns?: number;
  /** builtin: the grant as written, and the stored names it reaches now. */
  secrets?: string[];
  secretsInForce?: string[];
  /** builtin/chat: the effort set, null for the model's own default. */
  effort?: LlmEffort | null;
  /** In force, the runtime's default filled in. */
  concurrency: number;
  command?: string;
  program?: string;
  programError?: string;
  /** The agent's own prompt, and what is in force without one (null: nothing appended). */
  systemPrompt: string | null;
  systemPromptDefault: string | null;
  /** The agent's list (null: every tool offered) and what it leaves of the catalog. */
  tools: string[] | null;
  toolsInForce: string[];
}

/** One tool of a runtime's catalog. */
export interface LlmToolInfo {
  name: string;
  summary: string;
  /** The db runtime's tools only: the connection kind the tool serves (sql, mongo, redis). */
  on?: string;
}

/** The exporter's counters since the plugin started. */
export interface LlmTelemetryStatus {
  enabled: boolean;
  protocol: LlmProtocol | null;
  endpoint: string | null;
  queued: number;
  sent: number;
  dropped: number;
  rejected: number;
  lastExportMs: number | null;
  lastError: string | null;
  lastErrorMs: number | null;
}

/** GET /api/llm/status. */
export interface LlmStatus {
  serving: boolean;
  providers: LlmProviderRow[];
  embedding: LlmEmbeddingRow | null;
  agents: LlmAgentRow[];
  /** Every runtime's tool vocabulary, in its own spelling. */
  catalog: Record<LlmRuntime, LlmToolInfo[]>;
  /** Each runtime's system prompt when an agent sets none (null: the CLI's own). */
  defaultPrompts: Record<LlmRuntime, string | null>;
  telemetry: LlmTelemetryStatus;
}

/** POST /api/llm/providers/models: the provider's own listing, kinds guessed from the names. */
export interface LlmModelsReply {
  ok: boolean;
  url?: string;
  models?: { id: string; kind: LlmModelKind }[];
  error?: string;
  ms?: number;
}

/** Every Test's answer: a refusal is an answer (`ok: false` and why), not a failed request.
 *  `session` names a reported run in the collector; `env` lists a child's variable names.
 *  A builtin agent's loop also answers `turns` and its token totals. */
export interface LlmTestReply {
  ok: boolean;
  model?: string;
  text?: string;
  error?: string;
  ms?: number;
  dims?: number;
  session?: string;
  program?: string;
  env?: string[];
  protocol?: string;
  endpoint?: string;
  turns?: number;
  tokensIn?: number;
  tokensOut?: number;
  /** A streaming Test's `done` names the conversation it filed. */
  runId?: number;
}

/** POST /api/llm/agents/test/stream - the early `run` frame: which conversation this
 *  ask speaks in, and whether it continues one the reader already had. */
export interface LlmStreamRunFrame {
  runId: number;
  agent: string;
  provider?: string;
  model?: string;
  continued?: boolean;
}

/** The stream's closing `done` frame: `ok: false` carries why (`cancelled: true` when
 *  the loop was stopped); `runId` names the conversation on every outcome. */
export interface LlmStreamDoneFrame {
  ok: boolean;
  runId: number;
  error?: string;
  cancelled?: boolean;
  ms?: number;
  turns?: number;
  tokensIn?: number;
  tokensOut?: number;
  capped?: boolean;
  session?: string;
}

/** A stream ask the server refused BEFORE any frame (409): `code` names the class.
 *  `busy` is recoverable - the conversation lives, the ask may retry; any other code
 *  (closed, over-budget, gone, corrupt, ...) is permanent until New closes it out. */
export interface LlmStreamRefusal {
  error: string;
  code?: string;
}

/** GET /api/llm/history/runs?agent=<n>&open=true - the Test sheet's open-run
 *  discovery: the standard runs envelope, at most one row. */
export interface LlmRunsReply {
  runs: Array<{ id: number; at: string; atEnd: string | null; agent: string; runtime: string; provider: string | null; model: string | null; ok: boolean | null; ms: number; turns: number }>;
  page: number;
  pageSize: number;
  more: boolean;
}
