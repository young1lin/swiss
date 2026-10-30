# Initialization

Part 04 of [§memory](README.md), chapter 0012 of the [swiss specification](../../SPEC.md).

## §memory.map — The first map, drawn from the code

A project with notes but no map has no shape to file them in, and a hub written by hand for
every area is the work nobody does. **`swiss memory init --map`** draws the map from the code
**as it is now** - never from its history, which sync reads (§memory.sync) - in a way a small,
cheap model can do: the program decides the areas and checks every claim, and the model answers
one bounded question per area by filling fixed fields. A model that answers badly, or no model
at all, still leaves a map; every hub falls back to what the code index says.

**The skeleton** is read off the code index (§memory.code), refreshed first, and the checkout's
manifests. Code is the indexed files plus the source files the index reads no names from (`vue`,
`sql`, `proto`, …), never under a test, vendored, built, generated, example or docs directory,
nor a file the root `.gitattributes` marks `linguist-generated` or `linguist-vendored` - emitted
code a repository commits (the last matching rule decides, as in Git).
Areas are, in order of preference: **packages** - directories with a manifest (`Cargo.toml`,
`package.json`, `go.mod`, `pom.xml`, `pyproject.toml`, …), the outermost ones, taken when there
are two or more or one holds at least 30 % of the code; then **directories** of three code files
or more, a wrapper (`src`, `lib`, `app`, `internal`, `pkg`, `crates`, `packages`, `modules`, …)
looked through to its children; and in a **layered** layout - two or more technical layers side
by side (`controller`, `service`, `dal`, `repository`, `dto`, `remote`, `handlers`, …) - the
business areas cut across them by file name, as do the files sharing a name stem in a flat
directory (three or more; a generic first word - `index`, `util`, `base`, `config` - names no
business). A business owns its files under the directory the layers lie in
(`<that>/**/Invoice*`, never the whole checkout) and the directories inside a layer named for
it (`web/res/invoice`, `service/ecpay`; a name starting with a stem of three letters or more
counts, the longest stem winning); a stem such directories took most files of was a word the
businesses share, not one of its own, and is no area. A first word of one or two letters is an
abbreviation: it takes the words after it up to a generic, layer or role word (`ctl`, `query`,
`impl`, …) - `EcPayChatService` is `EcPay`, `PmCtl` is `Pm`, two letters alone name nothing. An
area of twelve code files or more is split by the same rules - a business by its longer stems
(`invoice/line`), never a layer's whole directory; a split that finds one part holding the
code (a package's sources one directory down, beside a build script) looks inside that part
instead, twice at most - and so are its parts, **largest first**: the area holding the most code
splits, then whichever area or part holds the most of what is left, while the map has room, so
how much code an area holds decides how deep the map goes there (a 200-file business reaches
its fourth level, a 20-file one stays whole). The rules draw four levels at most and the
review's new parent takes the fifth, the deepest a map goes (§memory.domains). At most 14
top-level areas besides 48 businesses - the largest by the files they own, the directories
named for them included (`Pm` names a few classes, its `web/res/pm` and `remote/pm` many more)
- (the review groups those, so it sees them all), 10 under one, 150 in all - a top level past
that loses its smallest businesses, and the last split that fits keeps its largest parts - each
area costing one question. Paths are slugs of the directory names, or of the shortest spelling a stem's files
use (the plural fold joins `job` and `jobs`, and never renames `redis`), unique among siblings,
a common prefix (`swiss-`) stripped. **Relations** between areas are the names one area's files
use that another defines (`via` the names) and the imports that resolve into another area; a
relation inside one family is drawn at the level both share. A name is matched by its spelling
alone, so a use counts only within one language, and from one package into another only when
the first one's manifest declares the second - Cargo's dependency tables (a target's
included, the workspace's own table not), a `package.json`'s four, `go.mod`'s `require`, a
`pom.xml`'s `<dependency>` artifacts; another manifest gates nothing. A name the mentioning
area defines itself is its own, not a use of the other definition.

**The review.** The rules know a layout, not the business: they keep technical layers side by
side, cut one business across directories, name an area after a folder. So before any area is
described, a draft with three or more new top-level areas goes to the model once - the lead when
a layer names one (§memory.init The lead), the workers otherwise (§memory.settings): the project's
README head, the top-level paths the map has, and each new area's kind, file count, languages,
`owns`, manifest description, sub-areas, 12 file names, 8 most used names and the areas it uses.
The answer is `{domains: [{path, title, members, merge}]}` (a schema for the runtimes that take
one): each entry one business domain over `members`, paths of that top level. One member is a
rename; several with `merge` are one area the rules cut apart (an entity's controller, service and
mapper), which becomes one area owning all their code - or, when their `owns` together pass the
30 entries a domain holds, sub-areas of a new parent, so no code is cut off; several without
`merge` become sub-areas of a new parent. Technical areas (configuration, scripts, database files) stay apart unless they
serve one domain alone; an area no entry names stays as it is. **The check** drops, with why, a
path that is not one lowercase English segment, one the map or another entry has, a member that
is not a new top-level area or is named twice, an entry left with no member, and a parent named
like its member; titles are clipped and redacted. The review regroups and renames; it never
draws an area of its own, splits one or moves a file. The draft is then drawn again with the
groups (`Group` areas, each standing where its first member stood; a merged area keeps its
members' sub-areas, a new parent owns nothing itself), and only then are the areas described. A
review that is not an object with a `domains` list is asked once more, told why; a second miss,
or a runner error, keeps the rules' draft and the summary says so. A review with no entry is a
valid answer: the top level already reads as the business.

**The question** for one area opens with the sync marker (§memory.sync: a conversation holding
it is never read back) and carries, bounded: what the area is (its kind, its file count and
size, its languages, what it owns, its parent), its manifest's name and description, its
children as already described, the project's README head (top-level areas) and the area's own,
its files with the names they define (30 files, the ones whose names are used most), its entry
files, the 15 names other files use most, its relations (8 each way), and the first 60 lines of
three files. Every text is redacted (§memory.sync) and credential-named files are never opened.
The answer is one JSON object: `{title, summary, overview, concepts: [{name, meaning}],
entryPoints: [{name, role}], flow: [step], code: [{name, role}], data: [item], dependsOn:
[{area, why}], keywords: [term]}`, with a schema for the runtimes that take one. **`language`**
(a tag: `en`, `zh`, `zh-TW`, `ja`, …; 2..16 letters or `-`) says what the hub is written in;
any but English also asks for every keyword in English and in that language, so either finds
the area (§memory.search). Children are asked before their parents - each level at once, the agent's
`concurrency` questions in flight (§llm.agents: 4 for `builtin` and `chat`, 1 for a spawned
runner, unless it sets its own) - and a parent is told
what its children turned out to be. A spawned runner answers with no tools (`claude`: every tool
off; `pi`: `--no-tools`).

**The check** takes nothing on trust: every field is clipped (the title to 80, the summary to
200, the overview to 900 and an item to 240 characters; 8 concepts, 6 entry points, 6 steps, 10
code items, 6 data items, 6 dependencies), redacted, and every path or name in `entryPoints`
and `code` checked against the index - a listed file, a unique file of the area ending in it, a
directory of the area, a name it defines, or a route (`/api/x`, `POST /api/x`) - the rest
dropped and counted; `dependsOn` keeps only areas of the draft. An answer that is not an object
or has no title is asked once more, told why; a second miss leaves the area its index outline.

**The hub** is assembled by the program from one template - Overview, Concepts, Entry points,
Flow, Code (each name cited with the file that defines it, the index's most used names where
the answer is silent), Data, Relations (the draft's, with what each `dependsOn` said) - and the
area's `owns` are its directories or files. The proposal creates every area not on the map yet,
parents first, through the same apply as a sync run (`Actor {agent, "sync"}`, unconfirmed, into
the review inbox), past the proposal's 20-domain limit. An area whose path the map already has
is **kept** - never asked about, never rewritten - and an area whose code the map already owns
is left out, so a second map run adds only what is new. The model never draws the map and
never writes a hub.

**The run** is a sync run with `trigger: "map"` (§memory.sync's queue, worker and runs), its
scope saying the `language`, the `units` drawn, the `asks` made and how many answers `filled` a
hub, saved as the levels finish so the run's progress shows while it runs. With no agent set the
run asks nothing and draws the skeleton. When questions were asked and none filled a hub, the
run fails and writes nothing; otherwise the areas that failed keep their outline and the summary
names them. A dry run keeps the proposal and writes nothing. A map run takes no `since`. A
project that has never synced is **baselined** by its first applied map run - HEAD, and every
transcript at its current size - since the map now matches the code as it is; a project that
has synced keeps its head. The run's scope also keeps the review as checked (`regroup`).

**Redraw.** A map drawn before the rules or the review improved stays as it was: areas on the map
are kept. `redraw` (a map or full run) first removes the domains an earlier map run drew and
nobody touched since - drawn by sync, never confirmed, every revision sync's, and every
sub-domain removable too - deepest first, through the ordinary delete (§memory.domains: notes
move to the parent or go unfiled), then draws the map as if they were never there, and refiles
the unfiled notes by their anchors (autofile) so they follow the new map. A map run that draws a
domain files them too: a note saved before its code had a domain follows the new one
(`memory_sync::tests::map_run::a_map_run_files_the_notes_saved_before_their_code_had_a_domain`). A domain a person
created, edited or confirmed is never removed, and neither is its parent. A dry redraw removes
nothing and its proposal is drawn with those domains set aside; the scope keeps `redraw` and the
paths `removed` (would remove).

**Entry points.** `swiss memory init --map [--redraw] [--dry-run] [--wait] [--lang TAG] [--json]`
registers the project as `init` does, then queues the map run on the gateway (`POST
/api/knowledge/sync` with `map: true`), `--wait` following it to its end; `init --map --plan
[--redraw]` prints the areas, the files they own, whether the review will be asked, what a redraw
removes and how many questions a run would ask, offline and asking no model. The panel's sync
sheet has the same under **Map and initialize from the code** (a preview, the language, a dry
run, Redraw) (`survey::tests::*` - among them `manifests_say_what_they_depend_on`,
`a_name_spelled_alike_is_no_relation_across_languages_or_undeclared_packages`,
`files_a_repository_marks_generated_or_vendored_are_no_area`,
`an_area_whose_code_sits_one_directory_down_is_split_inside_it`,
`a_java_project_is_cut_by_business_under_its_package_path_and_its_sql_is_an_area_beside`,
`a_review_regroups_the_top_level_into_business_domains_and_its_mistakes_are_dropped` -
`init::tests::a_redraw_removes_only_what_sync_drew_and_nobody_touched`,
`a_map_run_draws_the_domains_and_starts_the_sync`,
`a_map_without_a_runner_is_the_skeleton_and_a_dry_one_writes_nothing`,
`a_redraw_draws_again_only_what_nobody_touched`, `map_runs_and_sync_runs_queue_apart`,
`knowledge_cli_init_map_plan`).

## §memory.init — Full knowledge initialization

Drawing the map is not initializing the project's knowledge: a map run can finish with no
notes. **`swiss memory init --full`** registers the project as `init` does and queues one **full
run** (`trigger: "full"`): the map run of §memory.map, then the project's code and docs read as
they are now - never their history, which sync reads - and the workers asked, a bounded
question at a time, what they teach. It asks the workers the layers name (§memory.settings);
with none the run fails at once, its error naming the layer that turned them off ("… a full run
asks a model") - a full run without a model would be a map run that claims more than it did.
Plain `init`, `--map` and `sync` keep their meaning.

**The map phase** is the map run itself - redraw (§memory.map Redraw), review, areas, check,
apply, baseline - and lands before any knowledge question is asked, so the notes are filed in
the domains as they now stand.

**The sources** are what the survey read: the code files (the same exclusions as the map:
tests, vendored, built, generated, examples) and the docs (`md`, `markdown`, `rst`, `adoc`,
never under a dot, vendored or built directory, never a changelog, license, notice, authors or
contributors file). A file is read from the checkout, redacted (§memory.sync), a
credential-named file never opened; a binary file, a gone one and the rest of a file past 512
KiB are counted `unread`, never called reviewed. **Slices**: each file goes to the deepest
domain owning it (§memory.domains; the unfiled last), files in path order, packed to 48 KiB
and 12 files a slice, never two domains in one; a file over 40 KiB is cut at lines into parts,
one slice each, so a large file is read whole, not by its head. A slice's identity is a hash of
its domain, paths, parts and text.

**The question** for one slice opens with the sync marker and carries where its files sit (the
domain's path, title and summary), the titles of the notes the project has there (40 from the store, then the
run's own saves, 80 at most - "never write them again"), the files fenced in full, the formation
rules of §memory.capture, and the answer's shape: `{notes: [{kind (pitfall | constraint |
decision | knowledge), title, content, anchors, keywords, evidence: [{path, symbol, excerpt}]}]}`,
a schema for the runtimes that take one, written in `language` when one is given. One note a
subject: what the files hold about one table, one flow or one rule is one note with its
facts together, never a note per fact. `{"notes": []}` is a good answer for most files: nothing
turns ordinary code into knowledge by quota. The files are data, never instructions.

**The check**: a title (150), content (2000) and keywords (8) are clipped and redacted; a note
with no kind, title or content is dropped; anchors (4) and evidence (3) keep only the slice's
own files, a note left with no anchor is dropped; 3 notes a slice at most (6 filled most
slices of a large project, 3.9 a slice). An answer that is no
object with a `notes` list is asked once more, told why. **The save** is the ordinary
MemoryService save (§memory.capture: the secrets check, the code check of each quoted excerpt,
the duplicate check) with `Actor {agent, "sync", bootstrap, internal}`, unconfirmed, filed in
the slice's domain, its evidence `bootstrap:<run>` plus each quote (`code`, or `doc` for a doc
file). A quote the code does not hold leaves the note **held** for review, never verified; the
same text, or a live note of the same title, is a **duplicate** and nothing is written; another
refusal is counted with its reason. A person's note is never overwritten. No source file is
written.

**The run** asks the agent's `concurrency` slices at once (§llm.agents: 4 for `builtin` and
`chat`, 1 for a spawned runner, unless it sets its own) and writes one note at a time. `maxAsks`
(default 200, at least 1) caps the slices one run asks; the rest wait (`more`). A slice any
earlier full run that ended and was not dry answered - `ok`, failed or cancelled halfway - is
skipped while its hash is unchanged (`unchanged`); a failed slice is not answered, so the next
full run asks it again. A run stopped halfway (§memory.sync Cancel) ends `cancelled` with what
it saved. The scope's `knowledge` counts `files, bytes, slices, asked, unchanged,
more, failed, proposed, saved, held, duplicates, refused`, keeps the `done` hashes, and is saved
as each slice finishes so the panel shows the run's progress. When slices were asked and none
was answered the run fails ("the map landed, but the runner answered none of the N slice(s)");
otherwise it ends `ok`, its summary naming the map's outcome, the notes saved, held and known,
what waits, what failed and the first refusal. A project with no source and no area is
`empty`. A **dry run** draws the map's proposal without applying it, cuts the slices against the
map it would leave - the map as it is, less what a redraw removes, plus the new areas; `GET
sync/full` the same - counts what a real run would ask and asks no knowledge question.

**The lead.** The slices go to the workers by code (§memory.settings) - a bounded question
each, at its own effort and `concurrency`; the judgement goes to a second agent, the lead a
layer names (`sync.lead`, §memory.sync), set
for more thought (`effort: high` or `max`, §llm.agents). The lead answers the map's review
(§memory.map) and, once the slices are read, **wraps up** each domain the run saved a note in
(`phase: wrapup`): one question carrying the domain's hub (path, title, summary, keywords, body),
the notes this run saved there ("never write them again"), its files (150 listed) and the
formation rules, answered as `{summary, keywords, notes}`. **The check**: the summary is clipped
(200) and redacted, kept only when it says something the hub does not; new keywords (8 at most)
join the hub's, case-insensitively, up to its cap; the notes pass the slice check against the
domain's files, 3 at most, the rest dropped with why. The hub's summary and keywords change
through the ordinary domain update at the revision read (its body untouched, a person's edit in
between winning), and the notes go through the save above, filed in that domain. A builtin lead
runs its own loop with `read` and **`delegate`**: up to 8 tasks a call, each a sentence, up to
12 checkout files inlined (96 KiB in all) and an `effort` of its choosing (`low`..`max`, absent
the worker's own), handed to the workers - a builtin or chat one; a spawned runner takes no
task and the lead goes without - at the worker's `concurrency`; each answer comes back cut at
8 KiB, in the order asked. When the workers are several, a task may name the one that takes it
(`agent`, an enum of the roster the lead's addendum lists: each worker's name, its model and
its seats, §memory.settings The lead's roster); left out, the freest takes it, and a name no
worker holds is a tool error listing them. Another runtime leads with one bounded answer and
no delegate. The
domains are wrapped up at the lead's `concurrency`; an answer the check refuses is asked once
more, told why, and a second miss is counted, the domain left as the slices left it. The
scope's `wrapup` counts `domains, wrapped, hubs, saved, held, failed`, the
summary adds `the lead wrapped up W of D domain(s): H hub(s) refreshed, N note(s) saved`, and
the run's `runner.lead` names it. A run stopped during the wrap-up keeps every slice's notes and
the domains already wrapped up (`cancelled while the lead wrapped up W of D domain(s)`); no lead,
or no note saved, is no wrap-up (`init::tests::a_wrapup_refreshes_the_hub_and_keeps_notes_that_span_the_domain`,
`a_lead_reviews_the_top_level_then_wraps_up_each_domain_the_run_wrote_in`,
`a_stop_during_the_wrapup_keeps_what_the_slices_saved`,
`a_lead_delegates_tasks_at_the_effort_it_picks_with_their_files`).

**The recheck.** A note a run saves is unconfirmed until judged; a recheck judges it against
the code so initialization ends with notes that were read twice, not a pile awaiting review.
**`swiss memory init --recheck`** queues one (`trigger: "recheck"`), and every full run closes
with one (`phase: recheck`, after the wrap-up). It judges each **judgeable** note: live, written
by a model (`origin: agent`), confirmed by nobody, and never verified since - or held for review
again since. A person's note, a confirmed one and one a recheck already verified are passed by.
A recheck run first files the unfiled notes by their anchors (§memory.domains autofile). **The
questions**: a domain at a time, the unfiled last, each domain's notes in the order of the file
they first name; a question takes notes while it holds fewer than 20 and the code of their
files fits 128 KiB, each file read once from the checkout and shown whole up to 48 KiB (a note
alone in its question shows its first file whatever the budget). A longer file shows 48 KiB of
lines around what its notes name - a note on a method deep in a long class would otherwise sit
past the cut and never be judged: the code words the notes to judge name (lower camelCase or
holding an underscore - a method, a field, a constant; a type's own name sits on every import
and is left out), each found at 3 places at most, declarations first, a window of 8 lines
before and 40 after each, after the file's first 12 lines, taken a note at a time in turn while
they fit; each run left out is marked (`⋯ lines A-B not shown ⋯`) and the heading says which
lines of how many it shows. A long file none of those words is in shows its first 48 KiB, cut
at a line and said so. A line past 2 KiB - a dump's one-line `INSERT`, a minified bundle - is
shown in pieces: its first 256 bytes and a piece around each place one of those words sits in
it (384 bytes before, 1 KiB after, each word's first 3 places, a word at a time in turn), 6 KiB
of it at most, each stretch left out marked `⋯ N bytes not shown ⋯`; a whole-word match only, so
`kyc_fail` is not found inside `xkyc_fail`. A marker a model copies into its evidence with the
code around it reads as an elision (§memory.code `excerpt_mismatch`), never as text the code must
hold (`a_line_past_two_kib_shows_its_head_and_the_stretches_around_the_names`).
Each question is one request to the workers (§memory.settings; the workers alone, no lead), at
the pool's pace - the lead does not take part, since the code the
notes name is in the question and nothing is left to read. It carries the domain (path,
title, summary), each note in full - its id, revision, kind, title, files (a file not shown
marked so), content and, held, why - the domain's other live notes (80 listed, those sharing a
file first) as what a note merges into or links to, its files (150 listed), the code, the
formation rules and the answer's shape:
`{verdicts: [{id, verdict (keep | fix | drop | merge), into?, kind?, title?, content?,
anchors?, evidence?: [{path, symbol, excerpt}], reason?}], links: [{from, to, kind, note?}]}`,
a schema for the runtimes that take one, written in `language` when given; the model judges
from the code shown and gives no verdict where it was not shown. **The check**: a verdict
names a note this question judges and shows a file of (the first per note wins), the rest
dropped with why - an id the question does not name is read as the one id it names a single
slip away (a letter dropped, added or changed; a model copying a dozen hex characters slips
one now and then), and as none when two are;
a fix keeps only what changes (title 150, content 2000, clipped and redacted), evidence (3)
and anchors (4) only checkout files; a fix that changes nothing is a keep - unless the note is
held and the fix quotes the code, which re-checks the text as it stands against the new quotes;
a merge goes into another note of the question that stays; a link (30 at most, no system kind)
joins two notes that stay. An answer with no `verdicts` list is asked once more, told why.
**What it comes to**, as the init actor, each operation alone (one the store refuses is counted
with why, never the rest), the revision reason `recheck <run>: <reason>`: a **keep** is the
verified mark; a **fix** is the ordinary update at the revision the question showed, its
evidence `bootstrap:<run>` plus each quote, then the verified mark; a **merge** supersedes the
note by the one that says it (§memory.model `supersede`); a **drop** marks it wrong with the
reason; then the links. **A held note** - the code check holds what it quotes - is released only
by a fix whose quotes the code holds; a model's keep never releases it, and it stays for a
person (`held`), judged again by the next recheck. **The contradictions first**: a
`contradicts` link between two of the model's notes neither of which is confirmed, one of them a
note the recheck judges, puts both sides in their own question, a group each (a group spans every
note one chain of links reaches), before the domains' - the prompt opens on settling the
contradictions, lists the links under *The contradictions* and asks which side the code bears out;
a side checked before is judged beside the other, and a note asked there is listed among the
others of its domain's question, not judged twice. Each `contradicts` link of a question whose
both sides the answer left verified - a keep, or a fix that landed then the verified mark - is
lifted, unless the answer links them as contradicting again. **It settles what its answers
raise**: after its planned questions the run looks again at the contradictions between two of the
model's unconfirmed notes, one of them a note an answer judged or linked as contradicting, and
asks each that still stands side by side, in the same run - up to 3 rounds, a pair at most twice
(the second time each side's question says it was judged beside the other earlier in this run and
must be settled now; linking them again leaves it for a person), within `maxAsks`; a note a round
asks again counts again in `notes` and in its verdict's count. A contradiction with a person's
note or a confirmed one, or between two notes the run neither judged nor linked, waits in the
review inbox (§memory.freshness), where a review run takes it on; a recheck confirms nothing.
`maxAsks` (default 200, at least 1) caps the
questions a recheck run asks, the rest wait (`more`); a full run's closing recheck asks 200 at
most. A **dry run** counts the notes, domains and questions and asks none (`would recheck N
note(s) in D domain(s): Q question(s), M for a later run`). The scope's `recheck` counts
`domains, notes, questions, asked, failed, more, kept, fixed, dropped, merged, linked, held,
refused, left, lifted, rounds, open` (`left`: notes an answer gave no verdict; `lifted`: the
contradicts links lifted; `rounds`: the rounds asked after the planned questions; `open`: the
contradictions between the model's notes still standing at its end), saved as each answer lands;
the summary says `rechecked J of N note(s) in D domain(s): K kept, F fixed, X dropped, M merged, L
link(s) added; C contradiction(s) lifted (R more round(s) for the contradictions the answers
raised); O contradiction(s) between the model's notes still open` (each of the last three only
when not zero, the lifted ones also with a round) and what still needs review or waits. A stop keeps every answer landed
(`cancelled while it rechecked …`); a recheck whose every question failed fails; one with no
judgeable note is `empty` ("no note waits for a recheck"). It asks a model: with no workers
named (§memory.settings) it fails at once, its error naming the layer that turned them off
("… a recheck asks a model")
(`recheck::tests::*` - `a_recheck_judges_a_domain_at_a_time_and_passes_by_what_a_person_wrote_or_vouched_for`,
`a_recheck_asks_the_sides_of_a_contradiction_between_the_models_notes_together`,
`a_question_shows_the_code_its_notes_name_and_closes_before_it_runs_past_the_budget`,
`a_long_file_shows_the_lines_around_what_its_notes_name`,
`a_verdict_one_slip_from_a_note_id_judges_that_note_and_one_from_two_judges_neither`,
`a_fix_that_only_quotes_the_code_rechecks_a_held_note_and_keeps_any_other` -
`a_recheck_keeps_fixes_merges_and_drops_what_the_code_bears_out_and_links_the_rest`,
`a_lead_reviews_the_top_level_then_wraps_up_each_domain_the_run_wrote_in`,
`a_recheck_settles_the_contradictions_between_the_models_notes_and_leaves_a_stubborn_one_open`,
`a_full_runs_closing_recheck_settles_the_contradiction_its_answer_raises`,
`a_recheck_request_queues_a_recheck_beside_the_other_runs`).

**The review run.** A person may hand the review inbox (§memory.freshness) to the sync agent:
**`swiss memory review --resolve`** queues a review run (`trigger: "review"`, `review: true`),
a recheck whose reach is the inbox - every live note that carries a marker, regardless of origin,
confirmed ones too, since an anchor that changed or a contradiction puts a confirmed note back
in the inbox. Human notes are judged, rewritten or confirmed by the full review too. Each note's
question says why it waits besides a hold - the file that changed since it was checked, the
note it is linked as contradicting, the note it depends on and what became of it, the 180 days nobody checked it.
**The contradictions first**, as a recheck's, between any two of the judged notes - confirmed
ones too. Everything else is the recheck's: the questions, the check, the caps, the
operations, the held note a keep never releases, reasons `review <run>: <reason>`. **What it
adds**: a note the answer leaves verified - a keep, or a fix that landed then the verified mark -
is confirmed at the revision that mark wrote, as the init actor, with the reason `review <run>:
the code bears it out; confirmed on the person's request` (a note confirmed already is passed
by); contradicts links are lifted when both judged sides are verified, including human notes.
A held note stays held and unconfirmed. **It settles what its answers raise** as a
recheck does, reading the inbox again instead: an answer that links a note as contradicting a
confirmed one outside the inbox puts that one in it, so each contradiction between two of the
notes in the inbox that still stands is asked side by side - the same rounds, the same
pair limit, within `maxAsks`. The
scope's `recheck` counts add `confirmed`; the summary says `resolved J of N note(s) …; C confirmed, L contradiction(s) lifted (R
more round(s) for the contradictions the answers raised); K contradiction(s) between the
model's notes still open` (the last two only when not zero) and what still waits for the next
review; a dry run says `would resolve N note(s) in D domain(s): Q question(s), M for a later
run`; an inbox with no pending note
is `empty` ("the review inbox holds no note waiting for review"). It
takes `language`, `maxAsks` and `agents` - the workers alone, never a lead - and refuses `since`,
`redraw` and any other run kind beside it; a second request merges into a queued one
(`recheck::tests::the_inbox_judges_what_a_model_wrote_and_asks_the_sides_of_a_contradiction_together`,
`a_review_run_judges_the_inbox_confirms_what_it_keeps_and_settles_a_contradiction`,
`a_review_run_settles_the_contradictions_its_answers_raise_and_leaves_a_stubborn_one_open`,
`knowledge_a_review_run_confirms_what_it_keeps_and_lifts_a_contradiction_it_settles`).

**What changed alone.** `changed: true` beside `review: true` (`review --resolve --changed`, the
panel's **Review what changed**) narrows the
reach to the inbox's notes a model wrote that carry `anchor_changed` - a file each names changed
or went since the note was last checked - and records `scope.changed`. Their question says the
code changed and asks for the same verdicts; they are confirmed as a review confirms them. A
contradiction is no reason to judge a note here and is not settled here: its sides wait for a
whole review, and the notes the answers link as contradicting settle as a recheck settles them.
A note left verified is checked again at the file as it is now, so the next changed review passes
it by while its files hold; with no such note the run is `empty` ("no note a model wrote names a
file that changed since it was last checked: nothing to review"). It waits in the review queue:
a whole review asked while it is queued takes it over (the whole inbox holds the changed notes),
a changed request merges into a queued whole one and leaves it whole, and `changed` with any
other run kind is refused. The summary says `reviewed J of N note(s) whose code changed …; C
confirmed`, and `reviewed J of N note(s) - whose code changed, or an answer set against one - …`
once settle rounds judged the notes an answer set against a changed one (N counts every
question's notes); a dry run `would review N note(s) whose code changed in D domain(s): …`
(`recheck::tests::a_changed_review_judges_only_the_notes_whose_files_changed`,
`memory_sync::tests::recheck_run::a_changed_review_judges_the_notes_whose_files_changed_and_the_clock_queues_one_a_day`,
`memory_sync::tests::recheck_run::a_changed_review_counts_its_settled_notes_apart_from_the_changed_ones`).

**Embedding.** The notes go through the ordinary save, so the embedding indexer of
§memory.embedding picks them up like any note - there is no initialization-only embedder, and a
hub is no note. The run's `knowledge.embedding` keeps the index as the run ended: `state` (`off`
| `ready` | `indexing` | `stale` | `error`), `model`, `indexed`, `pending`, `error`. `off` is a
valid outcome - the notes are found by their words (§memory.search) - and is said as such, never
as a failure; `pending` is not `indexed`.

**Entry points.** `swiss memory init --full [--redraw] [--dry-run] [--wait] [--lang TAG]
[--agent A[,B…]|off] [--lead L|off] [--max-asks N] [--json]` queues the run (`POST
/api/knowledge/sync` with `full: true`); `--wait`
follows it to its end, prints the knowledge counts, then follows the index while notes are
pending (ten minutes at most; the gateway keeps indexing after) and says its state. `init --full
--plan [--redraw] [--max-asks N]` asks the gateway what the run would do (`GET sync/full`): the
map plan, the slices, how many it would ask, skip and leave, and the index - no model asked.
`swiss memory init --recheck [--dry-run] [--wait] [--lang TAG] [--agent A[,B…]|off]
[--max-asks N] [--json]` queues
a recheck (`recheck: true`); `--wait` follows it to its end and prints its summary. Two of
`--map`, `--full` and `--recheck` together, `--max-asks` without `--full` or `--recheck`, a
`--max-asks` under 1, `--plan` or `--redraw` with `--recheck`, and `--plan` with `--dry-run`,
`--wait`, `--lang`, `--agent` or `--lead` are usage errors, as are `--agent` or `--lead`
without a run flag, `--lead` with `--recheck`, and a name off the llm rule or named twice
(refused before `init` registers anything, §memory.settings). `swiss memory review --resolve
[--changed] [--dry-run] [--wait] [--lang TAG] [--agent A[,B…]|off] [--max-asks N] [--json]` queues a review
run (`review: true`, with `--changed` the notes whose code changed alone); those flags without
`--resolve`, `--lead`, and a `--max-asks` under 1 are usage errors. The panel's sync sheet has
**Preview initialize**, **Initialize memory**, **Recheck notes**, **Resolve the review inbox** and
**Review what changed** beside the map's controls (§memory.panel).
Full initialization is idle unless asked for: no watcher, no new dependency, no peer-crate edge
(`init::tests::*` - `sources_are_sliced_by_domain_and_a_long_file_in_parts`,
`an_answer_keeps_notes_anchored_in_the_files_read` -
`a_full_run_draws_business_domains_then_saves_what_the_code_teaches`,
`a_full_request_queues_a_full_run`, `a_recheck_request_queues_a_recheck_beside_the_other_runs`,
`knowledge_cli_init_map_plan`; live, `.env`-gated:
`e2e/memory-init.test.mjs`).
