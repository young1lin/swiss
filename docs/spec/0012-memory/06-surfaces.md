# Entry points and trust

Part 06 of [§memory](README.md), chapter 0012 of the [swiss specification](../../SPEC.md).

## §memory.cli — `swiss memory` offline

The same store, gateway running or stopped, never HTTP. Commands: `brief`, `save` (alias
`add`, `--domain PATH`, `--producer human|agent|bootstrap|import`, `--evidence SPEC`…, `--keywords
K1,K2` - commas of either width or `、` split them, `""` clears them on `edit`), `search [--domain PATH]`, `related`, `graph`, `list [--domain PATH|none]`, `show [--history]` (who wrote
the note and how it arrived, `confirmed` and `source-verified` apart, and an `Evidence:` line
per piece), `edit --revision N` (`--evidence SPEC`… alone needs no revision), `link` /
`unlink FROM KIND TO`, `mark ID verified|outdated|wrong` (`--evidence SPEC`… with verified), `confirm`, `restore --reason`,
`review`, `code index|status|find NAME|file PATH|drop [--all]` (§memory.code; a save prints each
code finding and says when the note was held), `rules [--bootstrap]` (the formation rules, §memory.capture), `lint [--type K]
[--title T] [--stdin] [--json]` (a draft's formation hints, exit 0 either way), `purge --confirm`, `domain list|show PATH [--history]|add PATH --title T [--summary S]
[--owns A,B] [--keywords K1,K2] [--stdin]|edit PATH --revision N [--path NEW] [--title|--summary|--owns|--keywords] [--stdin]|confirm
PATH [--all]|verify PATH|delete PATH --confirm PATH|file ID… --to PATH|--none|autofile` (`--stdin` is the
hub's body), `init [--here|--portable]`, `init --map [--redraw] [--plan] [--dry-run] [--wait] [--lang TAG] [--agent A[,B…]|off] [--lead L|off]`
(§memory.map; `--plan` is offline and asks no model, the rest needs the running gateway like
`sync`), `init --full [--redraw] [--plan] [--dry-run] [--wait] [--lang TAG] [--agent A[,B…]|off] [--lead L|off] [--max-asks N]`
(§memory.init; all of it, `--plan` too, needs the running gateway), `init --recheck [--dry-run] [--wait] [--lang TAG] [--agent A[,B…]|off] [--max-asks N]`
(§memory.init The recheck; needs the running gateway), `review --resolve [--changed] [--dry-run] [--wait] [--lang TAG]
[--agent A[,B…]|off] [--max-asks N]` (§memory.init The review run; needs the running gateway), `docs [list]|add PATH…|remove PATH…|search WORDS… [--kind K]
[--limit N]|read DOC [--section N]` (§memory.docs; offline on the store - `add` and `remove`
patch the project's `docs` sources), `docs sync [--dry-run] [--wait] [--lang TAG] [--max-asks N]
[--agent A[,B…]|off]` (queues a docs run; needs the running gateway), `project list|add|bind|rebind P IDENTITY [--cwd D] [--exact]|unbind|rename|delete|settings [--id P|--cwd D]
[--agent A[,B…]|off|inherit] [--lead L|off|inherit] [--json]`
(§memory.settings: no flag reads, a flag patches, offline on the store),
`status`, `export [--global|--all|--domain PATH] [--output F]` (`--domain` one domain of the
project with its subtree, refused beside `--all`), `import F [--dry-run] [--strategy skip|fail]
[--into P|.]` (§memory.store: `--into .` the project of the cwd; it prints where the file came
from, where it lands, what it wrote or would write and every conflict kept out with both sides),
`backup --output F`, `misses [list]|add QUERY [--wanted ID,ID] [--want TEXT] [--wrong ID,ID] [--shown ID,ID]
[--via V] [--style S] [--path P]…|export [--output F]|drop ID…|--all` (§memory.evaluation Misses; `export`
writes the fixture to stdout or a new file, never over one, and names each miss that wanted no note
on stderr), `hook print claude-code|codex|cursor` (prints a snippet, never edits agent
config; Claude Code's carries the `SessionEnd` nudge beside the `SessionStart` brief), and `sync
[--dry-run] [--since REV] [--agent A[,B…]|off] [--wait] [--json] | sync status | sync cancel RUN_ID | sync --nudge | sync hooks
install|uninstall` (§memory.sync; `--agent` as §memory.settings takes it) - the one command that needs the running gateway: it resolves
the project here, then asks the gateway over `/api/knowledge/sync` with the CLI key; `--wait`
follows the run to its end (exit 1 when it failed or was cancelled). `sync --nudge` behaves like `brief --hook`:
exit 0 always, `cwd` from stdin's JSON, two seconds at most. Scope is the process cwd (never mutated), `--cwd`, or `--global`. Unknown flags are
usage errors. Exit codes: 0 ok, 1 failure, 2 usage/validation, 3 conflict (revision, similar,
ambiguous, root bound, domain exists) with the current revision or the similar id in the message; `--json`
prints the views (and the error object) on stdout, diagnostics on stderr; `--stdin` is the entry
for long or sensitive text. `brief --hook` always exits 0, prints nothing on stdout on failure,
takes `cwd` from a JSON object on stdin when no `--cwd` is given, and gives up after 2 s
(`knowledge_cli_round_trip_from_a_nested_directory`, `knowledge_cli_exit_codes`,
`knowledge_cli_links_and_graph`, `knowledge_cli_hook_never_fails_the_session`,
`knowledge_cli_projects_and_transfer`, `knowledge_cli_domains`, `knowledge_cli_provenance_and_evidence`,
`knowledge_cli_code_index_and_a_held_save`, `knowledge_cli_docs`, `knowledge_cli_misses`).

## §memory.mcp — `/mcp/memory`

Exactly one builtin `memory` registry entry (the remote-plugin pattern, §remote.mcp), mounted
by the plugin's start and stopped by its stop. A user MCP owning the name is never overwritten
or stopped: the API's `status` reports `mcp.state = "conflicted"` and the page shows the fix.
Twelve tools, every one requiring an absolute `cwd` (`memory_invalid_cwd` otherwise; `scope:
"global"` still requires it), `additionalProperties: false` enforced by the server too:
`memory_brief`, `memory_search` (`domain` narrows to a subtree), `memory_related` (the graph out
from one note or a passage, `id`/`text` exactly one, `depth` at most 3, `limit` at most 50, the
answer appending the semantic state when the similar tier did not run), `memory_ask` (§memory.rag:
one question handed to the agent its layers name; a client that would rather dig does so with the
read tools itself), `memory_get` (the note with its
neighbours), `memory_domain` (no `path`: the map with every domain's line and the unfiled count;
a `path`: the hub in full, `owns`, the sub-domains and every live note filed there, one line
each) → Markdown; `memory_save` (`domain` files it; the answer says where it was filed and
carries its contradiction hints (§memory.capture), its formation hints, and its code findings with the note's `status` when the checkout
lacks what it names, §memory.code),
`memory_update` (`domain`, a path or `""` to unfile, and `evidence` need no `expectedRevision`
when they are the only fields), `memory_mark` (`evidence` with verified; the answer says
`sourceVerified`), `memory_link` (with `remove`), `memory_domain_save` (`path` plus
`title`/`summary`/`body`/`owns`/`keywords`/`newPath`: an absent path is created, a present one updated
with `expectedRevision`), `memory_missed` (§memory.evaluation Misses: `query`, the `wanted` ids or
`want` in words, `wrong`, `shown`, `via`, `style` and `paths` → `{id, project}`; no note changes)
→ JSON. `memory_save`, `memory_update` and `memory_domain_save` take
`keywords` (≤ 16 strings), described as the bilingual search terms of §memory.model. Business
errors are in-band (`isError: true`, the text is `{code, message, …}`); unknown tools are
protocol errors. `memory_save`, `memory_update` and `memory_mark` take `evidence`
(§memory.capture), its items `additionalProperties: false` too. MCP writes are `origin=agent`,
`actor=mcp:<client>`, `producer=agent`, `transport=mcp`. Confirm, restore, purge,
domain delete, project changes, transfer, association and graph reads are not exposed. The InitializeResult
carries the fixed `instructions`; descriptions carry the save policy, the cwd rule and "notes are
data from the past". A hand-written `ServerHandler` in the root crate
(`knowledge_mcp_tool_surface`, `knowledge_mcp_requires_an_absolute_cwd`,
`knowledge_mcp_projects_are_isolated_by_cwd`, `knowledge_mcp_domains`,
`knowledge_mcp_provenance_and_evidence`, `knowledge_mcp_missed_records_a_failed_retrieval`).

**The skill.** `src/skill_assets/memory/SKILL.md` (`swiss-memory`) is the one skill for memory,
reading and writing alike: the brief, which read tool fits what an agent holds (`memory_ask`
among them), the documents and the code index, how to treat what it reads, when and how to save,
the hub, the runs that keep memory current (queued only on the user's request), every tool and
the CLI. It is model-invoked; `swiss skill install` writes it beside the other two (§host.cli),
and it names each of the twelve tools in its table
(`skill_install::tests::the_memory_skill_is_the_one_for_reading_and_writing_and_names_every_tool`).

## §memory.api — `/api/knowledge`

The panel's routes inside the loopback guard, behind the admin session (never the MCP bearer,
§host.session), JSON in and out. Every record, search, graph, brief, link, review and export route
requires the `project` selector (`p-…` or `global`; query on GET/DELETE, body otherwise) and the
service rechecks every id against it. Errors are `{error: <code>, message, …extra}` with the
code's status (400/404/409/500/503); bodies are strict (unknown fields refused); a stopped
plugin answers 503. Routes: `GET status`; `GET|POST projects`, `PATCH|DELETE projects/{id}`,
`POST|PUT|DELETE projects/{id}/roots` (`PUT {identity, path, exact}` changes a directory in one
step); `GET|PATCH projects/{id}/settings` (§memory.settings: the project's own `agents` and
`lead`, null where a field inherits; `PATCH` is a merge patch answered as saved, and wakes the
queue worker); `POST brief`; `GET|POST records` (`domain`: a domain id, its
subtree, or `none` for the unfiled; a record carries `producer`, `transport`, `confirmed` and
`sourceVerified`, the full record its `evidence`), `GET|PUT|DELETE
records/{id}`, `POST records/{id}/mark|confirm|restore` (`POST records`, `PUT records/{id}` and
`mark` take `evidence`, §memory.capture), `POST records/file` (`{ids, domain |
null}`); `GET|POST domains` (the list carries each domain's counts and the unfiled count), `GET|PUT|DELETE
domains/{id}` (`history`; `PUT` with `expectedRevision`, a changed `path` moves; `DELETE` with
`confirm` = the path), `POST domains/{id}/confirm|verify` (`confirm` with `all`: the domain whole, answering
`{domain, domains, notes, held}`), `POST domains/autofile`, `GET
domains/coverage`; the previews (§memory.preview) `GET preview/file|files|log|commit|diff`;
`POST|DELETE links`; the code index (§memory.code) `GET|DELETE code`, `POST code/index`, `GET
code/find|file`; the misses (§memory.evaluation Misses) `GET|POST misses` (`{items}`, newest
first; a recorded one is answered whole, `actor` "panel"), `GET misses/export` (`{fixture,
queries, gaps}`) and `DELETE misses/{id}` (`{dropped}`); `GET search`
(`{items, semantic}`, §memory.embedding; `domain` as for records); `POST related` (its items,
plus `semantic` only when the similar tier did not run - §memory.embedding); `POST ask`
(§memory.rag: `{project | cwd, scope?, question, agent?}` - the panel sends a project id, the
CLI a cwd - answered `{answer, agent, model, mode}`, `mode` "tools" or "inline" by how the
runner worked it); `GET graph`
(each node carries its domain's path); `GET review`; `GET
export` (`domain` one domain's subtree; a JSONL attachment named `memory-{project}[-{domain}].jsonl`,
the domain's path with every other character than a letter, digit or `_` as `-`); `POST import`
(`{project, text, dryRun?, strategy?}` → the import report of §memory.store, `project` the
project it lands in, so a file of several projects is refused here; the text bounded by the body
limit, §host.routes); `POST backup` (the server picks the path under the home); `GET
embedding` (the llm plugin's block memory serves by, its provider by name - the key stays on the
provider entry and is never shown here - `remote` — note text leaves this machine — `configError`, and the index `status`: `off | ready | indexing | stale | error`, model,
dims (the index's one width), indexed, pending, incompatible (vectors of another width, never
scored), lastError, lastRunMs); `POST embedding/test` (`{text?}` → `{ok, model, dims,
ms}`, or `{ok: false, error}` — a provider's refusal is an answer, not a failure); `POST
embedding/reindex` (`{drop?}`: drop the model's vectors first, then nudge the indexer); `GET
sync` (§memory.sync: the effective block, `configError`, and whom a run would ask
(§memory.settings) - `workers`, each as the llm row holds it now, `{name, runtime, model}` or
with `error` the sentence a run would fail with; `lead`; `chosen` (`{agents: {names, from}, lead:
{…}}`, `from` the layer that decided: `run | project | plugin | newest`, null when none did);
`settings`, the project's own; and `agent`, the first worker, kept for the block editor - then
the project's `state`, what is `pending` - commits and transcript bytes since the head - and the recent runs);
`POST sync` (`{dryRun?, since?, map?, full?, recheck?, docs?, review?, language?, redraw?, maxAsks?, agents?, lead?}` → the queued run, 202;
`map: true` queues a map run, §memory.map, `full: true` a full run, §memory.init - both take
`language` and `redraw` and refuse `since`; `recheck: true` a recheck, §memory.init The recheck -
it takes `language` and `maxAsks` and refuses `since` and `redraw`; `agents` (a name, a list,
or "off") and `lead` (a name or "off") pick this run's own workers and lead (§memory.settings),
`lead` only with `map` or `full`; two of `map`, `full` and
`recheck` together, `language` without one, `redraw` without `map` or `full`, and `maxAsks`
without `full` or `recheck` are refused; `docs: true` a docs run, §memory.docs - it takes
`language`, `maxAsks` and `agents` and refuses `since`, `redraw`, `lead` and any other run
kind beside it, and so does `review: true`, a review run, §memory.init The review run - with
`changed: true` the notes whose code changed alone, `changed` refused without `review`); `GET sync/map[?redraw=1]` (the areas a
map run would draw now, no model asked - `{plan: [{path, title, kind, owns, files, uses?, kept?}],
asks, review, removed, codeFiles, ownedFiles, mappedFiles, truncated}`: `review` whether the top
level goes to the model first, `removed` what the redraw would remove); `GET
sync/full[?redraw=1&maxAsks=N]` (what a full run would do, no model asked - the map plan plus
`knowledge: {files, bytes, slices, asks, unchanged, more, unread}` and `embedding: {state, model?,
indexed, pending, error?}`; `maxAsks` 0 is refused);
`POST
sync/nudge` (`{cwd}`, no `project`: the directory selects it → `{queued, run?, reason?}`); `GET
sync/runs/{id}` (one run in full: scope, proposal and every operation's outcome); `POST
sync/runs/{id}/cancel[?project=P]` (§memory.sync Cancel → `{run, stopping}`; no `project`, the run by its id alone; the overview `GET
sync` lists the running runs told to stop as `stopping`); `GET docs?project=P[&q=&kind=&limit=]`
(§memory.docs: no `q`, the project's `{sources, docs, carded}`, each document with its card;
with `q`, `{hits}` - the sections that match, best first, `kind` one kind, `limit` at most
30; `kind` or `limit` without `q` is refused) and `GET docs/read?project=P&doc=D[&section=N]`
(one document: its card, outline and text, whole when short, else the section; `doc` an id,
a path or a unique tail; one no project keeps is 404). Records and
domains carry `keywords`, and `POST|PUT records` and `POST|PUT domains` take them
(§memory.model). The
`sync` block is saved through the host's `PUT /api/plugins/memory/config` and the embedding block
through `PUT /api/plugins/llm/config`, never here
(`knowledge_api_project_and_note_crud`, `knowledge_api_graph_related_and_status`,
`knowledge_api_answers_503_when_withdrawn`, `knowledge_api_embedding_status_test_and_semantic_search`,
`knowledge_api_domains_crud_and_filing`, `knowledge_api_domains_coverage`,
`knowledge_api_projects_change_a_directory`,
`knowledge_api_previews_stay_inside_the_checkout`, `knowledge_api_provenance_and_evidence`,
`knowledge_api_code_index`, `knowledge_api_misses_record_list_export_and_drop`,
`knowledge_api_domain_export_and_import_report`).

## §memory.panel — The Memory page

One `#memory` page, a workspace (the pane full-bleed, each column scrolling on its own): a
project rail (global first, live counts, a review badge; create a project with an optional
directory, rename, add/remove directories, change a directory in place, delete by typing the id;
*Export…* downloads the whole project or one domain, and *Import…* reads a file into the
project, checked first - a dry run naming where the file came from, what it would write and
every conflict, *edited* or *same title*, the file's text beside the store's - and imported only
after, the store's notes standing (§memory.store); the gear opens the semantic
search sheet), the project head's **Auto-update** button (a project's, never the global one's,
first among its actions) saying how its daily review stands - *Auto-update · daily 21:30*,
*Auto-update · off*, the bare word while its settings are unread - and opening the auto-update
sheet, the MCP conflict banner, and five tabs — **Domains** (the map beside the domain
picked from it: the tree, each level a step further in, with each domain's notes, review count, unconfirmed mark and drift,
then *Unfiled* and *Not covered*; the domain shows its title, path, summary, hub drawn as
Markdown, `owns` - each opening its preview - its sub-domains, its notes, its history, and the
actions: new domain, edit with the conflict message, move, confirm (always the domain whole, so
*Confirm all* once a domain or note under it waits too, the toast saying how many notes it took
and how many that need review wait in Review), verified, file matching
notes, delete by typing the path; a domain's keywords show as chips and are edited as one comma list; *Unfiled* lists the unfiled notes to file, *Not covered* the
directories no domain owns, each one click from a new domain owning it; the map paints from the
list, and coverage fills in drift and *Not covered* when it answers), **Notes** (the list beside the note
picked from it: keyword search with scores and, with a provider, whether meaning was matched,
or *Associate* over a pasted passage with explained scores; beside the two modes, the
**Ask** button opens the ask sheet (§memory.rag: a question, the agent left to the layers or
named, the answer drawn as Markdown with the note ids it rests on and the runner said
underneath); kind/status/unconfirmed/history/domain filters in a collapsed Advanced fold,
whose summary names active constraints and Clear resets them while preserving search text;
common search stays directly available; new note, filed in the domain picked), **Review** (the same two columns over the inbox: confirm, verified,
outdated, wrong, each moving on to the next note waiting; its bar says the daily review under
the inbox's own line - on, its time and how many pending notes of every origin it takes
then; off with such notes waiting, tagged *no daily review*, that they stay as they are, with
*Turn on the daily review*; either way one click from the auto-update sheet), **Graph** (the project graph or a
note's neighbourhood on the vendored Cytoscape.js, §panel.vendor, loaded on first use:
concentric chips around a centre; the whole project, once any note is filed, a named box per
domain round its notes - a sub-domain's inside its parent's, *Unfiled* a box of its own, a box
with no note not drawn - each box a list of its notes in columns (a dot, its name to the right,
cut to its row unless pointed at or selected, every name shown while the zoom leaves a row room
for one), the boxes packed to the shape that fits the pane largest, each dropped to the lowest
free spot so short boxes stack beside a tall one (the biggest first, the same data the same
picture), the box names always shown - a sub-domain's a step quieter than its parent's - and a
link a thin quiet line under the notes
until the selection lifts its own; with nothing filed, a force-directed map turned a quarter
when its long side crosses the pane's, the most linked quarter (at most 12) named always and the
rest once zoomed in or in reach of the pointer or the selection; either way the notes are dots
sized by their links with names one size on screen at any zoom, coloured by kind, arrows on
directed links, history dimmed; a tap selects into an inspector - a tap on a box clears it - and
a double tap opens the note)
and **Agent** (the exact brief agents get, with paths and task inputs: drawn as the Markdown it
is, its source one toggle away, Copy copying the source). The detail shows
content (drawn as Markdown by `ui/markdown.ts` on the vendored markdown-it in its safe mode - a
note's own HTML is text, links only http(s) and mailto, no images - and a ```` ```mermaid ```` fence
drawn as its diagram by the vendored mermaid at `securityLevel: "sandbox"`, a sandboxed iframe, so
nothing a diagram carries runs in the panel's origin; a diagram that does not parse stays as code,
the reason in its title), its domain (a step to the map; Move files it elsewhere), anchors with freshness, refs, provenance (who wrote it via which entry point, the writing actor
in its title; a *source-verified* tag apart from the confirm button), its evidence (each piece's
source opening its preview, its relation and revision, one attached to earlier text greyed) and markers, its links grouped by kind (add by
search, remove), its own small graph, related notes (the note shows as soon as it and its graph
are read; the association, the slowest of the three reads, fills its section when it arrives,
*Finding related notes…* until then), history, and the actions: edit with the
conflict message (keywords one comma list - either width, or `、`), mark, confirm, restore,
supersede and delete. **Previews** (§memory.preview):
an anchor opens a wide sheet with the file as it is now (numbered, a Markdown file drawn with
its source one toggle away), a directory's listing or a glob's files, and a **History** tab - the
commits that touched the path, the ones after the note was last saved or verified tagged
*after the note*, and the uncommitted change; an anchor whose file changed opens on History. A
`commit <hash>` ref opens the commit (message, files, the coloured patch, `ui/diff.ts`), a
`path: heading` ref the file at that heading, a URL a new tab; a preview opened from a preview
stacks, and Escape closes one layer at a time. The semantic search sheet shows the index
state and the model in force, read-only (the llm plugin's block, §llm.embedding), names the host
whenever note text would leave the machine, tests the model in force and re-embeds; *Edit on the
LLM page* goes to `#llm-embedding`. The **sync sheet** (§memory.sync), opened from the project
rail beside it, shows the project's sync - the workers in force (each a name, runtime and model,
a pool joined with `+`, or the sentence a run would fail with, and the layer that chose
them: the project's settings, the `sync` block, the plugin's newest), the lead in force named
the same way, its head, what is pending (the commits, and the conversations unread as a size:
KiB, MiB, GiB), *Sync now* and *Dry run*, and the
runs as a flow (§panel.ui) - a state dot, what queued the run, its marks (dry, redraw, the
areas described, the phase) and its age on one line, its verdict under it - each opening to its scope, summary and every operation with its outcome, a run that asked
naming its workers and how each fared, a map run with
how many areas it described, a full run with its notes saved, held and known from the slices
asked, what waits, what failed and the index as it ended, and what its lead wrapped up (domains,
hubs refreshed, notes) and what a recheck came to (questions asked, notes kept, fixed, dropped, merged,
links added, still held, what waits, failed or was refused; a dry one what it would ask, an empty one
that no note waits). Those lines speak the panel's language; the run's summary is the engine's
English for the CLI and the log, so a recheck run, or a full run that was not a dry run, keeps it
as the hover of its first line rather than saying everything twice in two languages
(`a recheck's run says its outcome in the panel's language, never the engine's English beside it`) -
while a run is queued or running the open sheet re-reads it every 3 s, says its phase in words and offers *Stop* on it (*stopping…*
once told, until it ends `cancelled`; §memory.sync Cancel); **Map and initialize from the code** (§memory.map,
§memory.init) previews the areas a map run would draw (`GET sync/map`: each area's path, title,
files and owns, the ones on the map already tagged *kept*, what a redraw removes and whether the
review is asked; an empty preview says the map already owns the code, never that the index
holds none) and queues one with a language, an optional dry run, *Redraw* and **this run's own**
workers and lead (§memory.settings: left out, the layers decide; the lead only beside a map or
full run, never a recheck - the pick rides the queued run's record and nowhere else); **Preview
initialize** shows what a full run would read and ask and the index its notes land in (`GET
sync/full`, off said as lexical search only), and **Initialize memory** queues it with the same
choices and a *Max questions* cap; **Recheck notes** queues a recheck (§memory.init The recheck)
with the language, the dry run and the cap, never the redraw - its row, and a full run's, says
the contradictions it lifted, the rounds it asked again for them and those still open, when any -
and **Resolve the review inbox** a review run (§memory.init The review run) with the same, never
the lead - its row says what it confirmed and the contradictions it lifted, then the same -
**Review what changed** the same run of the notes whose code changed alone (its row, and the
daily review's, reads *review · code changed*) - and edits the
`sync` block alone (a GET for the revision, then the CAS PUT): agents are a three-way pick -
*inherit* (the block says nothing and the layers below decide; an untouched save writes the
field back absent, never silently off), *off*, or a checkbox pool of the llm plugin's agents
as `name · runtime` - one name saves a string, more a list, none named is refused; a name the
llm row no longer defines stays shown and checked, and the llm plugin not answering is said -
the lead beside it *inherit*, *off* or one name, then the conversations, their directories,
auto and the minimum interval. **The project's own settings** (§memory.settings) sit in their
own fold - the same agents pool, the lead and embedding (*inherit*, *on*, *off*) - saved by one
PATCH carrying those three alone and repainted from the answer; the global project may set them
too. The daily review is not set there: for a project the fold says how it stands (*daily
21:30*, *off*) and *Set in Auto-update* opens that sheet in place of this one. *Agents* goes to
`#llm-agents`, where the runtime, the model and the key live.

The **auto-update sheet**, opened from the head and from the Review bar (never for the global
project, which has no checkout), gathers what keeps a project's memory up with its code without
a click. **Daily review** (§memory.settings The daily review): a switch; while it is on, the time
of day and *Runs a day* (1 to 10, empty the default 3 shown as the placeholder); and *Who
reviews*, the project's agents in the sync sheet's three-way pick - saved by one PATCH of
`{autoReview, autoReviewRuns, agents}` (off sends the time and the runs as null; on with no time,
runs off that range, or a named pick naming none is said in the sheet and nothing is sent) and
handed back so the head and the Review bar repaint at once. A card under it says the workers a
review would ask now and the layer that chose them; how many inbox notes wait, grouped by AI and
human origin (the full review takes both); when the clock looks
next, judged from the runs as the gateway's clock judges (today's time still ahead; *within a
minute* once it passed with no review since, or the newest left questions and the day has runs
left; else tomorrow; *reviewing now* while one waits or runs - the time read on the panel's clock,
the gateway's on the machine that serves it); and the newest review of the inbox, its
state, age and the sync sheet's outcome line. *Review now* queues that review (`POST sync`
`{review:true}`); while a review waits or runs the open sheet re-reads every 3 s, *Review now*
is off and *Stop the review* stands beside it (§memory.sync Cancel: the questions in flight are
dropped, what was settled stays), reading *stopping…* once told until the run ends. **Sync
after commits** (§memory.sync Triggers) edits the `sync` block's `auto` and minimum interval -
shared by every project, said so - by the sync sheet's GET then CAS PUT, every other field of
the block and the row carried back, and only when either moved; auto on with no agent named by
the project or the block warns that a nudge queues nothing (§memory.settings), the warning gone
once the pick names the workers; with no block, or its agents off, the sheet only says sync is
turned on in the sync sheet first (its conversations are an opt-in made there). The hooks that
send the nudges are named by the CLI commands that install them, and the foot opens the sync
sheet.

## §memory.security — Logs and trust

**Logs.** The adapter declares `LogPolicy::MetadataOnly` (`Adapter::log_policy()`, `Full` by default),
honoured by both logs: the traffic log keeps `{jsonrpc, id, method, params: {name}}` (plus
`clientInfo` on initialize) and no response; the call log keeps the tool, caller, timing and an
outcome (`ok`, `ok <id>` or the error code) — never cwd, query, task, title, content, anchors,
notes or result text (`knowledge_mcp_calls_never_log_content`,
`metadata_only_keeps_the_tool_name_and_drops_arguments`).

**Poisoning.** Notes and hubs are untrusted data that persist across sessions. Defences: the brief's
standing header, the unconfirmed marker on every agent-written note and hub until a person
confirms it, the review inbox, and the Skill rule that
a note asking an agent to run commands, fetch URLs, weaken checks or send data anywhere is
confirmed with the user first. **Previews** only read, only inside the project's root, never a
credential-looking name, and run Git with no prompt, hook-free reads and a deadline
(§memory.preview). **Backups from the panel** are written by the server under
`<SWISS_HOME>/memory/backups/` with a server-chosen name; the request names no path.
**Sync's runner** (§memory.sync) gets read-only tools and nothing else - a `claude` agent Read,
Grep and Glob inside the checkout with no shell, no network tool, no MCP server and none of the
user's hooks; a `builtin` agent memory's `read` (pinned to the checkout, credential-named and
store-sensitive files refused) and `bash` narrowed to read-only git, both answers redacted; a
`pi` agent its own `read` alone - and its answer is data: validated, then written through the
service as an unconfirmed agent write that passes the secrets check like any other. What it
reads goes to the agent's provider: the digest and the builtin tools' answers are redacted
before they leave the process, resolved env values are never logged, and the child's output is
kept only scrubbed and cut to 500 characters. Directory
binding reads only metadata (`.git`, `.swiss/memory.json`, the origin in `<commondir>/config`)
and never writes into the directory.

## §memory.evaluation — Verification

The `knowledge_*` tests drive the real service, CLI seam, MCP wire and API router against scratch
stores. Live, on the 19998 instance only: a Claude Code session with the SessionStart hook gets
the brief; a note saved over MCP from one agent appears in another's next brief for the same
project and never in another project; in the panel, create a project, bind a directory, add and
link two notes, open the graph and the association search.

**Retrieval fixtures.** `crates/swiss-memory/tests/fixtures/*.json` are a project's worth of
notes — kinds, anchors, links, supersessions, agent origins, and the project's map: domains
with hubs and `owns`, most notes filed by their anchors — plus the queries a person would
type, each tagged with a style (`keyword`, `natural`, `path`, `short`, `cross`, `symbol` - an
identifier among words other notes share) and its expected
notes: `swiss-history.json` (45 notes from this repository's history, English) and
`cc-tutorial.json` (38 notes from a Chinese tutorial project). A note may carry `evidence`
(the CLI's compact form) and `drift` (its anchor files change after the load, so it reads
stale); a query may carry `paths` (the files the asker works on) and `avoid` (notes whose
presence makes a context wrong). `eval::load` writes a fixture through the service, each
note's id derived from its key and its clock the load's start plus its place in the array, so
equal scores break the same way every load and an evaluation reproduces to the digit
(`two_loads_evaluate_alike`; a key's id already taken in the store is minted afresh);
`eval::evaluate` runs every query in one **mode** - `lexical` (the word tiers alone, even with
an embedder set: semantic OFF), `hybrid` (the search as every entry point runs it: semantic ON
when a provider is set and the scope indexed), `brief-task` (the brief with the query as its
task), `brief-paths` (the task and its `paths`; a query without paths runs as `brief-task`) -
and judges each query's **context**: a search's first five hits, or every note a brief shows,
in reading order (`BriefOutput.shown`, not on the wire; a rejected precedent line, §memory.brief,
names history, not a note shown as current). Per style and overall: recall@1, @3
and @5 and MRR@5; the context's mean size in notes; **wrong-domain** - of the context notes
some expected note's domain can judge, those filed apart from every expected note's domain
(neither it, above it nor under it; an unfiled note claims none and is not judged), a share
and a count beside it, since a smaller context raises the share of what stays; **unsupported** - context notes no checkable
evidence verifies as they read now; **stale** - context notes with a changed anchor, a broken
dependency or a status other than active; **fully correct** - queries whose context holds an
expected note and nothing stale, filed apart or avoided (support is the corpus's property,
reported apart). Every report records its **setup** - the fingerprint and the provider,
model and endpoint hash it names, the index width, state, indexed, pending and incompatible
counts - and `eval::compare` sets reports side by side, the A/B of modes and setups
(`eval_judges_every_context_by_hand`, `a_context_is_judged_note_by_note`). The
tests check the load (every note distinct, agent notes unconfirmed, every supersession and
link in the graph, anchored briefs, every note filed where its fixture says - by its anchors or
by hand - and a session-start brief that is the map, not the notes), score every mode (no
embedder: hybrid answers what lexical does; unsupported below 0.8; a search the retrieval
readout calls strong has an expected note first) and floor the lexical scores: keyword ≥ 0.8, path ≥ 0.9 and symbol ≥ 0.9 (English), keyword ≥ 0.8 and short ≥ 0.75
(Chinese) - and the brief for the
task alone on path queries ≥ 0.9, which only the symbol tier reaches (0.8 without it): a path is
an identifier, so its notes come in though no path is handed over
(`fixture_swiss_history_english`, `fixture_cc_tutorial_chinese`). The scores today, lexical
with no embedder:

| Fixture | Context | R@1 | R@5 | MRR@5 | Notes | Filed apart | Unsupported | Fully correct |
|---|---|---|---|---|---|---|---|---|
| English, 36 queries | search | 0.875 | 1.000 | 0.986 | 3.2 | 0.43 (48) | 0.64 | 0.33 |
| English | brief for the task | 0.847 | 1.000 | 0.972 | 5.1 | 0.22 (38) | 0.70 | 0.44 |
| Chinese, 25 queries | search | 0.820 | 0.980 | 0.960 | 2.6 | 0.29 (19) | 0.53 | 0.48 |
| Chinese | brief for the task | 0.860 | 0.980 | 0.980 | 4.4 | 0.19 (21) | 0.41 | 0.52 |

The brief with its paths scores as the brief for the task alone: the five queries that carry
paths are those paths, which the symbol tier already anchors - and the first task hit stays the
first when its paths already showed it, the next not promoted to show in full
(`brief::tests::a_task_hit_sharing_a_word_or_two_is_a_line`). Recall has risen with the ranking
work (0.83 and 0.84 at first) to near its ceiling; what the contexts carry was the work since.
Read note by note, a search's five hits were mostly filler: a context of five took a weak hit
whenever fewer notes matched well, and four of a natural question's five, filed in other
domains, each shared one word with it; the brief's English task hits had no floor at all, so
such a hit was shown in full and voted its domain into play. The word floor (§memory.search)
took a search's contexts filed apart from 0.55 and 0.44 to 0.46 and 0.29 and a brief's from
0.46 and 0.21 to 0.40 and 0.20, the fully correct from 0.17 and 0.20 to 0.25 and 0.48 (briefs
0.11 and 0.32 to 0.14 and 0.36); a path counted by the words naming its part (§memory.search)
took the English path queries' search contexts from 0.36 filed apart and none fully correct to
0.13 and 0.60, their briefs from 0.41 and none to 0.20 and 0.20; the brief's linked notes
following only the subject (§memory.brief) took its English notes filed apart from 0.38 to 0.27
- the symbol queries' from 0.54 to 0.35 - and the Chinese from 0.20 to 0.15; a task hit sharing
a word or two shown as a line, or under a strong readout not at all, and the in-play domain
counted rather than listed (§memory.brief) took a brief from 8.1 and 7.2 notes to 5.1 and 4.4,
its notes filed apart from 75 and 27 to 38 and 21 (a share of 0.27 and 0.15 to 0.22 and 0.19)
and its fully correct from 0.17 and 0.36 to 0.44 and 0.52; recall held through each. Support measures the corpus: the notes whose text cites one of their own files
(12 of the English 45, 20 of the Chinese 38, an agent's guess never) carry it as `verified_by`
code or doc evidence, and every note was unsupported while the fixtures carried none. The
retrieval readout (§memory.search) holds against the answers: of the 61 queries' searches it
calls 42 strong, every one with an expected note first, 17 mixed (14 first) and 2 weak -
natural questions whose note shares few of their words. What
stays wrong is mostly a note holding one or two of a short query's words - a share of 0.2 to
0.4 - shown in a search's five or a line under Also matching; a floor relative to the best hit
would drop most of it, but one expected note holds only 0.54 of its query's best share, so the
floor stays absolute. The symbol style's nine queries reach R@1 1.000 with or
without the symbol tier - their identifiers are rare words bm25 already ranks first; the tier's
measured work is the brief's path queries and an exact note held against meaning
(`hybrid_an_exact_identifier_beats_words_and_meaning`). The hub mention (§memory.search) moves
no score: the fixtures' hubs, like a surveyed one, name code and never a note's title or id; its
measured work is the tie it breaks (`knowledge_search_a_hub_mention_breaks_a_tie`).

**Real models.** Two tests of swiss-plugins (`crates/swiss-plugins/src/memory_live.rs`) measure against real
models and are ignored in every default run: each reads its SWISS_LIVE_* variables from the
environment or else the repo's `.env`, as the e2e live file does (`SWISS_LIVE_ENV` names another
file), hands a key to its client as a `${SWISS_LIVE_*_API_KEY}` reference rather than its
value, and skips itself without them: `cargo test -p swiss-plugins --lib memory_live -- --ignored --nocapture`.
There is no CLI for it: the CLI process holds no embedder. `fixtures_score_with_a_real_embedder` loads each
fixture into a scratch store and scores every mode twice, with no embedder and then with every
note embedded by the SWISS_LIVE_EMBED_* endpoint (`SWISS_LIVE_EMBED_FLOOR` overriding the floor),
printed side by side. With GLM's embedding-3 (2048 wide), each cell R@1 · filed apart · fully
correct:

| Floor | English search | English brief | Chinese search | Chinese brief |
|---|---|---|---|---|
| no embedder | 0.875 · 0.43 · 0.33 | 0.847 · 0.22 · 0.44 | 0.820 · 0.29 · 0.48 | 0.860 · 0.19 · 0.52 |
| 0.3, the default | 0.861 · 0.51 · 0.06 | 0.875 · 0.30 · 0.33 | 0.900 · 0.53 · 0.08 | 0.900 · 0.32 · 0.40 |
| 0.4 | 0.861 · 0.48 · 0.14 | 0.875 · 0.27 · 0.39 | 0.900 · 0.43 · 0.28 | 0.900 · 0.28 · 0.48 |
| 0.5 | 0.861 · 0.44 · 0.25 | 0.875 · 0.22 · 0.44 | 0.900 · 0.27 · 0.48 | 0.900 · 0.19 · 0.52 |
| 0.6 | 0.861 · 0.43 · 0.31 | 0.875 · 0.22 · 0.44 | 0.900 · 0.27 · 0.48 | 0.900 · 0.19 · 0.52 |

Recall@5 holds at every floor. Meaning reorders what the words found: the Chinese searches'
MRR@5 goes from 0.96 to 1.00 and every brief's first note is right more often, while one English
search loses its first place to a meaning neighbour at every floor. Below 0.6, embedding-3's
unrelated pairs pass the floor and its meaning-only neighbours fill a search's five, filed
apart, so almost no search context stays fully correct at the default 0.3. A brief shows a hit
found by meaning alone as a line, never in full, and none under a strong readout (§memory.brief):
before it did, no brief stayed fully correct at 0.3 either. At 0.6 the contexts are what the
words leave, but for one English search's, and the reordering stays: this model wants
`minSimilarity` 0.6 (§memory.embedding).

`a_model_judges_the_briefs_for_its_requests` judges what an agent receives, not rank positions.
A chat model (SWISS_LIVE_LLM_*, asked at low effort) takes twelve current notes of each fixture,
spread evenly through it, and writes for each the short question a developer who has not read
the note would type, leaving out its answer, its rules and its details. The test builds the
brief for that task, as `brief-task` does, first with no embedder and then with
SWISS_LIVE_EMBED_*. The model then grades each brief as the agent receives it: which shown
notes help, which would mislead, whether the brief answers the question, and what it lacks.
Only ids the brief showed count (`a_grade_counts_only_the_ids_the_brief_showed`). Each grade is
tallied by where the note stood, too: its section, its rank under Related to your task, a linked
note's relation and the place of the note it hangs from, the kinds in the other sections, and
where the question's own note stood - a note counted on its own line, never where another
note's text names it (`a_shown_note_stands_on_its_own_line_not_where_it_is_mentioned`). With
glm-5.3-flash writing and grading, before and after the brief's tail and its in-play domain
became lines and a count (§memory.brief):

| Fixture, 12 questions | Brief | Its note shown | Answerable | Shown notes that help | That mislead | Notes shown |
|---|---|---|---|---|---|---|
| English, no embedder | before | 1.00 | 0.92 | 0.26 | 0.03 | 9.8 |
| | after | 1.00 | 0.83 | 0.34 | 0.04 | 6.6 |
| English, embedding-3 at 0.6 | before | 1.00 | 1.00 | 0.28 | 0.04 | 9.8 |
| | after | 1.00 | 0.92 | 0.34 | 0.04 | 6.6 |
| Chinese, no embedder | before | 1.00 | 1.00 | 0.21 | 0.02 | 8.2 |
| | after | 1.00 | 1.00 | 0.36 | 0.00 | 4.2 |
| Chinese, embedding-3 at 0.6 | before | 1.00 | 1.00 | 0.25 | 0.02 | 8.2 |
| | after | 1.00 | 1.00 | 0.33 | 0.02 | 4.1 |

Every brief shows the note its question came from, and nearly every one answers the question;
an unanswered one asks for a diagnosis no note holds, its note shown. What a brief lacked was
precision: of the eight to ten notes it showed, about a quarter helped. By section (both
fixtures, no embedder, shown and helping) the first task hit was 24 and 24, the hits after it 46
and 12, the linked notes 43 and 8, the in-play domain's listing 54 and 2, the rules 36 and 2 - so
the tail of the hits became lines and the listing a count. After, the task hits are 32 and 26,
the linked notes - 31 of 36 now hanging from the first hit - 36 and 14, Also matching your task 27
and 3, the rules 34 and 2. The rules stay: a rule is a standing instruction, not an answer to the
question. So does a linked note that contradicts a shown one, which the judge calls misleading:
the line is the warning. Also matching, kept only where the readout is mixed or weak, is now the
weakest section and the next to look at. A grade is one model's reading of one run: the model
writes its questions afresh each run, single answers move between runs - answerable reads 0.83
to 1.00 for the same briefs - and the shares move by a few hundredths.

**Misses.** A retrieval that failed in real use is recorded where it happened: `memory_missed`
over MCP, `POST misses` from the panel, `swiss memory misses add` (§memory.mcp, §memory.api,
§memory.cli). A miss is the query or task, which retrieval ran it (`search`, `brief`, `related`,
`ask`, `domain`), its style if the recorder knows it, the notes that came back, the ones that
misled, and the notes that should have come - or, when no note holds it, what was wanted in
words; it says one of the last two, a note is never both wanted and wrong, every note it names is
the project's or a shared one, its paths are relative and inside the checkout, and a credential
in its text is refused like a note's. A miss changes no note; it is kept per project in
`memory_misses` (gone with its project, never in an export or import, §memory.store), the
newest 500 of them, recording one past that dropping the oldest. The **export** (`swiss memory
misses export`, `GET misses/export`) is a fixture `eval::load` reads back: the project's
domains, its live notes and any other note a miss names - each keyed by its id, with its kind,
text, anchors, keywords, domain, links between carried notes, `replaces` where a carried
successor supersedes it, its evidence bound to the text as it reads now and `agent` origin
while unconfirmed - then, oldest first, one query per miss that names a wanted note: those
expected, the wrong ones avoided, its paths, and its style as recorded or read from its shape
(a lone path is `path`, an identifier among words `symbol`, two words or four CJK characters at
most `short`, else `natural`). A miss that names no wanted note is a **gap**, listed beside the
fixture: a note to write, not a query to score. Reloaded and evaluated, an exported miss fails as
it failed in use, so a fix shows in the scores and stays held there
(`a_recorded_miss_exports_as_a_fixture_that_fails_alike`, `a_project_keeps_its_newest_misses`,
`a_miss_says_what_was_wanted`, `a_miss_without_a_style_reads_as_its_shape`).
