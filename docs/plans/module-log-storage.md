# Module-owned log storage: SPEC amendment and delivery plan

This is the implementation brief requested by the user, not a second canonical specification or a claim that the feature already exists. The sole current-behaviour specification remains [SPEC](../SPEC.md). Apply the draft amendments below **with the implementation commit that makes each paragraph true**, never as an advance future-behaviour edit to SPEC.

The deliverable is documentation only. No feature implementation, dependency update, schema migration, build, test run, live instance restart or production deployment accompanies it.

## 1. Authority, baseline and scope

### Confirmed requirements

- Use SQLite through sqlx 0.9 with FTS5; reuse the project's engine family instead of adding redb, Fjall, LMDB or Tantivy.
- There is **one database per module, not per MCP, server, client, target or record**. MCP Calls and Traffic share the MCP module's database. All Remote targets share the Remote module's database.
- Stopping, failing, clearing or maintaining module A does not close, cancel or mutate the unrelated module B's storage or work.
- Support typed field filters, retained-body text search, efficient sequential pagination, bounded detail/output reads and meaningful retained history.
- Prefer compact storage. A normal total log budget of approximately 500 MB is acceptable. WAL/SHM sidecars are allowed.
- Host shares storage machinery and contracts, not one live connection pool, writer queue or lifecycle for all modules.
- This brief is written for another implementation session. It does not authorize accidental production iteration or a separate speculative specification.

### Defaults adopted for this draft

The user ended further interviewing and requested a usable summary. The following are explicit implementation defaults, **not invented claims that the user individually answered every interview question**:

| Decision | Default |
|---|---|
| First delivery scope | MCP Calls + Traffic, and Remote Runs/history only. Jobs, daemon diagnostics, terminal recordings and Memory remain in their current stores. |
| File paths | SWISS_HOME/logs/mcp.db and SWISS_HOME/logs/remote.db, with their SQLite sidecars. The prefix denotes the resolved Swiss home, not a literal backslash path. |
| Normal budget | 500,000,000 bytes, decimal MB, for these two managed module histories. Default shares: MCP 200,000,000; Remote 300,000,000. Configurable; adding resources never multiplies a share. |
| Protection versus quota | Preserve Remote's seven-day protected metadata guarantee. It can cause **steady-state** overrun after eligible bodies are reclaimed; expose this explicitly, not as fictitious temporary maintenance. This is a soft budget, not a hard filesystem ceiling. |
| Module shutdown | Withdraw new module work, cancel/drain related accepted work, acknowledge accepted writes and close only that module. Remote cancellation is by accepted remote action/run identity, not submitting owner name. |
| Disabled history | Keep files; do not automatically reopen them through list, detail, usage or global search. Report disabled/skipped. Explicit offline read-only browsing is out of this delivery. |
| Privacy | Only explicitly owner-approved safe projections may enter plaintext FTS. Sensitive or unclassified raw bodies are encrypted; authorized full-body search scans them. Key-name masking alone never proves text safe. |
| Legacy import | Resumable, verified, idempotent import, then one authority cutover. Keep original legacy files until an explicit operator cleanup decision. Do not create new plaintext copies of sensitive data. |
| UI | Upgrade the existing Calls, Traffic and Remote Runs views. Supply the cross-module query contract, but do not add a global log page. |
| Capture scope | Search all **retained captured** text. Preserve existing capture/truncation rules initially; do not begin capturing previously excluded stdin or reconstruct historically lost bytes. |
| Ordinary durability | MCP history uses WAL + NORMAL if the engine gate passes; acknowledged SQL commits are visible, but latest commits can be lost on power failure. No claim of a fixed loss interval without evidence. |
| Protected durability | Remote's mixed audit/history database uses WAL + FULL for all its writes; do not claim per-row reliability from toggling a connection pragma. Migration checkpoints/authority markers use strict durability. |

### Source baseline and integration warning

- Worktree: swiss-test-1, based on local master at 7772e1ae2fd7b2539e0f66b07b4e65f551149ebb.
- The pre-existing local Cargo configuration change isolates this worktree to port 19997 and the test home; preserve it. It is not part of this feature.
- Main checkout is reference-only: its pending Memory subsystem already uses sqlx 0.9 + SQLite + FTS5. **That Memory code is absent from this master-based worktree.** Do not copy unrelated uncommitted work to obtain SQLite and do not make a peer depend on swiss-memory.
- The worktree ends at ADR-030. Main already contains ADR-031 and ADR-032. Allocate the next free ADR on integration; do not reserve or overwrite 031/032.
- No current SPEC, feature source or manifest is changed by this document.

## 2. Constitution and architectural ownership

The design preserves ruthlessly small, plugin-shaped and hot-pluggable behaviour: lazy, bounded resources; module ownership; shared mechanism below peers; no expensive instance hidden behind an enabled flag. It does not change how tools enter Swiss, introduce a database daemon, create a new preset/mode, or require another async/TLS/search runtime.

Host provides a HistoryStoreFactory and reusable ModuleHistory machinery through RuntimeServices. The factory accepts an opaque module identity and declared policy. It has no MCP-versus-Remote match arm. Common storage/schema/query mechanics live in swiss-host; crypto and platform primitives remain at their existing lower boundaries. Plugins own capture, sensitivity classification, source semantics, retention exceptions, legacy adapters and optional owner schema extensions.

Each enabled owner separately controls:

- its SQLite connection/pool, statement cache and worker lifetime;
- its byte-bounded queue, flush barriers and module generation;
- its migration, retention, checkpoint and index-maintenance tasks;
- its query provider, body reader, import ledger and error state;
- its schema versions and quota. One owner's SQL migration or clear cannot act on another database.

Start with at most one connection per active module, zero minimum connections, bounded idle lifetime and no connection per resource. Measure SQLx's per-connection worker cost; reusing the engine does not mean zero extra threads or memory. Factory construction and disabled module construction perform no log-file open, restore, migration or orphan sweep.

Same-home multiport gateways require actual cross-process safety. SQLite protects owner transactions, but old legacy writers do not honor a new import lease. Coordinate migration and host run-ID allocation; never imply an in-process mutex or single-connection pool solves other processes. Disabling a module in one gateway closes only that gateway's handles, not another gateway's database or WAL.

## 3. Engine prerequisite: before WAL or import

**Do not ship the audited bundled engine unchanged.** Main's locked native binding is libsqlite3-sys 0.30.1. Its cached non-SQLCipher header reports SQLite 3.46.0. This is compile-source evidence, not a query of the running executable.

[SQLite's official WAL-reset notice](https://sqlite.org/wal.html#walresetbug) reports rare corruption for 3.7.0 through 3.51.2 when multiple connections race on write/checkpoint. The straightforward release gate is SQLite >= 3.51.3. Only an explicitly reviewed fixed backport/source-ID allowlist can replace that gate; the predicate >= 3.44.6 is wrong because an intermediate 3.46.0 remains affected.

Before changing an owner file to WAL, importing logs or accepting writes:

1. Align one SQLite native engine with the existing sqlx family. Check the exact SQLx/native-binding version constraint; changing a lockfile number is not a dependency solution.
2. Prefer an upstream compatible fixed engine. If a reviewed backport is necessary, document its source and maintenance cost; do not silently ship a second SQLite or system-installed DLL dependency.
3. Observe sqlite_version() and sqlite_source_id() from the actual linked runtime in a disposable probe.
4. Probe FTS5, trigram, contentless-delete and incremental-vacuum capabilities with the shipping feature set. A manifest or historical Memory spike is not runtime evidence.
5. Keep default-features=false and only needed runtime-tokio/sqlite features. Regenerate the lockfile, inspect the normal/build dependency tree and update notices when needed.

The new history schema uses contentless-delete only after the probe passes. It needs SQLite >= 3.43.0; the stronger WAL gate already exceeds this. Do not transplant Memory's external-content delete triggers into a contentless-delete table.

## 4. Lifecycle and failure contracts

### Cheap facades, explicit live owners

Existing AppContext, adapters and run sinks keep long-lived Arcs. Their stable facade is not the database owner: it dispatches through the current module generation and rejects disabled, failed or stale operations without opening storage.

Move live database setup to enabled, bounded lifecycle work. Factory.create is currently outside START_TIMEOUT; it is not a place for SQL open or migration. Enabled first-use opens have a deadline and can be cancelled. Legacy migration runs before that owner accepts normal history writes, in bounded, resumable batches. If it does not finish within the start budget, preserve committed import progress and report a resumable import/deadline status; do not turn a large import into an unbounded create call or silently start dual legacy/new capture.

History health is distinct from plugin/tool health. A storage error does not crash the gateway or unrelated modules. Preserve fail-open MCP execution on logging errors, with recording-unavailable/drop counters. A successful Remote execution is not retroactively changed into an execution failure by a failed history commit; its terminal view honestly reports history unavailable. Do not fabricate a persisted audit row.

### Shutdown

For successful stop, within the existing 10-second module stop contract:

1. Withdraw serving routes/new history work and triggers.
2. Cancel/drain related accepted actions and prevent new leases.
3. Await admitted history writes and final-record acknowledgments.
4. Stop that owner's maintenance and queries; close its pool/worker.
5. Unregister contributions, invalidate its generation and release its leases.

The shutdown deadline covers the operation as a whole, not a fresh timeout for each task. Aborted futures or dropped Arcs are not evidence that a SQL worker closed. On timeout, reject further writes from that generation, account unresolved data and report a truthful failed/stopping state with lastError. Keep any required close supervision module-local; do not report Disabled/closed until the release invariant holds. Never unlink active WAL/SHM files to force release.

Remote sink detachment alone does not drain accepted runs. Existing producers include manual, remote-mcp and Jobs owners. Track accepted remote action/run leases; cancel those related actions regardless of submitting owner. Do not call global services.shutdown or cancel all Jobs/process work. Old tees cannot write into a newly enabled generation.

### Remote completion acknowledgment

The current synchronous RunHistorySink.finish contract promises history visibility before terminal publication. Convert it to an acknowledged asynchronous completion contract, adapting the legacy sink first. The coordinator awaits successful persistence before claiming recorded completion. Bounded persistence failure publishes a distinct history error rather than false success. Streaming tee enqueueing is byte-bounded and nonblocking for the execution path; overflow produces explicit capture gaps/truncation/drop counters, not an unbounded buffer or silent complete output.

The existing combined hourly Calls/Jobs sweep is split by owner. Stopping MCP never stops Jobs' unchanged maintenance.

## 5. Database and body model

Use the following common conceptual tables, with module-specific adapters rather than engine details in plugins:

- meta: owner/application identity, schema_version, import state, persistent internal sequence/highwaters and policy versions;
- resources: stable source IDs and current labels/rename aliases, never file-per-resource;
- events: kind, resource, original source IDs, timestamps/order, status, safe filter fields, summary and coverage;
- body_parts/chunks: event/part identity, original byte range, storage codec, sensitivity, capture/gap state and body generation;
- search_documents: derived index rows mapped to event/part/range/version;
- import_ledger: immutable source generation/path/byte-offset locators, progress and validation/cleanup receipts.

Version database schema, body payload, search projection, import protocol and cursor independently. Unknown newer versions produce an explicit owner storage error without dropping/recreating authoritative history. Rebuildable indexes are not a license to erase records.

Use 64 KiB target plaintext chunks, aligned to valid text boundaries. Smaller final chunks are valid. Reads default to 64 KiB and cap at 128 KiB. Original UTF-8/output bytes define offsets; neither SQL TEXT character positions, compressed bytes nor ciphertext positions define the public byte cursor. Discontinuous retained head/tail regions remain discontinuous.

Use independently decompressible blocks; prefer a measured minimal-feature pure-Rust LZ4 block codec, retain raw blocks when compression expands them. No new codec dependency lands without the dependency/size gates. No whole-database compressed/encrypted JSON container. Enforce declared plaintext size before decompression and allocation.

Event metadata, each admitted body batch, derived search changes and progress/coverage change in one owner transaction. A streamed run is **not** one transaction held open for its entire execution. Short append transactions create staging bodies; an acknowledged final transaction marks completion after admitted chunks/index work. Restart exposes interrupted captures as interrupted, not falsely complete.

Keep source call/traffic sequence and host run ID separate from the new internal record identity. Internal IDs and cursor generations survive clear/retention without reuse. Preserve existing legacy wire sequence reset behaviour where required, using a clear generation to distinguish reused legacy numbers. Never clear or move the host-wide runs.seq allocator into a module DB; coordinate its allocation across processes before claiming shared-home uniqueness.

## 6. Security and encrypted search

Admin-session and loopback protection apply to every query, body read, resume, usage and cleanup route. CLI key authorization remains the existing gate; the MCP bearer never authorizes admin history. Reauthorize every resume before key lookup/decryption. Denied fields do not leak hit IDs/counts/snippets.

Sensitivity propagates to every derivative: args/params, replies, errors, previews, output tails, snippets, FTS/shadow tables, WAL, backups, diagnostics and cursor state. Only a producer/owner-approved safe projection is plaintext-indexed. Unknown raw upstream text is not safe merely because recursive key masking found no password key. Do not resolve environment/vault references during import or search.

For sensitive chunks, compress before encryption and reuse the **unchanged frozen sealed envelope**. Each chunk is independently authenticated. Put module/resource/event/part/generation/chunk/range/codec identity inside the authenticated plaintext wrapper and verify it against the row after tag verification, preventing valid ciphertext swaps. Frozen no-AAD semantics and wire literals stay unchanged. A capped legacy whole envelope is authenticated in full before re-chunking; it is not a random-decrypt byte stream.

Sensitive SQLite blobs use strict envelope parsing with no plaintext fallback. Never use the forgiving config helper that seals plaintext on read as a body-reader fallback. Missing key, unknown version, malformed envelope or bad tag preserves the original and reports unreadable coverage; it never becomes empty successful content.

Plaintext is allowed transiently in authorized request memory. It is not persisted in query diagnostics, scan caches, snippets, continuation carry or temporary tables. Use memory-only temporary query work and disable sensitive parameter/statement tracing. A private legacy plaintext file is still plaintext: retain existing files only with an explicit exposure/accounting warning pending cleanup; do not duplicate them into a new allegedly encrypted backup.

## 7. Query, scan and cursor contract

### Literal semantics

Full search covers every eligible retained captured field/body, including authorized encrypted parts. It does not require q to match safe metadata before considering a sensitive body. Capture truncation, evicted bodies, legacy previews and excluded stdin are reported, not reconstructed.

Default q is a literal substring, at most 256 Unicode scalar characters, ASCII-case-insensitive and otherwise non-ASCII-preserving. FTS operators, punctuation, quotes, paths and SQL snippets are text, not an exposed query language. Bind SQL parameters and encode an FTS literal using the engine's documented quoting rules.

For safe text with q >= 3 characters, use trigram/detail=full MATCH to obtain candidates, then exact canonical verification and event deduplication. Contentless columns are NULL: do not assume LIKE can read the original body. Carry up to the preceding 255 characters when indexing a contiguous safe field so cross-chunk literals are not lost. Do not carry across events, fields, sensitivity boundaries or retained gaps. For q < 3, or sensitive/unindexed text, use bounded scanning with the same exact semantics.

### Bounded full search

Each request has configurable byte/chunk/time limits; conservative initial defaults are 8 MiB examined plaintext, 128 chunks and 200 ms work, with a bounded scheduling allowance. These are work budgets, not a promised latency SLA or idle-RSS measurement. Scan incrementally and yield; do not deserialize all retained history.

Carry matching state across text chunks and resumes in request memory or reconstruct it from authenticated preceding chunks. A Unicode character/display boundary helper alone is not a substring matcher. Crossing matches are emitted once. Never join unrelated fields or a retained head and tail into text that did not exist.

A zero-hit batch with remaining eligible data is incomplete and has a continuation; it is **not** No matches. Overall completeness requires examining the declared scope. Return examined/coverage information for truncation, gaps, eviction and unreadable data. Clear match completion is distinct from capture completeness.

### Records, snapshots and mixed results

Keyset pages use a total order such as timestamp plus stable internal identity; Remote completion can be out of run-ID order. Capture per-module highwaters and body generations for a query, without holding a database transaction or retention pin across HTTP requests. New rows do not shift an old page. Query/filter/resource generation changes and cleared/replaced bodies explicitly expire the cursor.

The opaque authenticated/encrypted cursor binds version, scope, query/filter fingerprint, normalization, authorization/session scope, highwaters and the last **examined** row/byte frontier. It does not expose query plaintext or body carry. Resume reauthorizes.

Cross-module results merge by a documented total order, not incomparable per-database BM25 scores. Indexed safe hits and scanned sensitive hits form a union before final paging, never an intersection. Do not finalize a sorted page while another participating stream's unexamined frontier could contain an earlier eligible hit; otherwise return progress/provisional results with honest completeness. A disabled module is reported skipped, never opened by aggregation.

## 8. API and panel amendment surface

Add descriptor/service-contributed, admin-protected endpoints; no new host plugin-type match arm:

- POST /api/history/query: modules/kinds/resources/typed filters/q/cursor/limit. Response: metadata-only records, nextCursor, complete, coverage, scan progress, skipped modules and capture gaps.
- GET /api/history/usage: owner quota, actual known physical bytes, sidecars, backup/maintenance bytes, reservations and overrun reasons. Disabled owners use their last known snapshot without opening the database.
- GET /api/history/{module}/records/{id}/body: part, plaintext after byte cursor and max. The response is bounded with next byte frontier, availability and truncation/gap state.

Cursor and byte-after fields have different types/contracts. Validate limits, invalid cursors and literal query length explicitly. Never expose table names or arbitrary SQL/FTS query strings.

Retain compatibility adapters for current module endpoints: Calls page/pageSize/seq/detail/clear; Traffic page/total/client/detail/clear; Remote before/nextBefore/filters/output/content/export. Pin absent optional-field behaviour and 404 versus unavailable/disabled errors. Legacy numeric paging may still require skip work and is not the efficient new cursor path. New panels use the new literal/full-body cursor contract; existing legacy-q compatibility does not masquerade as complete sensitive-body search.

Traffic's 500-row ring ceases to be the query authority. A bounded live cache may remain, but the archive and totals cover retained rows, including migrated valid recovery-tail entries. Document this deliberate retention/query expansion and replace the old ring-only assertions when delivered.

Calls body detail keeps legacy preview/body/bodyGone semantics and resource rename aliases. Remote output keeps its existing byte-exact UTF-8 contracts, head/tail caps and active-to-stored transition. Host run IDs remain independent of history retention.

Existing Calls, Traffic and Remote Runs views gain filters, literal full-text input, cursor navigation, bounded detail loading, search continuation/progress and honest unavailable/evicted/incomplete states. No global log page or new preset. Do not auto-poll an older reading position or rebuild an expanded live output on each list refresh. Use the in-tree UI library and tr/trn; English and Chinese tests/walks are required. Never render an incomplete zero-hit scan as No matches.

## 9. Retention, accounting and maintenance

Module policies preserve existing capture limits initially: Calls' stored long replies are capped in Unicode characters, Traffic capture is character-clipped, Remote output keeps its current head/tail limits and sealed write content its existing 256 KiB bound. Document the units correctly. Capturing more upstream data is a separate amendment.

Calls' age/long-body policy and Remote's age/count/audit policy stay source-owned. New Traffic archive retention is governed by its module policy/share rather than the 500-row query ring. A module clear selects only its kind/resource/client and atomically removes corresponding chunks/search rows, without clearing its sibling Calls/Traffic scope or another module.

Actual physical accounting includes the owner database (already including indexes), WAL/SHM/journal, in-flight reservations, managed migration/backup artifacts and maintenance temporary files. Do not double-count FTS atop database file size, credit free pages as released filesystem bytes, or report max_page_count/wal_autocheckpoint as a total disk ceiling. Keep normal, audit-protected, backup-awaiting-cleanup and maintenance overrun reasons distinct.

Owner transactions and reservations coordinate concurrent writers/processes. Reconcile abandoned reservations without deleting another process's active work. Each owner reclaims only its own oldest eligible rows/bodies and indexes. No global cross-database transaction or retention lock may hold unrelated module work hostage.

Set incremental auto-vacuum before creating a new schema if selected; an existing database requires an explicit migration plan. Bound incremental vacuum, checkpoint and FTS merge work. Deletion may enlarge WAL and FTS tombstones need index maintenance. Long readers must not pin an old snapshot across requests. Busy checkpoints defer physical reclaimed credit, not fabricate it. Full VACUUM is not routine per-append cleanup.

Remote metadata inside the seven-day protected window is not silently evicted for share/global pressure. Drop eligible bodies/index entries first and report sustained protected overrun if metadata alone exceeds the soft budget. General disk exhaustion and capture-queue overflow remain possible: record pressure/unavailable/drop state without failing unrelated executed tools. This policy does not promise an absolute 500 MB filesystem maximum.

## 10. Legacy import and recovery

1. Quiesce affected legacy writers and obtain a module migration lease. Old binaries cannot be assumed to honor a new marker. Detect mutation/growth and stop import if exclusivity is not established; never kill another live instance automatically.
2. Inventory source-relative paths, sizes, generations and fingerprints. Import keys are generation/path/original byte offset, not legacy seq alone; old clear operations may reuse seq.
3. Create private staging/IMPORTING state at the owner store. Normal queries cannot pretend an incomplete import is complete.
4. In bounded BEGIN IMMEDIATE batches, commit records, already-protected chunks, safe index entries, identity/highwaters and source progress together. Crash retry is idempotent.
5. Preserve original IDs, optional-field absence, bodyGone/truncated/evicted flags, client identities, aliases and byte ranges. Import valid retained source data, not invented lost originals. Account malformed lines/torn tails explicitly with a private non-leaking quarantine/diagnostic.
6. Verify a reopened store: inventory completeness, counts/locators, identity floors, body bytes/flags, authenticated binding, index consistency, integrity and foreign keys. Missing key/tag errors preserve source and stop successful cutover; no plaintext or empty-content fallback.
7. Commit verified ACTIVE authority once. New writers use SQLite only; no dual-write. A crash after ACTIVE never reimports remaining legacy files or resurrects rows cleared in SQLite.
8. Keep existing sources as inventoried legacy backup pending explicit operator cleanup, count them and disclose plaintext exposure. Cleanup is recorded and resumable; it never deletes an unverified artifact.

No downgrade is automatic: old files do not contain post-cutover writes. A later downgrade requires quiescing writers, authorized export of all understood new records into verified compatible temporary legacy shapes, a controlled switch and retention of the new store until verified. Do not implement a lossy old-binary rollback by reopening stale originals. Never back up a live WAL database by copying only its main file.

## 11. Proposed in-place SPEC amendment ownership

The following are **target paragraphs**, not edits to the current SPEC. Replace contradicted wording when delivered; preserve existing anchors. Every normative rule below has named enforcing tests in section 12.

### New host storage subsection: SPEC §host.history

> The host provides a history-storage factory, not a process-global database pool. A module owns its connections, bounded writer queue, queries, caches and maintenance. MCP Calls and Traffic share the MCP database; all Remote targets share the Remote database. Constructing a disabled module opens or migrates no history. Stopping or failing one owner leaves unrelated owners serving. History files survive stop and restart.
>
> The normal managed-log budget is 500,000,000 bytes across enrolled MCP and Remote history, with declared owner shares. Usage includes allocated database files, sidecars and managed artifacts. An owner reclaims only its own history. Protected Remote metadata may exceed the normal budget; the reported reason distinguishes this sustained exception from temporary maintenance and retained migration backups.
>
> Queries use typed filters and literal retained-text search. Safe fields use derived indexes; sensitive fields are authenticated and scanned only after authorization. Bounded incomplete scans return progress and continuation, including zero-hit batches. Stable record cursors and plaintext output-byte cursors are separate. A disabled owner is skipped without reopening its database.

Land ownership/lifecycle in D1, enrolled budget in D4, and query behaviour in D5, not all at once before consumers exist.

### SPEC §core.home / §mcp.logs

> MCP history occupies logs/mcp.db and Remote history logs/remote.db under the resolved Swiss home; WAL/SHM sidecars are allowed. Module resources are rows, not separate databases. Owner records, body state and derived index changes are transactional. Schema and payload versions are explicit. Verified, resumable import preserves retained legacy identities and bytes before cutover. Unknown newer versions never cause authoritative records to be dropped or replaced by an empty store.
>
> Sensitive chunks retain the frozen sealed envelope and contain authenticated inner row identity. Plaintext sensitive bodies, previews and terms do not enter ordinary FTS or durable scan state. Jobs JSONL/facts, daemon diagnostic rotation, terminal asciicast and the separate Memory store remain outside this conversion.

Land each module path/format paragraph only with D2 or D3. Keep unrelated frozen state names/envelope literals intact. Correct stale fixture-name citations to the actual existing sealed fixture without renaming the fixture or changing its bytes.

### SPEC §host.lifecycle / §host.actions

> Owner start/open/import work is bounded; factory construction performs no storage I/O. Stop withdraws new work, resolves accepted module leases and acknowledged writes, then releases owner connections and tasks within its deadline. A timeout reports failed/stopping history and unresolved capture; it never claims successful flush or closed storage. Remote stop targets accepted remote actions across submitting owners, not unrelated coordinator work. A successfully recorded terminal view follows its history completion acknowledgment.

Replace boot/teardown text for the consumers actually migrated; remove only their global restore/sweep/flush coupling. Keep Jobs maintenance independent.

### SPEC §mcp.calls / §mcp.traffic / §mcp.admin / §mcp.panel

> Calls and Traffic use the MCP owner's database. Calls retain source sequence, rename, preview/body availability and clear compatibility. Traffic lists query retained archive records rather than a 500-entry memory ring. List responses omit full bodies. Literal full-body search covers every retained permitted field before cursor paging and reports incomplete or unavailable coverage. Capture/write failure is observable without failing a completed tool call. The panel keeps a stable older-page reading position and visibly resumes bounded scans.

State the deliberate full-retained-body improvement: current Calls actually searches preview text despite contrary SPEC wording. Do not claim old clipped text becomes complete through import. Keep legacy paging/body adapters documented separately from the new cursor API.

### SPEC §remote.history / Remote lifecycle / §remote.panel

> All target histories use the Remote owner's database. Retained output byte cursors, capture bounds, env-key-only metadata and stdin exclusion remain in force. The coordinator's live output is separate from history storage. Literal search includes retained permitted output and authorized sealed content without indexing sensitive plaintext. Index/body eviction is transactional; the seven-day metadata guarantee outranks the normal soft log budget. Accepted related runs finish/cancel and acknowledge recording before successful module close; unrelated run owners remain live.

### Supporting sections and decision log

- Amend SPEC §arch.crates/deps for shared host machinery, one corrected engine, minimal features and measured codec/worker cost.
- Add a narrow SPEC §testing.rules exception for deterministic embedded SQLite/FTS scratch fixtures in workspace tests; external DB adapters keep gate-2 rules.
- Update SPEC §security, release hygiene/ignore rules and Jobs sweep wording where affected, without claiming Jobs migrated.
- Supersede ADR-005's Traffic ring rationale only when D2 actually removes ring authority. Allocate the successor number from the integrated SPEC; Memory ADR-031/032 stay intact.
- Record the hard-to-reverse module-owned SQLite/index/privacy choice as an ADR only when implemented; do not mark an unimplemented decision Accepted in current SPEC.
- Append a dated SPEC §product.memory record only from fresh later measurements. No invented benchmark, build-size or RSS numbers.

## 12. Acceptance tests: write red first, then implementation

All names/files below are planned, not created or executed. Each new test cites its delivered SPEC anchor in an English comment. Existing inline test owners may keep tests inline; proposed host test paths are new files. Use generated temporary homes/keys, deterministic clocks/barriers, tiny injectable budgets and fault injection, not production state, real sleeps or 500 MB fixtures.

| ID / proposed owner | Named test(s) | Failure before; success after | SPEC / delivery |
|---|---|---|---|
| E1 — host history engine tests | engine_rejects_unpatched_wal_before_import; engine_probes_contentless_delete_and_trigram | Current linked source lacks the fixed-version gate; shipping probe rejects affected engine before WAL/import and proves needed operations. | arch.deps, formats.logs / D0 |
| E2 — host schema tests | schema_step_and_version_commit_together; unknown_schema_leaves_store_untouched | An injected DDL crash or future version cannot leave a false version stamp or empty replacement. | formats.logs / D1 |
| L1 — tests/plugin_host.rs | disabled_mcp_and_remote_never_open_or_migrate_log_storage | Current real constructors can restore/sweep before the enable gate; injected production composition opens zero history paths while disabled. | host.lifecycle / D1-D3 |
| L2 — host log_store tests | closing_one_module_keeps_other_module_writes_and_reads_live; failed_open_rolls_back_only_its_module | Closing/faulting one real scratch store leaves the other's queue, connection and reads live. | host.history / D1 |
| L3 — host log_store tests | stale_generation_handles_cannot_write_after_close; flush_barrier_includes_unpolled_accepted_writes | Old facades/queued writes cannot reopen or mutate a new generation; barriers include pre-poll admission. | host.history / D1 |
| L4 — host/plugin tests | stop_deadline_failure_never_claims_disabled_or_closed; disabled_storage_queries_never_open_or_reenable | Timeout cannot be labelled successful release; reads/search/stats never wake a disabled owner. | host.lifecycle, host.history / D1-D5 |
| L5 — tests/plugin_host.rs | mcp_calls_and_traffic_share_one_module_db; remote_targets_share_one_module_db; adding_resources_does_not_add_pools_or_databases | Resource creation does not add database files/connections/quotas. | host.history, formats.logs / D2-D3 |
| L6 — tests/plugin_host.rs | disabling_mcp_keeps_remote_and_jobs_logging_live; disabling_remote_keeps_mcp_calls_traffic_and_jobs_logging_live | Real built-in factories, not only FakeFactory, prove reciprocal stop isolation and uncoupled Jobs sweep. | host.lifecycle / D2-D3 |
| R1 — host services/runs.rs | terminal_view_waits_for_history_finish_ack; history_ack_failure_is_visible_without_failing_execution | Enqueue-and-return cannot publish recorded completion; bounded persistence error is explicit. | host.actions / D1 |
| R2 — crates/swiss-plugins/src/remote.rs | remote_stop_cancels_accepted_actions_across_all_owners_and_acks_before_close; remote_stop_keeps_unrelated_runs_alive | Manual/MCP/Jobs remote actions drain by identity; unrelated process/Jobs runs survive. | remote.history / D3 |
| R3 — crates/swiss-plugins/src/remote.rs | remote_restart_rejects_old_history_generation_writes; remote_stop_timeout_reports_unclosed_resources | Retained tee/sink Arcs cannot outlive ownership invisibly or write into restart. | host.lifecycle, remote.history / D3 |
| M1 — MCP/Remote importer tests | import_every_legacy_variant_preserves_bytes_and_option_absence; import_resume_after_each_commit_is_idempotent | Replies, IDs, aliases, clients, flags and output survive; repeated interrupted batches do not duplicate. | formats.logs / D2-D3 |
| M2 — importer tests | import_source_mutation_blocks_cutover; import_missing_key_or_bad_tag_retains_source; import_crash_after_active_never_reimports_legacy | Concurrent legacy mutation/auth failure preserves originals; ACTIVE state prevents clear resurrection. | formats.logs / D2-D3 |
| M3 — importer/schema tests | torn_tail_is_accounted_without_losing_complete_records; cleanup_never_deletes_unverified_source | Corrupt/torn data is explicit; source retirement follows verified durable cutover only. | formats.logs / D2-D3 |
| I1 — module stores + host allocator | internal_identity_survives_clear_and_retention; legacy_clear_generation_preserves_wire_semantics; two_process_run_allocator_never_reuses_ids | Clearing logs never resets internal cursors or host allocator; legacy reset is distinct; processes cannot allocate identical host run IDs. | host.actions, formats.logs / D1-D3 |
| C1 — crates/swiss-mcp/src/calls.rs + tests/adminapi.rs | call_migration_preserves_seq_preview_bodygone_and_rename; a_recorded_call_is_visible_to_the_very_next_read | Existing optional fields/detail/read-after-record survive; missing/evicted body is not fake whole content. | mcp.calls / D2 |
| C2 — crates/swiss-mcp/src/traffic.rs + tests/adminapi.rs | traffic_pages_are_metadata_only_and_clients_survive_clear; retained_traffic_beyond_live_ring_is_queryable | Lists avoid payload copies; archive rather than 500-row cache governs query and detail. | mcp.traffic / D2 |
| B1 — body/history tests | migrated_output_cursors_reassemble_utf8_byte_exactly; byte_cursor_is_not_record_cursor; gaps_never_join_head_to_tail | Existing byte reads/UTF8 edge cases survive compression/encryption; gaps remain visible. | remote.history, formats.logs / D3-D5 |
| S1 — crypto/body tests | envelope_chunk_swap_or_reorder_is_rejected; frozen_envelope_fixtures_still_open | Valid envelope moved to a wrong row/offset fails inner binding; old frozen fixtures remain readable. | formats.sealed, formats.logs / D1-D3 |
| S2 — security/API tests | sensitivity_redacts_params_previews_errors_and_tails; sensitive_terms_never_enter_db_wal_fts_cursor_or_diagnostics | Unique secret markers are absent from all new durable derivatives; inventoried old plaintext exposure is separately disclosed, not claimed absent. | formats.logs, host.session / D1-D5 |
| S3 — tests/adminapi.rs | encrypted_scan_resume_reauthorizes_before_decrypt; denied_query_leaks_no_hit_ids_or_counts | Revoked/expired/forged/cross-owner cursors cannot decrypt or leak search results. | host.session, host.history / D5 |
| Q1 — host query tests | literal_query_preserves_ascii_fold_nonascii_quotes_paths_and_short_words; fts_candidate_verification_removes_false_hits | Literal semantics are not FTS grammar/rank; q<3 finds valid hits through bounded scan. | host.history, mcp.calls / D5 |
| Q2 — host query tests | fts_carry_finds_every_chunk_boundary_once; search_never_joins_fields_or_gaps | Cross-boundary matches are complete/deduplicated; unrelated fields/gaps do not create hits. | host.history / D5 |
| Q3 — host query tests | encrypted_zero_hit_batch_advances_with_continuation; resume_reconstructs_carry_without_persisting_it | No-hit budget exhaustion is incomplete, makes progress and preserves crossing matches. | host.history / D5 |
| Q4 — host query tests | new_rows_do_not_shift_snapshot_pages; out_of_order_run_finishes_page_completely; cleared_generation_expires_cursor | New writes/out-of-order finish do not duplicate/skip old pages; clear is explicit expiration. | host.history, remote.history / D5 |
| Q5 — aggregate query tests | mixed_index_and_scan_union_does_not_drop_ciphertext_only_hits; merge_waits_for_other_stream_frontiers; disabled_module_is_reported_skipped | A metadata prefilter/intersection or premature merge cannot suppress encrypted hits or claim complete empty results. | host.history / D5 |
| U1 — quota tests | quotas_are_per_module_not_resource; mcp_and_remote_share_one_declared_normal_budget; one_owner_reclaims_only_its_rows | New MCPs/targets do not multiply budget; A retention/clear never mutates B. | host.history / D4 |
| U2 — quota tests | physical_usage_counts_sidecars_backups_and_inflight_work; delete_without_checkpoint_is_not_reclaimed_credit | Logical row/body counts cannot masquerade as physical capacity. | host.history / D4 |
| U3 — quota tests | protected_metadata_overrun_is_honest; busy_checkpoint_does_not_hide_wal_growth; maintenance_crash_reconciles_usage | Preserve audit window while exposing sustained excess; paused physical cleanup remains counted. | remote.history, host.history / D4 |
| U4 — crash/concurrency tests | two_process_owner_reservations_are_not_double_spent; disk_full_import_append_and_cleanup_preserve_authority | Concurrent writes and injected full disk do not duplicate admission, erase verified data or cross owner boundaries. | formats.logs, host.history / D4 |
| P1 — panel existing search/pagination test files | incomplete_empty_scan_does_not_show_no_matches; continuation_and_filter_change_reset_correct_cursor; older_page_is_not_live_replaced | UI shows honest progress, filters invalidate old tokens, and reading position remains stable. | mcp.panel, remote.panel / D6 |
| P2 — panel traffic/remote view tests | disabled_module_does_not_auto_query; body_eviction_and_history_failure_are_visible; byte_output_keeps_expanded_view | Lifecycle errors and gaps are visible without waking storage or replacing live expansions. | host.lifecycle, mcp.panel, remote.panel / D6 |

Preserve and port the existing frozen-envelope, Calls preview/pruned-body/rename/read-after-record, Traffic restart/clear/redaction, Remote sealed-content/audit-window and run UTF-8 cursor tests. Existing presence is not evidence they passed during this planning session.

## 13. Sequential delivery: one green implementation commit per item

Write each item's enforcing tests first and witness the intended red condition. Land code, tests and only that item's now-true SPEC slice together. Do not commit a separate red-only test series or a future-SPEC-only amendment. Later items depend on the listed earlier items.

| Item | Depends | Implementation and SPEC slice |
|---|---|---|
| D0 — Engine safety / fixture seams | none | Align fixed shipping SQLite/native dependency and runtime capabilities; inject scratch roots BEFORE production constructors. Add arch.deps / embedded-fixture testing exception and observed engine evidence. No log-path or query behaviour claim. |
| D1 — Host owner lifecycle / ack / codecs | D0 | Factory, independent generation-bound queues/pools, versioned schema/body primitives, strict crypto binding and bounded close; async history ack with the current legacy Remote sink adapted; coordinated host run-ID allocation. Add host.history mechanics, lifecycle/ack/format primitives only. |
| D2 — MCP SQLite authority | D1 | Import Calls+Traffic, attach cheap facades in enabled scope, independent writer/sweep/flush, archive and compatibility routes. Deliver only MCP formats.home/logs, mcp.calls/traffic and relevant boot/teardown clauses; Jobs stays unchanged. Supersede Traffic ring ADR only here. |
| D3 — Remote SQLite authority | D1, integrated after D2 | Inject opener before Remote construction; import output/content, accepted-run lease drain, async final commit, byte cursor/retention adapters. Deliver Remote file/history/lifecycle clauses; no global budget enforcement claim yet. |
| D4 — Physical quotas / maintenance | D2, D3 | Shared policy/accounting with independent owner shares, actual sidecar/reservation/backup measurement, bounded checkpoint/vacuum/FTS cleanup and explicit audit overrun. Deliver global host.history budget and owning retention/usage clauses. |
| D5 — Complete query / body APIs | D2, D3, D4 | Literal trigram candidates + exact verification, short/sensitive scans, authenticated resume/highwaters/merge, new protected history APIs and legacy adapters. Deliver host.history query/security and module search clauses only now. |
| D6 — Existing panel/CLI consumers | D5 | Calls/Traffic/Remote cursor/search/progress/detail UI; CLI/export compatibility; TS source and committed emit together, translations and vitest. Deliver panel/wire clauses; no global log page. |
| D7 — Verification receipts / gaps / ADR | D0-D6 | Reopen/crash/disable isolation, real browser and memory/size evidence; close spec/code gaps. Allocate final integration ADR and append only observed memory records. No production deploy. |

MCP and Remote implementations can be prepared independently after D1, but integrate in this order and do not merge claims about both consumers before both exist. Do not make MCP or Remote depend on swiss-memory. Reconcile with main's pending Memory/ADR work explicitly, not by overwriting uncommitted files.

## 14. Gates and live evidence

Future implementation commands, **not executed for this document**:

```text
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo tree --locked -d -e normal,build
cargo tree --locked --offline --workspace --depth 0
cargo build --release --locked
npm run check          (in crates/swiss-panel/panel when panel changes)
```

Do not run rustfmt/cargo fmt. Manifest changes include the regenerated lockfile, dependency/notice review and shipping feature probes. External DB adapter/browser or swiss-it changes require the documented gate-2 integration run; embedded scratch SQLite tests are a narrow separate gate-1 exception, not permission to skip external gates. Re-evaluate gate-2 triggers if capture/adapter paths change.

Tests initialize deterministic scratch roots/opener before any constructor. The current Remote new(...).with_targets_path(scratch) fixture is unsafe because new already opens real history: fix the injection seam before executing it. Never read real operator logs/secrets as fixtures.

Live verification uses the repository's isolated **19998** discipline with fresh controlled home/keys and sanitized fixtures: no copied enabled forward rules, jobs or OAuth grants that can affect production. Stop its port owner before linking into an executable it serves on Windows. The user's review instance on 19997 and production on 19999 are not iteration targets for this work. Production is not stopped, restarted or deployed; only its sanctioned read-only health proof is permitted.

Real-browser walk: navigate freshly to each existing log view; use real pointer events for search, filters, next/previous/resume, detail, clear confirmation and disable/re-enable. Verify partial zero-hit, disabled, empty, evicted and failed-storage states visibly. Walk English and Chinese. Use the UI library, TS-source-only edits, committed emit, asset freshness and fresh served build hash. An external flow without credentials is reported NOT VERIFIED, not checked off.

Record later release size, actual linked engine version/source ID, active worker count, empty/loaded/post-ingest/reopened idle working set and private memory, plus disk including WAL/indexes/maintenance and query/scan/deep-cursor latency. Module disable should release its connection/tasks while the other continues; measure, do not infer from configured cache capacity. Append dated memory receipts only with the slice measured.

Definition of implementation completion: named acceptance matrix implemented/passing, per-commit gates, restored/cleared/migrated history correct, reciprocal disable and failure isolation observed, full-text and bounded encrypted scan semantics honest, engine fixed, SPEC/ADR amendments match the shipped tree, sensitive data absent from new plaintext derivatives, and live evidence gathered without touching production. A successful SQL insert or green unit suite alone is insufficient.

## 15. Known source facts and handover cautions

Read-only baseline observations as of 2026-10-08, not a general code-review verdict. The line ranges are that baseline's and have moved since; SPEC was then one file, now split into the chapters under `docs/spec/`:

- [Composition](../../src/server.rs) (lines 110-116) builds Calls early; Traffic restore (lines 245-250) precedes the plugin host. Do not add eager SQLite I/O there.
- [MCP stop](../../src/builtin.rs) (lines 175-222) does not own present log queues; the combined sweep in [server.rs](../../src/server.rs) (lines 410-425) is global. Lifecycle refactoring is necessary, not optional.
- [Remote construction](../../crates/swiss-plugins/src/remote.rs) (lines 52-104) opens history before enablement; [history open](../../crates/swiss-remote/src/history.rs) (lines 250-279) can mutate old storage.
- [Coordinator sink leases](../../crates/swiss-host/src/services/runs.rs) (lines 553-580) outlive withdrawal. The synchronous finish contract in the same file (lines 79-99) cannot be replaced by fire-and-forget SQLx.
- [Actual Calls query](../../crates/swiss-mcp/src/calls.rs) (lines 818-850) reads preview text, not body files, despite the SPEC wording of the time (single-file lines 1372-1375, now [§mcp](../spec/0005-mcp.md)). Correct the contradiction in the delivering search commit.
- The remote audit policy (single-file lines 3107-3133, now [§remote.history](../spec/0010-remote.md)) outranks byte/count caps. A finite hard quota and unlimited protected captures cannot both be promised.
- [Main Memory dependencies](../../crates/swiss-memory/Cargo.toml) (lines 12-17) and [pool options](../../crates/swiss-memory/src/store.rs) (lines 299-310) are reference evidence, not a template for log schema/lifecycle/FTS.
- The main locked binding in [Cargo.lock](../../Cargo.lock) (lines 2260-2268) and the cached native header identify the source risk. Query the final linked engine later; sqlx 0.9 is not the SQLite engine version.
- [Frozen envelope](../../crates/swiss-core/src/secure/envelope.rs) and [sealed fixture](../../tests/fixtures/sealed-node.json) remain unchanged. No whole-file re-encryption or plaintext-fallback migration.

Official supporting references: [WAL durability/checkpoint/safety](https://sqlite.org/wal.html), [FTS5/trigram/contentless-delete](https://sqlite.org/fts5.html), [incremental vacuum](https://sqlite.org/pragma.html#pragma_auto_vacuum).

## 16. Requirement receipt for eventual landing commit messages

Record date: 2026-10-08, Asia/Singapore. These are commit-message inputs, not attributions/dates to copy into normative SPEC prose. Each eventual amendment commit quotes the applicable original request and subsequent confirmation; the implementation brief does not replace that receipt.

Original requirement:

> 看着没什么问题，现在有个比较复杂的任务交给你，原来日志存储类的，都是 jsonl 或者其他的明文存储的，现在呢我想规整化，Rust 中，有没有那种，就是介于 SQLite 那种一大坨的文件内容，但是呢，又比 jsonl 更加紧凑，且更容易处理，搜索的方式？我打个比方，我MCP 有 log，traffic 有 log，Remote Runs 也有日志，我想搞个统一的存储机制，既能高效检索，又能高效翻页，还能存储一定量的内容。这种有没有比较合适的方案？除了 SQLite

Search/capacity follow-up:

> 1. 都要， 2. 500MB 以内都可以接受。 3. 不是单文件，而是每个内容一个单文件，例如 remote runs 下面有个 test 服务器，这个单独一个文件可以。 我额外补充一下，我这样的工具类的应用，怎么存储日志最合适呢？

SQLite correction:

> 不是明确排除 SQLite，我这里其他的地方也用到了 SQLite，例如 Memory 那个，就是主仓库代码那里，用到了 SQLite，主要是我怕有问题

The later module correction supersedes the earlier resource-file interpretation:

> 1. 不是每个 MCP 都有 db文件，而是每个模块，例如 MCP 模块，remotes 模块，都有 db 文件，互不干扰。 2. 这样做的主要目的是为了例如关闭 A模块，毫不相关的 B 模块不受影响。

Approval and document request:

> 没问题，开始 /swiss-spec  写新的计划文件吧

Final instruction ending further interview:

> 算了，总结吧，写个 spec，我让其他模型执行
