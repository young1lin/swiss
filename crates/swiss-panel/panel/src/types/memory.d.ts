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

/* The /api/knowledge wire shapes the memory page reads (views/memory*.ts) - field-for-field from
   bin/SWISS_MEMORY_SPEC.md §11.1 ("Wire shapes"), the contract the Rust side serializes. Epoch
   milliseconds everywhere; the page renders them at the edge. Optional fields are the ones the
   contract marks `?` or omits when empty. */

export type MemKind = "decision" | "constraint" | "pitfall" | "knowledge" | "preference";
export type MemStatus = "active" | "needs_review" | "superseded" | "invalidated";
export type MemLinkKind = "supports" | "depends_on" | "contradicts" | "related_to" | "supersedes";
export type MemMarker = "unconfirmed" | "needs_review" | "anchor_changed" | "old" | "contradicted" | "dependency_broken";
export type MemAnchorState = "ok" | "changed" | "missing" | "unchecked" | "none";
/** Who wrote a note, apart from how it arrived (ADR-032). */
export type MemProducer = "human" | "agent" | "bootstrap" | "import" | "sync" | "unknown";
export type MemTransport = "cli" | "mcp" | "panel" | "internal";
export type MemSourceType = "human" | "session" | "code" | "commit" | "test" | "doc" | "bootstrap" | "import";
export type MemRelation = "derived_from" | "supports" | "verified_by";

export interface MemRoot {
  identity: string;
  kind: "git" | "path" | "marker";
  path: string;
  originUrl?: string | null;
  addedMs: number;
  reachable: boolean;
}

export interface MemProject {
  id: string;
  name: string;
  global: boolean;
  createdMs: number;
  roots: MemRoot[];
  counts: { live: number; needsReview: number; unconfirmed: number; history: number; links: number };
}

export interface MemSummary {
  id: string;
  scope: string;
  kind: MemKind;
  title: string;
  status: MemStatus;
  origin: "user" | "agent";
  actor: string;
  producer: MemProducer;
  transport: MemTransport;
  /** A person confirmed it. */
  confirmed: boolean;
  /** Evidence from code, a test, a commit or a doc verifies the current text - apart from `confirmed`. */
  sourceVerified: boolean;
  revision: number;
  updatedMs: number;
  ageDays: number;
  markers: string[];
  linkCount: number;
  excerpt?: string;
  /** The path of the domain the note is filed in; absent: unfiled (SPEC §memory.domains). */
  domain?: string;
}

export interface MemAnchor { path: string; glob: boolean; hash?: string | null; state: MemAnchorState }

export interface MemRecordLink {
  dir: "out" | "in";
  kind: MemLinkKind;
  id: string;
  title: string;
  /** null: the neighbour was purged (the title reads "(purged)"). */
  status: MemStatus | null;
  note?: string | null;
}

/** One piece of a note's evidence, bound to the revision it was attached at; `current` while the
 *  note's text is still the text it was attached to. A line number is never an identity. */
export interface MemEvidence {
  id: string;
  revision: number;
  sourceType: MemSourceType;
  relation: MemRelation;
  path?: string;
  symbol?: string;
  commitSha?: string;
  sessionId?: string;
  runId?: string;
  sourceHash?: string;
  excerptHash?: string;
  actor: string;
  createdMs: number;
  current: boolean;
}

export interface MemRecord extends MemSummary {
  content: string;
  anchors: MemAnchor[];
  refs: string[];
  /** The terms it is searched by, in English and the team's language; absent when none. */
  keywords?: string[];
  supersededBy?: string | null;
  createdMs: number;
  verifiedMs?: number | null;
  links: MemRecordLink[];
  /** Absent when the note has none. */
  evidence?: MemEvidence[];
}

export interface MemSnapshot {
  kind: MemKind;
  title: string;
  content: string;
  status: MemStatus;
  confirmed: boolean;
  anchors: string[];
  refs: string[];
  keywords?: string[];
}

export interface MemRevision {
  revision: number;
  op: string;
  actor: string;
  reason?: string | null;
  atMs: number;
  snapshot: MemSnapshot;
}

export interface MemHit extends MemSummary {
  score: number;
  tier: "id" | "title" | "anchor" | "prefix" | "symbol" | "lexical" | "cjk" | "substring" | "semantic";
  /** The cosine to the query, when the semantic tier ran and the note has a vector. */
  similarity?: number;
  /** The share of the query's word weight the note holds, when a word tier measured it. */
  wordShare?: number;
}

/** POST /related's reply: the neighbours, and why the similar tier did not run when it
 *  did not (the project's settings turn embedding off, SPEC §memory.embedding). */
export interface MemRelatedReply {
  items: MemRelated[];
  semantic?: MemSemantic;
}

/** Whether a search used the semantic tier, and why not (SPEC §memory.embedding). */
export interface MemSemantic {
  state: "off" | "on" | "unavailable";
  reason?: string;
  model?: string;
}

/** One memory question answered (SPEC §memory.rag). */
export interface MemAskReply {
  answer: string;
  agent: string;
  model: string | null;
  mode: "tools" | "inline";
}

export interface MemSearchReply {
  items: MemHit[];
  semantic: MemSemantic;
}

/** What an export says of itself on its header line (an older build's file carries only
 *  the format and the time). */
export interface MemExportHeader {
  formatVersion: number;
  exportedMs: number;
  exporter?: string;
  scope?: "all" | "project" | "domain";
  project?: { id: string; name: string };
  domain?: string;
  visibility?: "project" | "global" | "mixed";
  notes?: number;
  confirmed?: number;
  linksLeftOut?: number;
}

/** A conflict's note as one side holds it. */
export interface MemConflictSide {
  id: string;
  revision: number;
  title: string;
  status: MemStatus;
  confirmed: boolean;
  updatedMs: number;
  excerpt: string;
}

export interface MemImportConflict {
  kind: "revision" | "title";
  line: number;
  incoming: MemConflictSide;
  stored: MemConflictSide;
}

/** POST /import: what the file brought, what the store already had, what it kept out. */
export interface MemImportReport {
  dryRun: boolean;
  source: MemExportHeader;
  into?: string;
  projectsCreated: string[];
  domainsCreated: number;
  domainsSkipped: string[];
  recordsCreated: number;
  recordsSkipped: string[];
  conflicts: MemImportConflict[];
  linksCreated: number;
  linksSkipped: number;
}

export type MemReason =
  | { type: "link"; kind: MemLinkKind; hops: number; via: string }
  | { type: "text"; terms: string[] }
  | { type: "anchor"; paths: string[] }
  /** Close in meaning: the cosine of the two notes' vectors (SPEC §memory.embedding). */
  | { type: "similar"; similarity: number };

export interface MemRelated extends MemSummary {
  score: number;
  reasons: MemReason[];
}

export interface MemNode {
  id: string;
  kind: MemKind;
  title: string;
  status: MemStatus;
  confirmed: boolean;
  origin: "user" | "agent";
  degree: number;
  /** Hops from the center; null when the graph has no center. */
  hop: number | null;
  domain?: string;
}

export interface MemEdge {
  from: string;
  to: string;
  kind: MemLinkKind;
  directed: boolean;
  note?: string | null;
}

export interface MemStatusReply {
  dbPath: string;
  schemaVersion: number;
  mcp: { state: "serving" | "conflicted" | "stopped"; path: string };
}

/** The embedding block memory serves by - the llm plugin's (SPEC §llm.embedding), defaults
 *  filled; `{provider: "off"}` while there is none. `provider` names an llm providers entry. */
export interface MemEmbeddingConfig {
  provider: string;
  model?: string;
  dimensions?: number;
  batch?: number;
  timeoutMs?: number;
  queryPrefix?: string;
  documentPrefix?: string;
  /** The least cosine that counts as close in meaning; absent, the library's 0.3. */
  minSimilarity?: number;
}

/** The index at a glance: `indexed` live notes have a current vector, `pending` do not yet.
 *  `stale`: `incompatible` vectors are of another width than the provider answers now - never
 *  scored, embedded again by the next run. `dims` is the index's one width. */
export interface MemEmbeddingStatus {
  state: "off" | "ready" | "indexing" | "stale" | "error";
  model: string | null;
  dims: number | null;
  indexed: number;
  pending: number;
  incompatible: number;
  lastError: string | null;
  lastRunMs: number | null;
}

/** GET /api/knowledge/embedding. `remote`: note text leaves this machine (a non-loopback
 *  endpoint); `configError`: the block parsed but its client could not be built. */
export interface MemEmbeddingReply {
  config: MemEmbeddingConfig;
  remote: boolean;
  configError: string | null;
  status: MemEmbeddingStatus;
}

/** POST /api/knowledge/embedding/test: a refusal is an answer (`ok: false`), not a failure. */
export interface MemEmbeddingTest {
  ok: boolean;
  model?: string;
  dims?: number;
  ms?: number;
  error?: string;
}

/** Why a checkout gave no history (SPEC §memory.domains): the panel words it; `reason` is git's. */
export type MemGitIssue = "no_directory" | "not_a_repository" | "git_missing" | "refused" | "git_failed";

/** Every non-2xx body: the §7 code (or, from a stopped plugin, the host's message alone), the
 *  sentence, and the code's extra fields. */
export interface MemErrorBody {
  error: string;
  message?: string;
  /** memory_git_unavailable (and a project with no reachable directory): why. */
  issue?: MemGitIssue;
  projectId?: string;
  current?: number;
  similar?: { id: string; revision: number; title: string };
  rule?: string;
  possibleMatch?: { projectId: string; name: string };
}

/* ------------------------------------------------------------------ domains (§memory.domains) */

/** A domain's live notes (filed in it directly), and the ones of them waiting on a person. */
export interface MemDomainCounts { live: number; needsReview: number; unconfirmed: number }

/** One node of a project's map. `body` (the hub) only where one domain is read. */
export interface MemDomain {
  id: string;
  scope: string;
  path: string;
  title: string;
  summary: string;
  body?: string;
  owns: string[];
  /** The terms it is searched by, in English and the team's language; absent when none. */
  keywords?: string[];
  origin: "user" | "agent";
  actor: string;
  confirmed: boolean;
  revision: number;
  createdMs: number;
  updatedMs: number;
  /** When the hub was last true to the code: a create, a body or owns change, a verify. */
  verifiedMs: number;
  depth: number;
  parent?: string;
  subDomains: number;
  counts: MemDomainCounts;
}

/** GET /domains: the map in tree order. */
export interface MemDomainList { project: string; items: MemDomain[]; unfiled: MemDomainCounts }

export interface MemDomainRef { id: string; path: string; title: string }

export interface MemDomainSnapshot { path: string; title: string; summary: string; body: string; owns: string[]; keywords?: string[]; confirmed: boolean }

export interface MemDomainRevision {
  revision: number;
  op: string;
  actor: string;
  reason?: string;
  atMs: number;
  snapshot: MemDomainSnapshot;
}

/** GET /domains/{id}: the domain, where it sits, what is under it; its revisions on `history`. */
export interface MemDomainDetail extends MemDomain {
  ancestors: MemDomainRef[];
  children: MemDomain[];
  notes: MemSummary[];
  history?: MemDomainRevision[];
}

/** A commit as coverage names it: the latest one that touched owned code. */
export interface MemCommitFiles { hash: string; atMs: number; subject: string }

/** GET /domains/coverage. `git` false: no drift and no uncovered list - `issue` says why. */
export interface MemCoverage {
  git: boolean;
  issue?: MemGitIssue;
  reason?: string;
  domains: Array<{ id: string; path: string; counts: MemDomainCounts; files?: number; drift?: number; latest?: MemCommitFiles }>;
  unfiled: MemDomainCounts;
  uncovered: Array<{ path: string; files: number }>;
  filesTruncated: boolean;
}

export interface MemFileOutcome { moved: number; domain?: MemDomainRef }
export interface MemDomainDeleted { path: string; refiled: number; to?: string }

/** POST domains/{id}/confirm with `all`: the domain whole - what was confirmed, and the notes
 *  that need review, left for a verdict. */
export interface MemDomainConfirmed { domain: MemDomain; domains: number; notes: number; held: number }

/* ------------------------------------------------------------------ previews (§memory.preview) */

export interface MemDirEntry { name: string; dir: boolean; size?: number; sensitive: boolean }

/** GET /api/knowledge/preview/file: the file or the directory at `path` ("" is the root). */
export type MemFilePreview = { root: string; path: string } & (
  | { kind: "file"; size: number; modifiedMs: number | null; binary: boolean; truncated: boolean; lines: number; text: string | null }
  | { kind: "dir"; entries: MemDirEntry[]; truncated: boolean }
);

/** GET /api/knowledge/preview/files: what a glob anchor matches now. */
export interface MemGlobFiles { root: string; glob: string; files: string[]; truncated: boolean; git: boolean }

export interface MemCommit { hash: string; short: string; author: string; dateMs: number; subject: string }

/** GET /api/knowledge/preview/log. */
export interface MemHistory {
  root: string;
  path: string;
  commits: MemCommit[];
  more: boolean;
  dirty: boolean;
  untracked: boolean;
}

export interface MemChanged { path: string; status: string; additions: number | null; deletions: number | null }

/** GET /api/knowledge/preview/commit: the patch is `path`'s alone when one was asked. */
export interface MemCommitDetail extends MemCommit {
  root: string;
  body: string;
  parents: string[];
  files: MemChanged[];
  path?: string;
  patch: string;
  truncated: boolean;
}

/** GET /api/knowledge/preview/diff: the working tree and the index against HEAD. */
export interface MemWorkingDiff { root: string; path: string; patch: string; truncated: boolean; untracked: boolean }

/** The plugin row's `sync` block (SPEC §memory.sync): `agent` names one llm agents entry,
 *  a list of them - a pool - or "off"; absent, the layers below decide (the project's settings,
 *  then the newest agent; SPEC §memory.settings); `lead` names the one that reviews the map and
 *  wraps up the domains a full run wrote in. */
export interface MemSyncBlock {
  agent?: string | string[];
  lead?: string;
  transcripts?: "off" | "claude-code";
  transcriptDirs?: string[];
  auto?: boolean;
  minIntervalSec?: number;
}

/** An agent a run asks, as the llm row holds it now: its runtime and model, or the sentence a
 *  run would fail with (no such agent, its provider gone, the plugin off). */
export interface MemSyncAgent {
  name: string;
  runtime?: string;
  model?: string | null;
  error?: string;
}

/** What one layer said about a run's agents (SPEC §memory.settings): null - the next layer
 *  down decides - "off", or the names (the lead's one name in the settings). */
export type MemAgentsChoice = "off" | string[];
export type MemLeadChoice = "off" | string;

/** Whom a run asks, and the layer that decided it (SPEC §memory.settings): `from` null when
 *  no layer named an agent and none stood in (a lead never falls back). */
export interface MemPick {
  names: string[];
  from: "run" | "project" | "plugin" | "newest" | null;
}

/** A project's own settings (GET/PATCH /api/knowledge/projects/{id}/settings), every field
 *  null where it inherits (SPEC §memory.settings). `embedding` null inherits (on whenever a
 *  model is configured, §memory.embedding); the global project may set it alone. `autoReview`
 *  is the time of day ("HH:MM", the gateway's clock) a review of the notes whose code changed is
 *  queued each day, null for none; `autoReviewRuns` the runs that day may take while each leaves
 *  questions for the next (1 to 10), null for the default 3. */
export interface MemProjectSettings {
  agents: MemAgentsChoice | null;
  lead: MemLeadChoice | null;
  embedding: boolean | null;
  autoReview?: string | null;
  autoReviewRuns?: number | null;
}

/** One worker of a pool (SPEC §memory.settings): the questions handed it - a retry on another
 *  worker counts there too - and those that failed. */
export interface MemWorkerTally {
  label: string;
  model?: string;
  asked: number;
  failed: number;
}

/** A full run's wrap-up by the lead: the domains it wrote in, the ones wrapped up, the hubs
 *  refreshed and the notes saved (held for review among them), the questions that failed. */
export interface MemWrapupScope { domains: number; wrapped: number; hubs: number; saved: number; held: number; failed: number }

/** A recheck (SPEC §memory.init The recheck): the notes it judged, the domains they sit in, its
 *  questions (asked, failed, left for a later recheck by the cap), and what the answers came to -
 *  kept, fixed, dropped, merged, links added, notes the code check still holds, operations
 *  refused, notes an answer left unjudged - and the contradictions it lifted, the rounds it asked
 *  again to settle the contradictions its answers raised or left standing, and those still open at
 *  its end. A review run (the inbox handed over) counts the notes it confirmed too. */
export interface MemRecheckScope {
  domains: number; notes: number; questions: number; asked: number; failed: number; more: number;
  kept: number; fixed: number; dropped: number; merged: number; linked: number; held: number; refused: number; left: number;
  confirmed?: number; lifted?: number; rounds?: number; open?: number;
}

/** A docs run (SPEC §memory.docs): the read - the sources named and those missing from the
 *  checkout, the documents found, added, changed, unchanged and removed, the files not kept -
 *  then the cards: the documents waiting for one, the questions (asked, failed, left by the
 *  cap), the cards saved, outrun by a change or refused, and the code names kept and dropped. */
export interface MemDocsScope {
  sources: number; missing?: string[]; files: number; bytes: number; added: number; changed: number; unchanged: number;
  removed: number; cut: number; unread: number; waiting: number; questions: number; asked: number; failed: number;
  more: number; carded: number; stale: number; refused: number; symbols: number; dropped: number;
  /** The cards whose code names the run looked up again and found elsewhere. */
  relinked?: number;
}

/** One sync run as the engine serialises it (camelCase): the queue row, its scope, the
 *  operations an applied proposal came to, and the runner's own account. */
export interface MemSyncRun {
  id: string;
  project: string;
  trigger: "manual" | "nudge" | "map" | "full" | "recheck" | "docs" | "review";
  dryRun: boolean;
  state: "queued" | "running" | "ok" | "empty" | "failed" | "cancelled";
  queuedMs: number;
  dueMs: number;
  startedMs?: number;
  finishedMs?: number;
  since?: string;
  base?: string;
  head?: string;
  ops?: Array<{ op: string; outcome: string; detail?: string }>;
  /** A map run's count: areas cut, questions asked, answers that filled a hub; a redraw's
   *  removed domains; a full run's cap and its knowledge phase; the phase a map or full run
   *  is in (or, cancelled, stopped in); a recheck's count, a full run's closing one too; a
   *  docs run's read and cards. */
  scope?: {
    language?: string; units?: number; asks?: number; filled?: number;
    redraw?: boolean; removed?: string[]; maxAsks?: number; knowledge?: MemKnowledgeScope;
    phase?: MemSyncPhase; wrapup?: MemWrapupScope; recheck?: MemRecheckScope; docs?: MemDocsScope;
    /** A review run of the notes whose code changed alone (SPEC §memory.init The review run). */
    changed?: boolean;
    /** The run's own pick of its workers and lead (SPEC §memory.settings); absent: inherited. */
    agents?: MemAgentsChoice;
    lead?: "off" | string[];
  };
  runner?: {
    runner: string; model?: string; attempts: number; stderr?: string; lead?: string;
    /** How each worker of a pool went, when the run asked more than one (SPEC §memory.settings). */
    workers?: MemWorkerTally[];
  };
  summary: string;
  error?: string;
}

/** Where a map, full, recheck or docs run is (SPEC §memory.sync Progress). */
export type MemSyncPhase = "review" | "areas" | "knowledge" | "wrapup" | "recheck" | "docs" | "ask" | "apply";

/** A full run's knowledge phase (SPEC §memory.init): the sources cut into slices, the
 *  slices asked, skipped as unchanged or left for a later run, the notes they came to. */
export interface MemKnowledgeScope {
  files: number; bytes: number; slices: number; asked: number; unchanged: number; more: number; failed: number;
  proposed: number; saved: number; held: number; duplicates: number; refused: number;
  embedding?: MemEmbeddingScope;
}

/** The semantic index as a full run (or its plan) saw it: "off" is lexical search only. */
export interface MemEmbeddingScope {
  state: "off" | "ready" | "indexing" | "stale" | "error";
  model?: string;
  indexed: number;
  pending: number;
  error?: string;
}

/** One area a map run would draw (SPEC §memory.map); `kept` is one already on the map. */
export interface MemMapUnit {
  path: string;
  title: string;
  kind: "package" | "dir" | "stem";
  owns: string[];
  files: number;
  uses?: string[];
  kept?: boolean;
}

/** GET /api/knowledge/sync/map: the areas the code is cut into now, no model asked;
 *  `review` when the top level goes to the agent first, `removed` what a redraw sets aside. */
export interface MemMapPlan {
  plan: MemMapUnit[]; asks: number; codeFiles: number; ownedFiles: number; mappedFiles: number; truncated: boolean;
  review?: boolean; removed?: string[];
}

/** GET /api/knowledge/sync/full: the map plan, then the slices a full run would read and
 *  ask about, and the semantic index its notes would land in. */
export interface MemFullPlan extends MemMapPlan {
  knowledge: { files: number; bytes: number; slices: number; asks: number; unchanged: number; more: number; unread: number };
  embedding: MemEmbeddingScope;
}

/** GET /api/knowledge/sync: where the project stands and the last runs. */
export interface MemSyncOverview {
  config?: MemSyncBlock | null;
  configError?: string | null;
  /** The first worker a run would ask (kept for the block editor); see `workers`. */
  agent?: MemSyncAgent | null;
  /** The workers a run would ask now, and its lead, each as the llm row holds it (SPEC
   *  §memory.settings). */
  workers?: MemSyncAgent[];
  lead?: MemSyncAgent | null;
  /** Whom a run would ask, and which layer decided (run / project / plugin / newest). */
  chosen?: { agents: MemPick; lead: MemPick };
  /** The project's own settings (SPEC §memory.settings). */
  settings?: MemProjectSettings;
  state: { head?: string | null; lastFinishedMs?: number; pendingCommits: number; pendingBytes: number };
  runs: MemSyncRun[];
  /** The running runs told to stop (SPEC §memory.sync Cancel): each ends cancelled at its next question. */
  stopping?: string[];
}
