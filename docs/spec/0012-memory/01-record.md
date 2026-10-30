# The record

Part 01 of [§memory](README.md), chapter 0012 of the [swiss specification](../../SPEC.md).

## §memory.model — Notes, lifecycle, revisions

A note is `{ id (m-…), scope (p-… | global), kind, title (1..160, the claim itself), content
(≤ 8 KiB: why, then how to apply), status, origin (user | agent, set by the entry point), actor
(cli | panel | mcp:<client>), producer, transport, confirmed, anchors (≤ 20 repo-relative
paths/globs with a content baseline), refs (≤ 5), keywords (≤ 16), domain?, supersededBy?,
revision, createdMs, updatedMs, verifiedMs? }` with `kind`
one of `pitfall | constraint | decision | preference | knowledge`. `domain` is where the note is
filed (§memory.domains) - organisation, not content: it is outside the revision snapshot and
moving a note never bumps its revision. Over-limit or malformed input
is a field-precise `memory_invalid`; nothing is truncated on write. **Status** is `active |
needs_review` ("live") or `superseded | invalidated` (history). Agent writes go live at once as
`origin=agent, confirmed=false` — no candidate gate; the brief labels them and ranks confirmed
notes first. CLI notes also start unconfirmed (an agent with a shell can run the CLI); a note
created in the panel starts confirmed. Both are workflow signals, not a security boundary.

**Keywords** are the terms a reader would search for that the title does not hold - the
business term, a synonym, the main name - written in English and in the team's language side
by side (`refund`, `退款`), so a query in either language finds the note with no embedding
model (§memory.search). Notes and domains carry them alike: each trimmed with its inner
whitespace folded, one line, at most 40 characters, a repeat (case aside) kept once, at most 16
- over-limit input is a field-precise `memory_invalid`, never cut - and secrets-checked like the
title. They are content: in the revision snapshot, exported, checked again on import
(`knowledge_transfer_round_trip`). A sync proposal's or a map answer's keywords are taken
leniently instead - an over-long, repeated or secret-shaped one dropped, the rest kept.

**Provenance (ADR-032)** is three facts, never one flag. The **producer** is who wrote the note -
`human | agent | bootstrap | import | sync | unknown` - and the **transport** how it arrived -
`cli | mcp | panel | internal`; both are set when the note is created and an edit never changes
them. The panel writes `human/panel`, MCP `agent/mcp`, the sync `sync/internal`; the CLI writes
`unknown/cli`, since a shell cannot tell a person from an agent, unless `--producer
human|agent|bootstrap|import` declares one (a declaration, not an authentication). `confirmed`
says a person accepted the note; **`sourceVerified`** says a checkable source backs its current
text: some evidence with relation `verified_by` and source `code | test | commit | doc` was
attached to the text the note has now. Neither implies the other. **Evidence** is `{ id (e-…),
revision, sourceType (human | session | code | commit | test | doc | bootstrap | import), relation
(derived_from | supports | verified_by), path?, symbol?, commitSha?, sessionId?, runId?,
sourceHash?, excerptHash?, actor, createdMs, current }`, bound to the revision it was attached at
and to the note's content hash then: an edit to the title or content leaves it behind -
kept, shown as attached to earlier text, no longer verifying - while a mark, a confirm or a
restore keeps it current. A source names a file and a symbol, a commit, a session or a run; a
line number is never an identity and is refused (`src/a.rs:12`, `#L12`, a `line` field). A
cited file under the project root gets its content hash stamped; an excerpt is kept only as its
`sha256:` hash. At most 10 pieces a call and 50 a note
(`knowledge_provenance_producer_transport_and_confirmation_are_separate`,
`knowledge_evidence_binds_to_the_text_it_checked`, `knowledge_evidence_is_checked_at_the_door`).
Transitions: `mark verified|outdated|wrong` (anyone; a reason unless verified;
verified refreshes the anchor baselines), `save … replaces=<id>` (new note + `supersedes` link +
old status + both revisions in **one** transaction), `supersede` (a recheck's merge, §memory.init
The recheck: a live note superseded by another live one - the status, `supersededBy`, the
revision and the `supersedes` link in one transaction, a reason required), `confirm` and `restore` (CLI/panel only),
`purge ID --confirm ID` (CLI/panel only, the one physical delete). `update` is compare-and-swap on
`expectedRevision` (`memory_revision_conflict` carries `current`); feedback and links never
need it. Revisions are append-only with a snapshot after each change
(`knowledge_capture_update_is_compare_and_swap`, `knowledge_capture_mark_confirm_restore_purge`,
`knowledge_capture_replaces_supersedes_in_one_step`).

## §memory.scope — Projects, roots, resolution

A project is `{ id, name }` plus zero or more roots (identities `git:<common dir>`,
`path:<dir>`, `marker:<dir>`, each owned by one project). Notes belong to the project's id,
never to a path: a directory is only how a checkout finds its project, so it can change -
`rebind` swaps one directory for another in one transaction, and a moved or renamed checkout
keeps its notes - while a directory belongs to at most one project whatever its identity's
kind (`memory_root_bound`), so an exact binding never takes another project's repository
(`knowledge_projects_a_directory_is_one_projects_and_can_move`). Resolution walks up from the
canonicalized directory and the nearest boundary wins; at one directory `.swiss/memory.json`
beats an exact `path:` binding beats Git. Git identity is the canonical **common dir**, so
linked worktrees share a project and independent clones do not; no git process is spawned.
A linked worktree's root is the **main checkout** (the directory holding a non-bare `.git`
common dir): the first write from an agent's worktree names the project after the repository
and registers the checkout that outlives the branch, so the project - its name, its root, the
sync, the code index and the panel's preview - is the main checkout's. A call's own code reads
work in its **checkout** instead: the working tree its cwd is in - the repository's top from a
subdirectory, a linked worktree itself - where a save hashes and checks its anchors, a read
judges their freshness, a brief says its root and a question reads the code, so an agent's
note on a file only its branch has is hashed there, never held as missing from the main
checkout. A `.swiss/memory.json` committed into the repository is checked out into every
worktree too: when the main checkout's own marker names the same project, the main checkout
is the root, as Git's would be; a marker only the worktree's branch carries keeps the worktree.
A bare repository has no main checkout, so its worktree stays the root, and a submodule (no
`commondir`) is its own (`knowledge_scope_a_worktree_registers_the_main_checkout`,
`a_worktrees_committed_marker_is_the_main_checkouts`). A Git root row heals on the
repository's next write: one naming a working tree of the repository whose root is now the
main checkout (written by a worktree before roots followed it) is pointed at the main
checkout, and one naming a directory that is gone (a bare repository's worktree, removed)
follows the worktree writing now; another standing worktree of a bare repository, or a
directory another project holds, is left alone
(`knowledge_scope_a_worktree_root_row_heals_to_the_main_checkout`,
`knowledge_scope_a_bare_repositorys_dead_worktree_root_heals`).
`GIT_DIR` is honoured by the CLI only, for its own cwd, and passed explicitly. A WSL
`/mnt/<drive>/…` cwd maps to the Windows path; the Swiss home is never a project. Reads never
register; the first write from an unregistered Git root registers it, a plain directory needs
`init --here` (writes nothing into it) or `init --portable` (writes `.swiss/memory.json`).
Binding an identity another project owns answers `memory_root_bound`; a write from a checkout
whose `origin` matches a known project under another identity answers
`memory_project_ambiguous` with `possibleMatch` instead of fragmenting the project. `global` is
an explicit scope, never a fallback (`knowledge_scope_nested_dir_and_nested_repo`,
`knowledge_scope_linked_worktree_shares_the_project`, `knowledge_scope_reclone_is_not_merged_silently`,
`knowledge_scope_marker_and_exact_bindings`, `knowledge_scope_bad_cwd_is_refused`). `cwd` is a
selector, not authorization.

## §memory.capture — The write path

An exact duplicate (normalized title + content) returns the existing note with
`duplicate: true` and writes nothing, so retries are idempotent; a live note with the same
normalized title answers `memory_similar_exists` (`replaces` or `allowSimilar`); a save returns up
to 3 similar hints - notes sharing the saved title's subject, never a stray token: a
CJK title hints through the search's CJK tier over the live notes with the brief's coverage
floor (FTS cannot segment CJK, so its Latin tokens alone would match everything), a title
without CJK through the word tier. A save also answers `contradicts: [{id, title, anchor,
message}]` - up to 3 live notes, oldest first, sharing an anchor with it (equal paths, or a glob
covering the other's file) whose title asserts the opposite of its title (`opposed::opposed`:
each title's claim is its words without the polarity words - negations like `never`, `not`,
`don't`, `avoid`, `不要`, `禁止`, `不`, affirmers like `always`, `must`, `必须`, `要` - lightly
stemmed, CJK as bigrams; two titles are opposed when exactly one is negated and their claims
share at least half their words and at least two); a note the save links as contradicting is
left out, so is a duplicate's, and such a note is not repeated under the similar hints. It is a
hint, never a refusal: the message asks to mark the stale note outdated (`replaces` does it in
the save) or link the two with `contradicts`; the CLI prints it with both commands, MCP and the
API return it, and a contradiction worded apart goes unnoticed
(`knowledge_capture_an_opposite_title_on_the_same_file_is_a_contradiction_hint`,
`an_opposite_assertion_is_opposed_and_a_restatement_is_not`). A hand-written secrets check (PEM keys, AWS/GitHub/Slack tokens, JWTs, `sk-`
keys, `password=`-style assignments) refuses save, update, link notes and import with
`memory_sensitive_content`, naming the rule and never echoing the match
(`knowledge_capture_same_save_twice_is_one_record`, `knowledge_capture_same_title_is_similar_exists`,
`knowledge_capture_similar_hints_share_the_subject`, `knowledge_capture_secret_is_refused`). The save policy agents follow (save on a user correction
or stated rule, a non-obvious failure and its fix, a decision with a non-trivial why that is not
an ADR, a fact the code does not make obvious; never what code, `git log` or repo docs already
say, task progress, secrets or speculation) is verbatim in `memory_save`'s description and the
Skill. A rule the user gave is kept in their words: a preference, or a decision the user made,
opens its content with what they said, quoted letter for letter and dated (`> "Keep answers
short." - stated by the user, 2026-10-21`), then why it holds - an agent's paraphrase drifts
from the ask, and the date says which of two asks is newer; the note rules, the save policy, the
sync's rules (dated by the session) and the Skill all say so.

Formation (ADR-032) is the shape a note should have: one central claim, at most three facts
behind it, why - and why not the obvious alternative - and its evidence, each fact's file with
the line or two read there copied letter for letter; typically 300 to 900 characters; zero
notes is a valid answer. A **claim over a set** - a count (`exactly two`, `only 3`, `all 7`), a
closed set (`the only`, `the sole`, `no other`, `nothing else`, `nowhere else`, …), `every
other`/`all other`, or `most` (not `at most`, `most likely`) - is re-counted by the save
(§memory.code): it names the set in backticks, a glob or a directory (`scripts/gen-*.ts`,
`src/hooks/`), and what each member holds (`main`), or says what was read ("in `path`",
"observed in `path`", "in the inspected scope"); always and never alone are about one place's
behaviour and are written only of the code read. `formation::set_claims` reads them, a
sentence at a time outside code, at most 20 a note. `formation::NOTE_RULES` is the one text of
those rules: the sync prompt (§memory.sync) carries it, `swiss memory rules --bootstrap` prints
it for an external bootstrap pipeline, and the MCP save policy and the Skill restate it.
`formation::lint` is advisory, never a refusal: a new note's save answers `formation: [{rule,
message}]` (absent when clean, and for a duplicate) for `sections` (two or more headings),
`long` (over 1200 characters), `call_chain` (three or more hop lines), `code` (a fenced block
over 8 lines), `restates_code` (most of the note's characters appear verbatim in its anchored
files' text, which the code check reads anyway - what the code already says is not worth a
note; never for the global notes, which have no checkout) and `quantifier` (a claim over a
set that names no set or file and is not scoped; not for a preference, and left out when the
code check ran, which says it as `unscoped_quantifier`) and `user_words` (a preference whose
content holds no quote - a `>` quote line, or two words or four CJK characters in quotation
marks, so a quoted name is none - or no date, `2026-10-21` or `2026年10月`; asked of a note a
person did not type: the panel's save, the one that starts confirmed, is the user's own words
and is never asked to quote them, while the CLI's, which may be an agent with a shell, is;
`swiss memory lint` asks it of any draft); the CLI prints each as a `hint`, MCP
and the API return it, and the panel's toast names the rules
(`knowledge_capture_formation_lint_is_advisory`, `knowledge_capture_restated_code_is_advisory`,
`knowledge_capture_a_preference_quotes_the_user`,
`formation::tests::a_preference_without_the_user_s_dated_words_is_a_hint`,
`knowledge_cli_rules_and_formation_lint`,
`skill_install::tests::the_memory_skill_asks_for_the_user_s_words_and_reads_the_history`).

A save, an update and a verified mark may carry **evidence** (§memory.model): the CLI's
repeatable `--evidence [RELATION=]TYPE[:LOCATOR][#SYMBOL]` (`code:src/retry.rs#retry`,
`verified_by=test:tests/retry.rs#reuses_the_key`, `commit:abc1234`, `session:ID`,
`bootstrap:RUN`, `human`), the API's and MCP's `evidence: [{sourceType, relation?, path?,
symbol?, commitSha?, sessionId?, runId?, excerpt?}]` with unknown fields refused. Absent, the
relation is `derived_from` on a save or an update and `verified_by` on a verified mark;
`verified_by` takes only code, a test, a commit or a doc; an outdated or wrong mark carries its
why in `reason`, never evidence. Evidence on an update with nothing else needs no
`expectedRevision` and writes no revision, like filing. Every piece is checked before the
transaction opens - a malformed one refuses the whole call and writes nothing.

A new note in a checkout is also checked against the code (§memory.code): a name, a path, an
anchor or a quote the checkout does not hold, or a claim over a set that counts otherwise or
names nothing to count, is said in the answer's `code`, and holds a note no person wrote as
`needs_review`. An edit to the text is checked the same way, and releases a note the check
held once it passes: the model that wrote a note fixes it, no person confirms it.

## §memory.domains — The project map

A project's knowledge has a shape before it has details. A **domain** is one area of the
project - a business area (`funds`, `risk-engine`), a subsystem (`gateway`, `panel`) or a
cross-cutting layer (`platform`) - and the domains form a tree of at most five levels
(`risk-engine/broadcast`). The global scope has no domains: its notes hold across projects.

A domain is `{ id (d-…), scope (p-…), path, title (1..80), summary (≤ 200, the line the map
shows), body (≤ 48 KiB Markdown, the hub), owns (≤ 30 repo-relative paths/globs), keywords (≤ 16,
§memory.model), origin, actor, confirmed, revision, createdMs, updatedMs, verifiedMs }`. A path is
1..5 segments joined by `/`,
each `[a-z0-9][a-z0-9_-]{0,39}`, unique in its project; a sub-domain needs its parent; a project
holds at most 500 domains. Titles and summaries are in any language (`资金与支付`), paths are
not. Writes follow the note rules: `origin` and `confirmed` as for notes (an agent's or the CLI's
domain is unconfirmed until confirmed in the CLI or the panel; **confirmed whole**, the domain, every
domain under it and their unconfirmed active notes are confirmed in one transaction, each a
revision reading `confirmed with PATH`, while a note that needs review stays unconfirmed and is
counted as `held` - the code check or a reader doubted it, and it waits for a verdict), compare-and-swap on
`expectedRevision`, append-only revisions with a snapshot of every field above but the
counters, the secrets check on title, summary and body (`memory_sensitive_content`), and a path
already taken answers `memory_domain_exists`. `verifiedMs` is set by a create, by any change to
the body or `owns`, and by `verify` (anyone): it is the moment the hub was last true to the
code. **Move** is an update of `path`: the sub-domains' paths follow in the same transaction;
the new path is free, its parent exists, and it is not under the domain itself. **Delete** (CLI
and panel only, by typing the path) is refused while sub-domains remain; the domain's notes go
to its parent, or are left unfiled for a top-level one, in the same transaction
(`knowledge_domains_tree_rules`, `knowledge_domains_move_and_delete_keep_notes`).

**The hub** is the domain's working document: the page an agent reads before it changes the
area, deep enough to plan the change from it and open only the files it will edit. A leaf's hub
has these sections, in order: Responsibility (what the area does, then what it does not),
Entry Points (who calls in, by symbol), Core Flow (a Mermaid diagram), Call Chain (an indented
tree of `Type.method` hops that marks where the flow BRANCHES, RETRIES, LOCKS, CACHES or FAILS
SILENTLY), Business Rules, Key Symbols (a table of symbol, file and role), Data (tables, files,
config keys, events), Pitfalls, and Related (other domains by path). A parent's hub is an
overview with a table of its sub-domains. A hub names code (`Type.method`, paths, tables,
config keys) and never quotes line numbers, which rot with every edit; every file the domain
owns appears in it. A claim that needs its own anchors, freshness and review - a decision and
its reason, a pitfall and what it breaks - is also a note filed in the domain: the hub states
the rule where it acts, and the note keeps why. Init and sync write hubs to this shape; the
store does not enforce it.

**Owns** is the code the domain answers for. An entry owns the paths it matches as a glob and,
as a directory, everything under it. The deepest domain owning a path is that path's domain.

**Filing.** A note is filed in at most one domain, or unfiled. A save takes `domain` (a path;
an unknown one is `memory_not_found` naming the map's paths); without one, the note is filed by
its anchors: each anchor (a glob anchor by its literal prefix) goes to its deepest owner, the
note to the domain that took most of them, a tie to the tied domains' deepest common ancestor,
and no anchors or no owner leaves it unfiled. `file(ids ≤ 200, domain | none)` moves notes by
hand; `autofile` applies the anchor rule to every unfiled live note and answers how many it
filed. An explicit filing is never undone by `autofile` (`knowledge_domains_filing_by_anchors`).

**Coverage** is computed, never written: per domain its live, needs-review and unconfirmed
notes, its owned files (`git ls-files`, or the bounded walk outside Git) and its **drift** - the
commits since `verifiedMs` that touched an owned path, with the latest one; and for the project
its unfiled notes and the directories holding tracked files no domain owns (at most 20, by
file count, two levels deep). Drift and the uncovered directories need Git (§memory.preview's
runner, one `git log --name-only` over at most 500 commits); without it coverage keeps the
counts and says why as `issue` - `no_directory` (none reachable on this machine),
`not_a_repository`, `git_missing`, `refused` (another user owns it; git's safe.directory) or
`git_failed` - with git's own line in `reason`. The panel words the issue in the reader's
language and offers to change the directory for the first two; it shows `reason` only for
`git_failed` (`knowledge_api_domains_coverage`). The listing and the log are read side by side,
and coverage is computed on the blocking pool, never on the runtime thread. Each path's owner is
looked up once, for the files and the commits alike. An owns glob tests the path and its
directories in one pass, split once per read, and a path lacking the glob's literal pieces in
order is refused before that pass: a 6 000-file checkout under 150 owns entries and 500 commits
is computed in 30 ms (`glob::tests::a_match_may_end_at_a_directory_in_one_pass`,
`domain::tests::owners_made_ready_once_answer_as_the_map_does`).

## §memory.freshness — Freshness and the review inbox

Freshness is computed at read, never persisted: an anchored file whose hash changed or that
disappeared marks the note `anchor_changed`; a live `contradicts` pair marks both
`contradicted`; a `depends_on` target that is invalidated or superseded marks `dependency_broken`;
180 days without verification marks `old`. A note's read (brief, show, search) re-hashes at most
50 files, read-only, under the resolved root only; a file past that cap is unchecked and raises
nothing. A whole-project scan - the review inbox, its counts, a review run's reach - checks every
anchored file of the live notes, read off the async runtime (a file over 1 MiB by its size and
modified time, as every read), so a project with more anchored files than the cap sees every one
that changed (`knowledge_freshness_anchor_change_and_dependency`,
`fresh::tests::a_whole_scan_checks_past_a_reads_cap`). A hash once taken is remembered by the
file's path, size and modified time and reused while both stand, so a scan over a thousand
unchanged files reads their metadata, not their bytes; a file modified within the last two seconds
is read every time, since a second write inside the clock's grain could keep both; at most 16,384
files, forgotten with the kept scopes (§memory.store)
(`fresh::tests::a_settled_files_hash_is_reused_until_its_size_or_mtime_moves`,
`fresh::tests::a_file_written_just_now_is_read_every_time`). The review inbox
(`swiss memory review`, the panel's Review tab) lists live notes carrying any marker; every
unconfirmed note carries one, whether an agent or the CLI wrote it, so the inbox holds what the
review counts say. The panel reads the counts again on every pick of a project or a tab: notes
also arrive from outside it. A person may hand the entire inbox to the sync
agent: a review run (`swiss memory review --resolve`, §memory.init The review run) judges them
against the code, confirms what the code bears out and settles the contradictions, including human notes;
a project's `autoReview` (§memory.settings) does the same each day even when the notes' files
have not changed. A recheck, a full run's closing one too, settles on its own the
contradictions between the model's unconfirmed notes it judges or links (§memory.init The
recheck); what it leaves standing waits here.

## §memory.store — Storage and durability

The gateway owns one service per active Memory plugin lifetime. Disable withdraws the service
and closes both the notes and code-index pools, including when a caller still holds an old
service. Closing serializes with lazy initialization and prevents an old request from reopening
either database; closing an unused service creates no files. Re-enable constructs a fresh
service over the same persisted data. The offline CLI's independently owned service is unaffected.

One database at `<SWISS_HOME>/memory/store.sqlite3` (ADR-031): WAL, `foreign_keys=ON`, 5 s busy
timeout, one lazily opened connection per process, every mutation one `BEGIN IMMEDIATE`
transaction (record, revisions, anchors, refs, links, evidence, supersede stamp), schema version 11 under
`schema_migrations` (v3 adds `memory_vectors`, created in place on a v2 store; v4 adds
`memory_domains`, `memory_domain_revisions` and the nullable `memories.domain_id` - deleting a
domain re-files its notes in the service, the column's `ON DELETE SET NULL` is the backstop -
created in place on a v3 store; v5 adds sync's bookkeeping, `memory_sync_state`,
`memory_sync_commits`, `memory_sync_cursors` and `memory_sync_runs` (§memory.sync), created in
place on a v4 store; v6 adds `memory_evidence` - cascade-deleted with its note - and the
`memories.producer` and `memories.transport` columns, which the migration fills in one
transaction from what each row's origin and actor prove and nothing more: `panel` →
`human/panel`, `cli` → `unknown/cli`, `sync` → `sync/internal`, `mcp[:client]` → `agent/mcp`,
any other actor `unknown/cli` for a user and `agent/internal` for an agent; no row is lost and
`confirmed` is kept as it was (`a_v5_store_gains_provenance_from_its_actors`). A v5 build refuses
a v6 store, so the gateway and the CLI move together; v7 adds the `keywords` column to `memories`
and `memory_domains` - one keyword a line, empty for none - and a third `keywords` column to
`memory_fts`, the index rebuilt in place on a v6 store, its triggers re-created to follow it; v8
adds `memory_project_settings` (§memory.settings), created in place on a v7 store - a project's
own agent settings, machine bookkeeping like sync's rows: never exported, cascade-deleted with
the project; v9 adds the documents a project keeps (§memory.docs), created in place on a v8
store - `memory_docs` (one row a document, its card beside it, `UNIQUE(project_id, path)`, its
`domain_id` set null when the domain goes), `memory_doc_sections` (cascade-deleted with their
document, an `INTEGER PRIMARY KEY` that is the index's rowid) and `memory_doc_fts` (FTS5 over
each section's heading, text and its document's search words, maintained by the service, no
triggers) - machine data like the code index: never exported, deleted with the project; v10
adds `memory_docs.names_json`, the code names a card gave (§memory.docs The card), added in
place on a v9 store - null on a card written before, which the next docs run asks again; v11
adds `memory_misses` (§memory.evaluation Misses), created in place on a v10 store - the
retrievals recorded as failed, machine data like sync's rows: never exported, cascade-deleted
with the project). A
newer schema fails with `memory_unsupported_schema` and never wipes; a v1
development store holding no notes is rebuilt in place, one holding notes is refused with the
backup instruction. FTS5 is external-content, trigger-maintained and rebuildable. Backup is
`VACUUM INTO` a temporary file then a rename, never "checkpoint then copy", and never overwrites.
Export/import is JSONL `formatVersion 4` (a header line, then projects, domains with their
revisions parents first, records with their revisions, producer, transport, evidence and their
domain's path, links); a `formatVersion 3` file still imports, its provenance derived from its
actors as the migration derives it and anything else it claims about provenance ignored, and a
`formatVersion 2` file imports unfiled as well. Imported evidence obeys the rules attached
evidence does, belongs to the note on its line and to one of that note's revisions, and never
replaces a piece the store already holds. A domain whose path the project already has is
that domain (the strategy decides a differing id like a record's), never a second one. Import validates every line first and runs in one transaction (`--dry-run`
rolls it back), creates unknown projects without roots, never imports identities, and refuses
links whose ends are missing or cross scopes and records filed in a domain the file does not
carry (`knowledge_transfer_round_trip`).

A read keeps the scope it read while nothing writes. The review inbox, a listing, the map and a
domain, the brief, the graph and the association read a project's notes and links whole; the
service keeps them for the next read of the same scope (three scopes at most, the one read longest
ago going first) and reads them again once anything has committed to the store since. It asks
`PRAGMA data_version` on a connection of its own that reads nothing else, so this process's
writes, the offline CLI's and any other process's are seen alike
(`scopes::tests::a_scope_is_kept_until_a_commit_and_read_again_after`,
`scopes::tests::a_commit_from_another_connection_is_seen`). The version is asked before the read,
so a commit landing during it leaves the scope behind the store and the next read misses. Only a
read outside a transaction keeps a scope; a write reads its own. Ten minutes without such a read
release the scopes, that connection and the remembered file hashes (§memory.freshness)
(`scopes::tests::the_cache_goes_after_it_idles`), as closing the service does at once.

A transfer is a knowledge PR, read before it merges. The header says what the file is: beside
`formatVersion` and `exportedMs`, the `exporter` (the build), the `scope` (`all`, `project` or
`domain`), the `project` (`{id, name}`, not for `all`), the `domain` path of a domain export, the
`visibility` (`project`: that project's agents read it, `global`: every project's, `mixed`: an
export of everything), the `notes` it carries, history included, how many a person
`confirmed`, and for a domain export the `linksLeftOut`. The fields are additive - an older build
reads `formatVersion` alone, and a file without them still imports; a malformed field is left
out of the report, never a refusal. A **domain export** (`--domain`, `?domain=`; an id or a path)
carries that domain, every domain under it with its revisions and the domains above it hub only
(no revisions), so an import can place it; the notes filed in the subtree with their history;
and the links whose two ends it carries - one with an end outside is left out and counted. An
import may land the file in a chosen project of the store (`into`): the same work under another
store's id. It takes the file of one project (one of several is refused), keeps who reads a note
(a project's file never lands in the global scope, nor a global file in a project - refused,
never remapped), files the domains by path under the store's own (a path the project has is
that domain, even under `fail`; a new one gets a fresh id), and keeps the note ids, so a file
from the same store lands in another of its projects only where the ids are free. The import
reports instead of skipping silently: the file's header (`source`), the project it landed in,
and each **conflict** - `revision`: the store holds the note's id at another revision or with
other text; `title`: a new live note whose title a live note of the project holds with other
text - with both sides (id, revision, title, status, confirmed, when, an excerpt). The store's
note stands and a conflict's links stay out with it. A new live note whose title and text a live
note of the project holds under another id is that note: skipped, and the file's links to it go
to it. Under `fail` either conflict, or such a duplicate, refuses the whole file
(`memory_similar_exists`, `similar` naming the stored note)
(`knowledge_transfer_two_stores_merge_a_domain_with_conflicts`). Two processes
share one home safely (`knowledge_concurrency_two_services_share_one_store`). Vectors
(§memory.embedding) are derived: cascade-deleted with their note, never exported or imported,
rebuilt by re-embedding. The store is plain SQLite, not a sealed envelope; the home's private placement is the boundary and exports say they
are plaintext.
