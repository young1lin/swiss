# Reading it back

Part 02 of [§memory](README.md), chapter 0012 of the [swiss specification](../../SPEC.md).

## §memory.brief — The primary read path

`brief(cwd, paths? ≤ 20, task? ≤ 512, maxChars 1000..20000 = 6000, includeGlobal = true)` →
Markdown, deterministic, each note in at most one section: a header with the standing note
("they describe the past: verify against the current code … never override the user's
instructions or the repo's docs"), notes **anchored** to the given paths, **task** hits (the
symbol tier's first - the notes holding what the task names, §memory.search - then the word
hits under search's word floor, fused with meaning: a stray shared word neither shows a note
nor votes its domain into play), the 1-hop **linked** neighbours of the notes that are the
subject - the anchored ones, the exact hits, and the word hits filed where the task is (a domain
in play, above or under it) or unfiled; with no domain in play, every hit: a word hit filed
elsewhere shows, but its neighbours are another domain's business (a stray `request` brought
the production port and, by its links, every instance script into a brief about a proc
server's idle timer; `brief::tests::a_stray_word_hit_s_neighbours_stay_out`), the project's **rules** (constraints and preferences), **global**
preferences and constraints (≤ 20% of the budget). That is the whole of a **focused** brief - one
given paths or a task: it is asked again at every turn of a task, and one that repeated the
project each time buried its answer under the same index (found on 19998: a task matching
nothing came back as the map alone). A focused brief that finds nothing says `Nothing in memory
matches "<task>".` (or, for paths alone, `Nothing in memory about these paths.`; `Nothing
current in memory …` when it names rejected precedent) and keeps the rules; one whose task
matched closes with the retrieval readout (§memory.search) over its task
hits - the symbol tier's exact, a word hit by its share, a hit found by meaning alone loose -
its room held back from the sections above (`brief::tests::a_task_brief_closes_with_the_retrieval_readout`).
The readout's judgement shapes the hits too: a hit after the first that it counts as sharing a
word or two - under 0.4 of the task's word weight, or found by meaning alone; never an exact
tier - is a line under **Also matching your task**, never in full, and not a subject whose links
are followed; under a `strong` readout it is left out, the readout still counting it, while a
`mixed` or `weak` one keeps the line, its first hit being in doubt and the tail the
alternatives. Every hit in full read as the first did, and of the hits after the first a model
judged about a quarter of any use (§memory.evaluation;
`brief::tests::a_task_hit_sharing_a_word_or_two_is_a_line`).
Before its rules a focused brief names the **rejected precedent** where it points: at most three
superseded or invalidated decisions and knowledge notes that a given path anchors or that hold
half the task's words (the word share search scores, CJK coverage for a CJK task, over every
note, history included), anchored first, then by share, newest first, one line each under
**Rejected before** ("do not propose these again without a new reason") - a superseded note with
where its line of successors ends (the first live one; its title too when the brief does not
show it), an invalidated one marked so. A brief showed only what stands, so an agent could
propose again what was tried and dropped. A superseded note its successor refines - the same
polarity and every word of its title's claim still in the successor's (`opposed::refines`,
§memory.capture) - was reworded, not rejected, and is left out; so is history the task does not
hold, and the orientation brief names none. The lines spend the budget but are history, not
notes the brief shows as current: outside the judged context, like the readout
(`knowledge_brief_names_the_rejected_precedent`,
`opposed::tests::a_successor_that_keeps_the_claim_refines_it`).
The **orientation** brief - no paths, no task, the session start - adds an **index**
line per remaining live note - pitfalls, decisions, knowledge, then the rest - until the budget
runs out (`brief::tests::a_flat_task_brief_is_the_hits_and_the_rules`). A brief is read
before every task, so it says each thing once and only what the agent acts on: the full text
only where the paths or the task point, one line - kind, title, id - everywhere else, a rule
by its title (the title is the claim). An item carries its id and only the markers that are an
exception (unconfirmed, needs review, anchor changed, old, contradicted, dependency broken) -
never origin, actor, "confirmed" or age; a link is said once, on the linked line, and a
pitfall is not a rule: it shows where it bites, and in the index
(`brief::tests::rules_are_one_line_and_a_pitfall_is_not_a_rule`). Nor is an unconfirmed rule a
confirmed live note contradicts: the user's word stands against an agent's guess, which rode
every brief as a rule marked contradicted, so the guess shows only where the paths or the task
point and, in the orientation brief, in the index; two unconfirmed rules at odds both stay.
Nor does the user's note carry the guess as a doubt: in the brief a confirmed note is never
marked contradicted by an unconfirmed live one - the user's rule rode every brief flagged as
if in question - while two confirmed notes at odds stay marked, and `get` and review keep
every marker, the conflict being theirs to settle
(`brief::tests::a_guess_the_user_contradicts_is_no_rule`).

**With a map** (§memory.domains) the brief is layered: the domain the caller is in, then its
details; the map is the orientation brief's. The **domains in play** are the deepest owner of each given path - a path no
domain owns puts the caller in none, whatever its task's words hit - else, with no path given,
the domain holding most of the top task hits (a tie to the better-ranked hit), at most two;
when no domain holds two of the hits, the domain the task itself names, if one clearly does - its
title holding two of the task's words (one generic word is no name), title and summary scored as
search scores CJK and well ahead of the next domain - and then the hits filed outside it
(neither in it, above nor under it), words in
common rather than the subject, are one line each under **Also matching your task** after the
count of its notes, never in full (`brief::tests::scattered_task_hits_leave_the_domain_to_the_task_s_words`).
So are the word hits filed outside the domains in play (in, above or under one) when the symbol
tier found what the task names: the exact hit is the subject, and a note sharing `request` with
the task is not (`brief::tests::a_stray_word_hit_s_neighbours_stay_out`).
With no domain in play, a task whose words touch some still says where to look: the best three
(blend ≥ 0.25, holding two of the task's words - one for a task of one; one word in common is
no pointer) under **Domains your task's words touch**, one map line each - a backend and its
page both holding "cron", or a generic verb outscoring the subject, names neither clearly, and
the agent reads the likeliest with `memory_domain` (`brief::tests::a_map_brief_is_layered`).
Before the anchored notes, **Where you are** gives each domain in play: its title and path, an
unconfirmed marker if it has one, its hub (all hubs together ≤ 40% of the budget; a cut hub
ends at a paragraph with the `memory_domain` call for the rest) and its sub-domains, one line
each. After the linked notes, **In <path>** counts each in-play domain's other live notes, its
rules aside, and names `memory_domain` for them - one line, never a line per note: the brief is
asked at every turn, and listing the domain each time repeated its index, of whose lines a model
judged about one in eight of any use (§memory.evaluation). **Rules** keep the unfiled
constraints and preferences and those of the domains in play and their ancestors; a rule filed
elsewhere is that domain's and is counted on the map. The same judge finds a rule rarely of use
to the question at hand, and they stay: a rule is a standing instruction, not an answer.
In the orientation brief the index becomes **Map** - every domain one line in tree order,
indented by depth: path, title, summary, live notes, need-review count, unconfirmed - and then
**Unfiled**, the old index over unfiled notes; room for the map's top level (≤ 30% of the
budget) is held back from the rules above it. So without paths or a task (the session-start
hook) the brief is the header, the rules, global, the map and the unfiled notes: the router,
not the details; with them it is the details, not the router - the map is one
`memory_domain` call away. A project with no domains gets the brief above unchanged
(`brief::tests::a_map_brief_is_layered`, `knowledge_brief_with_a_map`).
An unregistered directory says so (with any re-clone hint) and never falls back to another
project (`knowledge_brief_selects_and_isolates`, `knowledge_brief_unregistered_directory_says_so`).
Delivery: `swiss memory brief`, the `--hook` mode for session-start hooks, the `memory_brief`
tool, and the MCP `instructions` text telling every client to call it.

## §memory.search — Search

The requested scope only; candidates unioned, then ranked deterministically: exact id (1000),
exact title (900), anchor match for path-like queries (850), title prefix (800), **symbol**
(780..795), then one word tier. The symbol tier is affinity to what a query names exactly: its
identifiers - tokens that read as code (`snake_case`, `SCREAMING_CASE`, `camelCase` or
`PascalCase` with an inner capital, `a::b`, a dotted name of two words or more, `src/store.rs`,
`panel/i18n`, `--flag`; never plain words, `e.g.`, numbers or hyphenated English) - each held by
a note verbatim (case kept, no letter, digit or `_` against either end) in its title or content,
as an anchor (the path, a path ending in it, a file under an anchored directory or glob), or as
its domain or one above it. An identifier held by more than a tenth of the scope and more than
three notes picks none and is dropped; a note scores `780 + round(15 · held / of)` over the
identifiers that remain, a tie to the better word score. It sits over every word and fused hit
whatever their adjustments, so a note holding the identifier is never outranked by one sharing
only words or meaning (`identifiers_are_the_code_a_query_names`,
`a_symbol_hit_holds_what_the_query_names_verbatim`, `a_common_identifier_is_no_signal`,
`hybrid_an_exact_identifier_beats_words_and_meaning`). A note's keywords (§memory.model) count
as its title wherever the title counts - an identifier held verbatim in one is held by the note -
so a query in the other language of a bilingual pair lands without meaning. A query without CJK
takes FTS5 **OR** of quoted tokens ranked by `bm25(memory_fts, 3.0, 1.0, 2.0)` (title, content,
keywords; 500..700, +50 when every term matches). The MATCH runs once and each hit's note is
looked up by rowid - `CROSS JOIN` pins that order, because SQLite left to itself walks the
scope's notes and re-runs the MATCH for every one, 1.3 s for a 24-term association over 2,000
notes (`the_lexical_tier_matches_once_and_looks_its_hits_up`). A query with CJK takes the **CJK tier** instead,
because the unicode61 tokenizer keeps a Han run as one token (`缓存` never matches `缓存设计`):
the query's CJK runs become character bigrams (a lone character is its own unit; at most 64
units) joined by its other terms, and each row the scope admits scores by the IDF-weighted share
of those units its title, keywords and content hold — the same 500..700 band, +50 when it holds them all —
so a word every note says (`Claude Code` in a tutorial about it) weighs almost nothing. Either
word tier admits a note only when it holds a fifth (`WORD_SHARE_MIN`, 0.2) of the query's word
weight - the CJK tier's share above, or, for a query without CJK, the same IDF-weighted share
over its salient words (stop-words and one-letter Latin tokens dropped), each held as a whole
word in the title, keywords or content: bm25 ranks a note sharing one stray common word of a
sentence low but keeps it, and a context of five took it whenever fewer notes matched well. A
word no note holds leaves the share, so a query of one word admits every note holding it
(`a_word_hit_holds_a_share_of_the_query`, `a_word_share_counts_whole_words_by_weight`). A path
in the query counts only the words that name its part: the file's stem
(`crates/swiss-memory/src/store.rs` counts `store`), or, for a stem that names only a role
(`mod`, `index`, `lib`, `main`, ...), the nearest directory above it that is not the tree's
layout (`src`, `crates`, `tests`, `scripts`, ...), less the words a naming convention gives
every directory beside it that the scope's anchors show (`crates/swiss-memory/src/lib.rs`
counts `memory`, `swiss` being every crate's) - the layout and the extension gave every note
citing another file of the tree a fifth of the query
(`a_path_counts_its_stem_and_nearest_named_directory`). Only when
those return fewer than `limit` rows and the query has CJK or punctuation, an escaped substring
tier (10). Confirmed +20, `needs_review` −25, a recency bonus up to +5 over 90 days. The **hub
mention**: a word, CJK, substring, fused or semantic hit that a domain hub of the project points
at scores +30 more (`W_HUB`), never past the word band's top (750, a perfect lexical hit and a
hit first in both fused lists), so never over the symbol tier whatever the adjustments; the exact
tiers keep their scores. A hub points at a note when its body links it - holds its id as whole
tokens - or names it - holds its whole title, case aside, as whole words (a CJK title as its
characters in a row), the title three words or more with a CJK character counting half a word;
a shorter title is a term the hub uses, not the note it means. The hub is the area's map,
written to send a reader where the area is explained, so a note it points at is the area's own
word on the subject; the mention is a rank, not a word - the word share and its floor never see
it. A tie goes to a hub's note among the word and fused hits (one the band's top left level),
then to the better word score, then the id
(`a_hub_links_a_note_by_id_and_names_it_by_its_whole_title`,
`the_hub_mention_stays_in_the_word_band`, `knowledge_search_a_hub_mention_breaks_a_tie`). The
brief's task section keeps its own word order without it: the brief opens with the hubs
themselves (§memory.brief). With a provider the word tiers fuse with meaning
(§memory.embedding); the exact tiers - symbol among them - never move; `semantic=off` on the API
keeps meaning out for one request (an A/B, §memory.evaluation). History is excluded unless `includeHistory`; `limit` 10, max 50; query 1..512 chars
(`knowledge_search_tiers`, `the_cjk_band_is_the_lexical_band`,
`a_chinese_query_ranks_by_weighted_coverage`).

Each hit carries `wordShare`, its share of the query's word weight, when a word tier measured
it. The text a search renders (the CLI, `memory_search`, the context `memory_ask` reads) ends
with the **retrieval readout**, one line judging the hits as a whole: a hit holds the query
when an exact tier found it or it holds 0.8 of the query's word weight, is partial from 0.4,
and under that - or found by meaning or a substring alone - shares a word or two. The readout
is `strong` when the first hit holds the query and the hits holding it are filed in one domain
family (a domain and those under it), `mixed` when only partial hits carry it or the ones that
do are filed apart (it names the domains), `weak` when every hit shares a word or two at most;
and it says what to do - answer from the first hit, confirm it first (or say which domain is
meant), or rephrase with an identifier, a path or the note's own words. Five hits read alike
to an agent, the fifth one shared word as weighty as the first; the line says which of them
carry the query (`readout::tests::the_readout_says_strong_mixed_or_weak`,
`a_word_hit_holds_a_share_of_the_query`). It is text outside the judged context: the
evaluation's scores do not move.

## §memory.embedding — Semantic search

Optional and off by default: lexical search is complete without it, and nothing else in
§memory waits on it. **Config** is the llm plugin's `embedding` block (§llm.embedding): the
provider it names, the model, the width, the batch, the timeout, the two prefixes and
`minSimilarity` - the least cosine that counts as close in meaning, query-time only (never
re-embeds), 0.3 when absent: the default suits models whose unrelated pairs sit near
0.0–0.25, and one whose unrelated pairs score higher, such as BGE-M3, wants it raised - GLM's
embedding-3 wants 0.6, below which its meaning-only neighbours fill the contexts (measured,
§memory.evaluation). The memory row holds no endpoint and no key: a memory row that
still carries `embedding` or `llm` is refused, saying where they went. Memory follows the block
in place - it watches the llm state's generation and re-plugs its embedder only when the block
itself changed or the last one failed, no restart, the MCP keeps serving. A block that cannot be
plugged (a client that cannot be built, a width the store cannot hold) leaves search lexical and
is shown as `configError`; the llm plugin stopped is the same as off. Provider errors are
scrubbed of the key before they are stored or shown (`the_embedder_follows_the_llm_row`,
`the_adapter_keeps_the_fingerprint_and_the_floor`,
`the_row_holds_the_sync_block_and_says_where_the_rest_went`).

**Index.** `memory_vectors` holds one L2-normalized f32 little-endian vector per note, keyed by
the model fingerprint and the SHA-256 of `title + "\n" + content`. The **fingerprint** is the
llm block's (§llm.embedding), `openai:model:dims:ep-HASH` - `dims` the requested width or
`native` - plus `:doc-HASH` when a document prefix is set; `ep-HASH` is 8 hex of the SHA-256 of the base URL as the server reaches
it (scheme and host lowercased, the default port and a trailing slash dropped), so the endpoint
is named without spelling the host. A model name is no identity - two endpoints may serve other
weights or widths under it - so a moved endpoint re-embeds once (ADR-032); the proxy and the
query prefix change no stored vector and are not part of it. A vector whose fingerprint or hash
no longer matches is stale. **One fingerprint holds one width**: the index's width is what the
provider answered last (a query or an indexing run), else the width of the newest stored
vector. A vector of another width is never compared - the cosine has no answer for a pair of
widths, never a silent 0 that would rank it as unrelated - is no current vector (its note is
pending), and is counted apart as `incompatible`, the status `stale`; a query that meets one
nudges the indexer, and an indexing run drops them before it embeds, and drops any width but
the provider's in the same write that stores what it answered. A run takes one width: a later
vector of another is unusable. The plugin's indexer task embeds the live notes lacking a current
vector, 64 per run, nudged by every write and retried on a 300 s tick: a failing provider costs
one request per tick and its error is in the status, never a hot loop. The vectors are derived:
dropping every one of them loses no note, revision, link or evidence, the export is unchanged,
search answers by words, and a run rebuilds them from the notes alone
(`hybrid_one_fingerprint_holds_one_width`,
`hybrid_vectors_are_derived_dropping_every_one_loses_no_note`,
`the_fingerprint_names_the_endpoint_not_the_provider_name`).

**Query.** A search with a provider and indexed notes embeds the query (within
min(`timeoutMs`, 4 s)) and takes the cosine to every candidate's current vector. The exact tiers
keep their scores and places; the word-tier hits (FTS, CJK, substring) and the cosine ranking
fuse by reciprocal rank (k = 60) into the 500..750 band — a hit first in both lists scores what a
perfect lexical hit does — plus the hub mention (§memory.search) and the record adjustments. A word hit is credited for its cosine
rank wherever it falls (a floor there once dropped a first-by-words note below a weaker one for a
cosine 0.001 under it); a note the words missed - one under the word floor among them - enters
only at or above the floor, at most 50 of them, as tier `semantic`. A tie goes to a hub's note, then the better word rank. Every hit with a vector
carries its `similarity`. The answer carries `semantic: {state: on | off | unavailable, model?,
reason?}`: a provider that fails or stalls costs only the semantic tier, never the search, and so
does an index of another width than the query's (`unavailable`, the reason naming both widths;
only the vectors of the query's width are scored when some are). The
brief's task section and association (§memory.related) use the same vectors
(`hybrid_indexing_staleness_and_purge`, `hybrid_search_fuses_meaning_and_degrades_to_lexical`,
`hybrid_brief_and_related_use_meaning`, `knowledge_api_embedding_status_test_and_semantic_search`,
`a_word_hit_keeps_its_meaning_credit_under_the_floor`, `a_fusion_tie_goes_to_the_better_word_rank`).

**Per-project.** A project may turn embedding off for itself: `embedding` in its settings
(§memory.settings), null inherits (on whenever a provider is configured), `false` never,
`true` even against a later default. The global project may set it too - it is about search,
not a checkout. Off, the project's notes are no embedding input: the indexer never embeds
them, and the status's `indexed`/`pending` never count them, whatever the index at large
says. Its reads are lexical only - search, the brief's task section and association alike -
and each answer says so (`semantic: {state: "off", reason: "the project's settings turn
embedding off"}`; a related reply adds `semantic` only when the similar tier did not run).
The full plan and a full run's scope say `off` for it. Its vectors are **kept**, not dropped:
switching back on re-embeds only what changed meanwhile
(`per_project::a_project_turning_embedding_off_is_lexical_only_and_keeps_its_vectors`,
`the_full_plan_says_when_a_projects_embedding_is_off`).

## §memory.links — The memory graph

Links are rows of their own `{from, to, kind, note?, actor, createdMs}` — linking never bumps a
revision. Kinds: `supports`, `depends_on` (directed), `contradicts`, `related_to` (symmetric,
stored once with `from < to`), and `supersedes` (system only, written by `replaces` and a
recheck's merge). Both ends
must be in the caller's scope (`memory_not_found` otherwise), no self links, ≤ 50 links per note,
idempotent add/remove answering `changed` (`knowledge_graph_link_rules`). Project deletion and
purge take the links with them (`knowledge_graph_project_delete_cascades`).

## §memory.related — Association search

`related(scope, seed: id | text ≤ 4 KiB, depth 1..3 = 2, limit 1..50 = 20, includeHistory)`
combines three normalized signals deterministically — graph proximity (BFS ≤ 3 hops
weighted by link kind), text (FTS5 OR over the seed's salient terms, bm25, the seed itself
excluded) and shared anchors — into a 0..100 score with `reasons` (`link` via a neighbour,
shared `text` terms, shared `anchor` paths); every candidate is scored, and the reasons are built
for the ones kept alone. With a provider (§memory.embedding) meaning is a
fourth signal: an id seed compares its own stored vector (no provider call), a text seed is
embedded like a query, and a note at or above the similarity floor carries a `similar` reason
with the cosine (`knowledge_association_ranks_linked_and_similar_first`,
`hybrid_brief_and_related_use_meaning`).

## §memory.graph — Graph reads

`graph(scope, center?, depth 1..3 = 1, includeHistory, limit 10..300 = 200)` returns
`{nodes, edges, truncated}`: the neighbourhood around a center ordered by hop, or the scope's
live notes most-connected first, each with the path of the domain it is filed in (absent:
unfiled); edges are every link among the returned nodes.

## §memory.rag — asking a question

`POST /api/knowledge/ask`, the MCP `memory_ask` tool, `swiss memory ask "QUESTION" [--agent
A] [--worktree PATH|BRANCH] [--json]` and the panel's Ask box all land in one flow: a natural-language question about
what the project's memory holds, answered by the agent the layers of §memory.settings name -
the ask's own `agent`, the project's settings, the sync block, the agent added last. A read,
so no nudge rule holds it back: the newest agent may answer. The first named worker answers
(a comma list is refused like a run's, and so is `"off"`: an ask names who answers, or leaves
`agent` out for the layers). The project whose settings count is the one the scope selects: a
project id names it, a cwd resolves to the project registered over it - the way every read
finds it - and an unregistered directory carries no settings. An unknown project id is
`memory_not_found` (404).

A question is 1..=4000 characters once trimmed (`memory_invalid` otherwise; the MCP schema's
`maxLength` is the same bound); a retrieval searches its first 512 (§memory.search's bound).
No agent anywhere - the no-workers sentence of §memory.settings - or the agent failing to
answer is `memory_agent_unavailable` (503), the sentence its `message`; a store error keeps
its own code.

The answerer works from the project's notes, the documents it keeps (§memory.docs) and its
code index with the same read-only shapes the CLI gives - `search` (notes by words), `docs`
(the documents' sections by words, `kind` one kind), `doc` (one document, or one of its
sections), `code` (where a name is defined and which files mention it, §memory.code),
`related` (the graph out from a note or a passage), `get` (one note whole), `domain` (the map
or one hub), `brief` - and the answer names the note ids (m-…), the document paths and the
code paths it rests on, or says plainly that memory holds nothing for the question. A
document's card names the code it is about and where that is defined, so "the code that
pauses a copy after 30 idle days" reaches the job's file in a search and one read; the
prompt says a design, plan, proposal or rejected document is intent, not what the code does,
and the answer says which it reports. It says where code lives only as a tool answered it - a
definition `code` lists, a path a card or a note cites; when `code` lists no definition (the
scan can miss one, and its answer says so) it names the files `code` says mention the name,
never a file guessed from a class name or a theme. A file that mentions a name is not where
that thing is done, and one `code` marks as naming it only in a comment does not use it: the
answer says what a file does only from a definition `code` lists or lines a note or a document
shows, else calls it a file that mentions the name; a rule a document states in general
("notifications go through X") places one case only when `code` shows that case's file using X
outside a comment. How it works them
depends on the runner:

- **A builtin agent loops** over those tools in its own bounded loop (the agent's `max_turns`
  and `timeout_ms`, §llm.agents): search, follow, read, answer. `mode` is "tools". When the
  scope has a checkout (a cwd, or a project's first root) it also takes the recheck's `read`
  and `bash` over the checkout's top - the working tree a cwd is in (§memory.scope): the
  repository's top from a subdirectory, a linked worktree itself, so a branch's new files are
  there to read - or the one the ask's `worktree` names - in the
  recheck's pieces, pinned there, credential files refused and what they serve redacted
  (§memory.init The recheck), the agent's `tools` narrowing them as it narrows a recheck's. Memory
  lags the code - a document's card says the commit it was last checked at, and a note or a
  document can still describe a plan - so the prompt has it check each specific it states (a
  route and the class serving it, a file or script path, a count, a retry, a state, a value)
  against the code first, cite a file only once the code index, the checkout's listing or a
  read showed it is there now, and, where the code and a note or a document disagree, report
  what the code does and name the note or document as out of date. What a step does (whether it
  retries, what a failure leaves) is read in the method that does it, never in a document's
  table, and where memory disagrees with itself (a note says a step never retries, a document
  says three times) the code decides before the answer, which never states both. A scope with
  no checkout, or an agent that takes neither tool, answers from memory alone, without that rule.
- **A spawned runtime (claude, pi) cannot call the tools**, so the gateway retrieves - a
  search over the question, then a related expansion of it, then the documents' sections that
  answer it - inlines what it found under the question, and asks once in the checkout the
  scope names, that same top, never a deeper cwd. `mode` is "inline". The notes are capped at 8 KiB and the documents at 8 KiB
  more, each cut on a character boundary and said to be cut (`… (cut)`). A scope with no checkout (a project without a root) has no checkout to spawn
  in: that is the error.

An ask may name the working tree its code is read in with `worktree` (API and MCP field, CLI
`--worktree`): a checkout's absolute path - a linked worktree or a directory in one - or a
branch some working tree of the scope's repository has checked out (`feature/x`, or
`refs/heads/feature/x`), found from the common dir's own `HEAD` files with no git spawned. It
serves an ask from the main checkout or by project id about a branch an agent is building in
a worktree. The named tree scopes the memory tools too, as an ask from inside it would: a
note on a file only the branch has reads fresh, not missing from the main checkout. A path
outside the project, or a branch no working tree has, is `memory_invalid` -
even for an agent that takes neither code tool - never quietly read in the main checkout; the
CLI sends a relative path (`.`, `../wt`) absolute, from its own shell.

Tests script a fake Ask for both paths (`the_tools_read_what_the_project_holds`,
`the_docs_tools_read_the_documents_the_project_keeps`,
`a_looper_works_the_tools_a_one_shot_gets_the_retrieval_inlined`,
`a_looper_checks_memory_against_the_code_with_the_checkouts_read_and_bash`,
`a_subdirectory_asks_at_its_top_and_a_worktree_in_itself`,
`a_named_worktree_or_branch_is_where_the_code_is_read`,
`the_retrieval_cut_never_splits_a_character`) and resolve the answerer over a real llm row
(`the_answerer_comes_from_the_layers_by_id_and_by_cwd_alike`); every surface refuses a
malformed ask (`the_ask_route_refuses_a_malformed_ask_and_says_no_agent_is_unavailable`,
`knowledge_mcp_ask_refuses_a_malformed_ask_and_says_no_agent`,
`knowledge_cli_ask_refuses_a_malformed_ask_locally`) - a real model is never asked by a test.
