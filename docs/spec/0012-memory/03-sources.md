# Code and documents

Part 03 of [§memory](README.md), chapter 0012 of the [swiss specification](../../SPEC.md).

## §memory.code — The code index

A note names code; the checkout says whether it is there. The code index (ADR-032 phase 5) is a
derived map of a checkout, kept apart from the memory graph in its own
`<SWISS_HOME>/memory/code.sqlite3` beside the store (the store's open options, its own schema
number in `code_meta`: another number is dropped and rebuilt, never migrated; 2 marks the
references only a comment makes). It is never
exported, imported or backed up, and deleting it - like deleting every vector - loses no note.

**What it holds.** Per root (`code_roots`, keyed by the root's display path; when it was indexed,
how many files were listed, whether the listing was cut, whether Git listed it): every listed
file (`code_files`: path, size, mtime, `sha256:` hash, language, and why it was not read), the
names a file defines and so contains (`code_defs`: name, kind - fn, method, struct, enum, trait,
type, mod, const, static, union, macro, class, interface, var, namespace, record, object - and
line), the modules it imports (`code_imports`: the spec as written and its line) and the
code-like names it references anywhere in its text (`code_refs`, each marked `comment` when
only a comment of the file holds it); each name is held once (`code_names`).

**Listing and reading.** The host lists a root (the `Lister` the gateway and the CLI install;
swiss-memory never runs git): git's tracked and unignored files (`ls-files --cached --others
--exclude-standard`, 8 MiB of output), or outside Git the previews' bounded walk (20 000 entries,
§memory.preview). A cut listing marks the index `truncated`. Definitions are a keyword scan line
by line - the ctags way, no grammar - for Rust, TypeScript and JavaScript, Python, Go,
Java-family (Java, Kotlin, Scala, C#: its types, and an indented `Type name(args)` - past
annotations, modifiers and type parameters, a dotted, generic or array return type - closed by a
body, `;`, `throws`, `where`, `=>`, `default`, or the line's end, as a method; a call, an
assignment, a constructor and a statement word are none), Ruby, PHP, shell, PowerShell and
C/C++; imports are what
`use`, `import`, `from`, `require`, `#include`, `Import-Module` or a dot-sourced script name;
references are the code-like words (§memory.search's identifier test) of 3 to 64 characters, up
to 5 000 a file. In a file of a listed language a name only its comments hold is a mention,
not a use, and is marked so: `//` and `/* */` (Rust, TypeScript and JavaScript, Go,
Java-family, PHP, C/C++), `#` (Python, Ruby, PHP but `#[`, and shell and PowerShell where it
starts a word - not `$#` or `${#`), PowerShell's `<# #>` and Ruby's `=begin`. A string literal
is code (a key in quotes is used), a char literal's quote opens no string and a Rust lifetime
no literal; the lexer is that small, so the odd raw string or regex it misreads stays code, as
every mention was before. A text file of no listed language is read for references only, none
marked. What the scan
misses (a destructured binding, a grouped Go `const (...)`, a C function) is simply no
definition; its references still hold the name, so the check never calls it absent.

**Refresh.** Built or refreshed only when asked (`swiss memory code index`, `POST
/api/knowledge/code/index`), by a save's miss (below), or by a find (`code find`, the asker's
`code` tool) on an index older than the lister's gap (30 s) - one that fails to refresh answers
as it is: no daemon, no watcher. The scan has a version (`code_meta.extractor`): an index a
different scan read keeps its facts but marks every file for reading again, and its next
refresh - due at once, whatever its age - reads each one. A file whose
size and mtime are unchanged is not opened; a changed one is hashed, and one whose hash is
unchanged is only re-stamped (`touched`); a file gone from the listing is removed with its
facts, and names no file holds any more are pruned. Files are read off the async runtime in
batches of 256, each batch written in one `BEGIN IMMEDIATE` transaction. Never read for names,
only listed with the reason: a file whose name says it holds credentials (`sensitive`, the
list §memory.preview refuses, `secrets::sensitive_path`: never opened, so never hashed); over
1 MiB (`large`); data and generated text (`data`: `.jsonl`, `.ndjson`, `.map`, `.svg`, `.csv`,
`.tsv`, `.lock`, `.log`, `.snap`, `.min.js`, `.min.css`, `.pem`, the npm and pnpm lock files);
binary (`binary`: a NUL in its first 8 KiB). Every other file's text passes `secrets::redact`
before a name is taken, so a credential-shaped token never becomes a name; a redacted span keeps
its line breaks (`redact_lines`), so a definition past a private key block is at its own line.

**The save check.** A new note in a project under a root is checked before its transaction
opens (a global note, or a project with no directory, is not). The title and content yield at
most 60 claims: a **path** (a `/` and a code or document extension; `:line` and `#anchor` cut;
a bare file name claims nothing), a **code name** (an inner `_`, a `$`, lowerCamel, or a
code-like name in backticks) and a **plain** one (any other code-like word, or an identifier in
backticks); `a::b` and `a.b` are judged by their parts. Prose is never code: a plain name is
never called unknown. The rules:

- `missing_anchor` - a file anchor that is not a file under the root (read live: needs no
  index), or a glob that matches no indexed file;
- `missing_file` - a named path that is neither indexed (as itself, or as the tail of an
  indexed path) nor on disk;
- `unknown_name` - a code name in neither the anchored files' text nor any indexed file's
  definitions or references, and that begins no family of names in the anchored files (a
  key family's stem: `shop_email_coupon` for `shop_email_coupon_name_1..9`, cut at an
  `_` or before a digit; one longer name alone is no family);
- `elsewhere` (advisory) - in a note with anchors, a name the anchored files do not hold,
  defined in one to five other files, which the finding lists;
- `missing_evidence` - a code, test or doc evidence path that is not a file under the root
  (read live: needs no index);
- `excerpt_mismatch` - an evidence excerpt its file does not hold: whitespace runs compare as
  one space, a string literal's escapes as what they stand for (`\"` as `"`, a written `
` as
  a space - a model quoting a SQL string drops or doubles an escape level), and an elision
  (`...`, `…`, or a recheck's `⋯ lines A-B not shown ⋯` / `⋯ N bytes not shown ⋯` marker copied
  with the code around it, §memory.init The recheck) or a `[redacted:<rule>]` marker (a value
  the model was never shown) splits it into parts found in order. The file is
  read live; one whose name says it holds credentials, or over 1 MiB, is never read, so never
  judged;
- `unscoped_quantifier` - a claim over a set (§memory.capture) that names no set and no file
  and is not scoped; a closed set with no set named is counted by a code-like name instead,
  and a count whose number an evidence excerpt states is backed by the quote;
- `count_mismatch` - a claim over a set the checkout counts otherwise. A set's members are the
  indexed files its glob matches or that lie under its directory (over 300, not counted), read
  live for the names the claim gives in backticks: `exactly N` (or `only`, `just`) is the
  members holding every name; `all N` the set's size; `all` and `every other` a member, not
  one the note names, lacking a name; `most` more than half holding them; a closed set, a file
  holding them that the note neither anchors, names nor cites and that defines none of them -
  with no set named, over the index's every mention of its code-like names. The finding lists
  up to 8 files.

Without an index only anchors are judged; a `truncated` index judges nothing absent. A miss
refreshes an index older than the lister's gap (30 s) once, then judges again (`index:
"refreshed"`). A failure of the index is said in `error` and never fails the save.

**Hold.** Every rule but `elsewhere` holds a note whose producer is not a person
(§memory.model): it is saved as `needs_review`, in the review inbox, its create revision's
reason `held for review: <rule> <name>, …`. A person's note (the panel, `--producer human`) is
told and saved active; a CLI save without `--producer` is `unknown`, so it is held. A save
answers `code: {index: none|checked|refreshed, indexedMs, names, findings: [{rule, name,
message, files}], held, error?}` - absent for a global note and for a duplicate; MCP returns it
only when it found something, beside the note's `status`; the panel's toast names what the
checkout lacks.

**Release.** The model fixes its own note; no person confirms it note by note. An update that
changes the kind, title, content, anchors or refs is checked like a save - the text it leaves,
with the evidence it carries - before its transaction opens, and answers `code` too (MCP and
the API when it found something, MCP beside `status`; the CLI prints each finding): a held
edit holds the note, its revision's reason saying why; a passing one makes a note the check
held (its latest revision's reason `held for review: …`, not an outdated mark) `active` again,
reason `released: the code check passes`. Evidence stays bound to the text it came with, so a
note held for `excerpt_mismatch` or `missing_evidence` is released only by an edit that changes
its title or content, or that cites a file the check then reads (a corrected quote); an edit that
does neither keeps it held and repeats why. A verified mark accepts
a held note from a person or a CLI caller who declares nothing; from an agent, a bootstrap or a
sync it is refused (`memory_invalid`, the findings in `check`) while the check still fails, and
always for a held quote - a model fixes its note, it does not vouch for it.

**Reads.** `swiss memory code index | status | find NAME | file PATH | drop [--all]` and `GET|DELETE
/api/knowledge/code` (status; drop, `all=true` every root's), `POST code/index`, `GET
code/find` (`name`: where it is defined, kind and line, and up to 50 files that reference it -
those whose code names it first - with the total, and which of them name it only in a comment:
`inComments`, with `inCommentsTotal` of the total; `code find` and the asker's `code` tool count
both and list those files apart, under `only a comment names NAME in these files; they do not
use it:`), `GET code/file` (`path`, exact or a path's tail: language, size, hash, why it
was skipped, its definitions, imports and reference count) - each with the `project` selector,
the global scope refused (`code_index_is_derived_rebuildable_and_hash_aware`,
`code_index_holds_no_credential`, `code_check_holds_a_note_naming_code_that_is_not_there`,
`code_check_without_an_index_checks_anchors_only`,
`code_check_refreshes_a_stale_index_before_it_holds`, `code_check_holds_an_excerpt_its_file_does_not_hold`,
`a_newer_scan_reads_every_file_again_before_a_find_answers`,
`code::extract::tests::java_family_methods_are_definitions_their_calls_are_not`,
`code::extract::tests::a_name_only_a_comment_holds_is_a_mention_not_a_use`,
`code_check_recounts_a_claim_over_a_set_or_holds_it`, `an_agent_fixes_its_held_note_by_editing_it`,
`knowledge_cli_code_index_and_a_held_save`, `knowledge_api_code_index`).

## §memory.docs — The documents a project keeps

A note is one short claim; a design doc is how something was designed and why, a pitfall log
what went wrong and how to keep clear of it. Distilled into notes they lose the reasoning, so a
project also **keeps documents whole** (`crates/swiss-memory/src/docs.rs`): the Markdown,
reStructuredText and AsciiDoc files (`md`, `markdown`, `rst`, `adoc`) under the **sources** its
settings name - a skill's reference folder, `docs/`, a design directory. A document is not a
note: the note kinds of §memory.model are unchanged, and a document carries its own kind.

**The sources.** `docs` in the project's settings (§memory.settings): at most 8 paths inside
the checkout, relative to it (`./` and a trailing `/` dropped, `\` read as `/`, 300
characters at most), never absolute, never a drive, `..` or an empty segment; a dot directory
(`.claude/skills/…`) is fine and `"."` is the whole checkout. The global project keeps none. A
source is a folder - walked to 16 levels, never through a symlink, never into `.git`,
`node_modules`, `vendor`, `dist`, `build`, `target` or the other built and vendored
directories - or one file. A file is kept when it carries a document extension, is no change
log or legal text (`CHANGELOG`, `LICENSE`, `NOTICE`, `AUTHORS`, …), and is not
credential-named (§memory.sync's `sensitive_path`) - such a file is never opened.

**The read.** A docs run reads the sources first, offline and deterministic: each file's bytes
as UTF-8 (a broken tail dropped), redacted (§memory.sync; a redacted span keeps its line
breaks, so a section's line is the file's), at most 1 MiB kept (the document
says it was cut); at most 2000 documents and 64 MiB a read, the rest counted as not kept. A
file whose hash is unchanged is left alone. A new or changed one is kept whole: cut at its
headings outside code fences into **sections**, each headed by its breadcrumb (`Stats jobs ›
Jobs › OrderSweepCheckJob`) and its line, a section over 8 KiB cut at lines into
`(cont.)` pieces; its title is its first heading, else its file name. A document no source
holds any more - the file gone, the source removed - is deleted with its sections, card and
index rows; a project naming no source keeps none. A changed document keeps its card, marked
older than its text, until a new one lands.

**The index.** `memory_doc_fts` (FTS5, `unicode61`) over each section's heading, its text and
the words its document is found by - its path's words, its card's keywords and code names. The
text is stored segmented, a CJK run as overlapping bigrams, so a Chinese question finds a
Chinese passage the tokenizer would keep as one long token. The service writes it in the
transaction that writes the section, never a trigger; its rowid is the section's integer key,
stable across `VACUUM`.

**The card.** Each new or changed document is put to the workers (§memory.settings: the workers
alone, no lead) in a **card question**: up to 8 documents and 48 KiB a question, each
document's first 40 KiB and then the headings of the rest (120 at most), with the project's
domains (200 at most). It opens with the sync marker and asks one card a document: `kind`; a
one- or two-sentence `summary` (300 characters at most) in the run's `language`; the `domain`
path it is about (one listed, or `""`); up to 12 `keywords`, the terms a reader searches it
by, in English and in the run's language; and up to 12 `symbols`, the code names it is about
(classes, methods, jobs, tables, endpoints) as the code spells them. The kinds:

| `kind` | What it is |
|---|---|
| `reference` | how the code works now: an overview, a flow, a schema, an API, a runbook |
| `design` | how and why something was designed: a design or a decision record |
| `plan` | the steps to build something |
| `proposal` | an idea not built |
| `rejected` | an idea turned down, and why |
| `pitfalls` | what went wrong and how to avoid it: pitfalls, incidents, postmortems |
| `other` | anything else |

A `design`, `plan`, `proposal` or `rejected` document says what was intended when it was
written, never by itself what the code does now: every rendering of hits or a document opens
with that trust line, and the asker's prompt says to tell intent from what the code does
(§memory.rag).

The answer is checked - one card for each document asked and no other path, a known kind, a
listed domain or `""`, the bounds - and a wrong one is asked again within the attempt budget;
then the question fails and its documents wait. Each code name is looked up in the code index
(§memory.code), refreshed as the run starts: the name whole; else, for a qualified name
(`Class.method`, `pkg.Class`, `Class#method`, `mod::fn`), its last part in the file named for
the part before it, or under a directory so named; else the part before it when it names a
type; else the last part alone, only when at most 3 files define it - so `OrderService.place`
lands in `OrderService.java` though every service defines a `place`. Up to 3 definitions a name
are kept as `{name, path, line, kind}`; a name the code does not define is dropped and counted.
The names the card gave are kept beside them, a dropped one too, so a later run looks them up
again (The run); a card written before cards kept them - a v9 store's - waits to be asked
again, shown meanwhile. A card lands only while the document still holds the text it was asked
about (its hash); otherwise it is stale and the next run asks again. Its keywords and the names
found join the document's search words.

**The run.** A sync run with `trigger: "docs"` (§memory.sync's queue, worker and runs; phase
`docs` while it cards): the read; then the code index refreshed and every card's kept names
**looked up again** in it, no model asked - a definition the index holds since (new code, a
newer scan, §memory.code Refresh) joins its card, a moved one is where it is now, a gone one
leaves it, and a card whose document changed is left for its new card; then the card questions
at the workers' concurrency, at most `maxAsks` (200 unless given; the rest wait for the next
run), each card saved as it comes. A dry run writes nothing and asks nothing: it says what the
read would do and how many questions it would ask. No source is `empty`, its summary naming
`swiss memory docs add <path>` (ok when it removed the documents the project kept). No workers -
none named, or the layers' off - lands the read and the look-up alone and is ok, its summary
saying how many documents wait for a card. Nothing changed, no card re-linked and every
document carded is `empty`. A docs run takes no `since`, `redraw` or `lead`; a second
request merges into a queued one, its newer cap and language kept. The scope's `docs` counts
`sources`, `missing` (sources the checkout lacks), `files`, `bytes`, `added`, `changed`,
`unchanged`, `removed`, `cut`, `unread`, `waiting`, `questions`, `asked`, `failed`, `more`,
`carded`, `stale`, `refused`, `symbols`, `dropped` and `relinked` (the cards whose names were
found elsewhere). The sync sheet says them (`read N document(s) from S source(s): … · C carded
from A/Q question(s), K code name(s) found in the code · R card(s) re-linked to the code as it
is now`), and `swiss memory sync status` a running one's as `A/Q card question(s) answered, C
document(s) carded`.

**Search.** The sections that answer a query, best first: a document whose path the query names
first; then bm25 over the index (headings 3, the document's words 2, text 1), the query cut
into §memory.search's units (words and CJK bigrams, OR'd), 400 candidates read, at most 2
sections a document, 8 hits unless asked (30 at most); `kind` keeps one kind. A hit carries its
document - path, title, kind, card, and its code names with where each is defined - and the
section's ordinal, breadcrumb, line and a snippet around the words found. **A read** takes a
document id (`d-…`), its path or a unique tail of it (a tail two documents share is refused,
naming them): the card and the outline (each section's ordinal, heading, line and bytes), then
the text whole when it is at most 12 KiB, else the section asked for (cut at 12 KiB, and said
to be).

**Surfaces.** The asker's `docs`, `doc` and `code` tools and its inline retrieval
(§memory.rag); `GET /api/knowledge/docs`, `GET /api/knowledge/docs/read` and `POST
/api/knowledge/sync` with `docs: true` (§memory.api); `swiss memory docs …` (§memory.cli); the
sync sheet's run lines (§memory.panel). Sources are set by the settings API or the CLI; the
panel shows a docs run but does not yet start one (`docs::tests`,
`settings::tests::docs_are_sources_inside_the_checkout`,
`docs::tests::a_card_keeps_its_names_and_one_written_before_is_asked_again`,
`memory_sync::tests::docs_run::a_docs_run_keeps_the_documents_and_cards_them_with_the_code_they_name`,
`a_docs_request_queues_the_read_and_the_docs_routes_serve_it`, `knowledge_cli_docs`,
`memory_ask::tests::the_docs_tools_read_the_documents_the_project_keeps`).

## §memory.preview — What a note points at, read in place

Anchors name files and refs name commits; the panel opens both instead of leaving the reader to
find them. Five read-only routes under `/api/knowledge/preview/`, each with the `project`
selector, rooted at the project's first root that is a directory (the global scope has none:
`memory_not_found`): `file` (`path`, "" the root: a file's text, or a directory's listing),
`files` (`glob`: the tracked and unignored files it matches), `log` (`path`, `limit` ≤ 50 = 30:
the commits that touched it, `dirty` and `untracked`), `commit` (`rev`, optional `path`: author,
message, parents, files with their counts, the first-parent patch - that path's alone when
given) and `diff` (`path`: the uncommitted change).

**Confinement.** A path is repo-relative and made of plain components only (no `..`, no root,
no drive): it is joined to the root, canonicalized, and must still lie inside the canonical root
(a link out is `memory_invalid`). A credential-looking name is refused before it is read, on
the asked and the resolved name both (`memory_sensitive_content`): any `.git`, `.ssh`, `.gnupg`
or `.aws` component; `.env*` but the example, sample and template; `master.key`,
`session.json`, `secrets.json`, `gateway.config.json`, `managed.json`, `tunnels.json`,
`.npmrc`, `.netrc`, `_netrc`, `.pypirc`, `.git-credentials`, `.htpasswd`, `credentials(.json)`;
`id_rsa`/`id_dsa`/`id_ecdsa`/`id_ed25519*`; `.pem`, `.key`, `.p12`, `.pfx`, `.jks`,
`.keystore`, `.kdbx`. A listing shows such an entry by name, marked, never opened.

**Git** runs as `git -C <root> --no-pager` with quoted paths off, colour off, fsmonitor off,
UTF-8 log output, no prompts, no optional locks, literal pathspecs, `LC_ALL=C`, `GIT_DIR` and
`GIT_WORK_TREE` cleared, stdin closed, no console window, killed on drop or after 10 s. A rev is
4..64 hex digits - never a ref name or an option. Commits read first-parent, without renames,
external diff drivers or textconv. Git missing, failing, slow or not a checkout is
`memory_git_unavailable` (503) carrying the same `issue` as coverage; an unknown commit is
`memory_not_found`.

**Caps.** A file 512 KiB / 5000 lines, cut at a line end, BOM dropped, binary (a NUL in the first
8 KiB) said instead of shown; a listing 500 entries; a glob 200 files (a walk of 20 000 entries
outside Git, skipping `.git`, `node_modules`, `target`, `.venv`, `venv`, `__pycache__`); a patch
256 KiB; git's output 8 MiB. Each cut answers `truncated` (`memory_preview::tests::*`,
`knowledge_api_previews_stay_inside_the_checkout`).
