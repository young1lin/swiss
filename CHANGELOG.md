# Changelog

All notable changes to swiss are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- SQL analyses persist business conditions, revisions, executions and result pages. The Data
  agent and workspace share stable task/result references for follow-up questions, reruns,
  checks, charts, comparisons, saved work and exports (SPEC §data.analysis).
- System exposes installation and status of the bundled skills in the supported client
  directories; Plugins owns plugin management separately from machine settings
  (SPEC §panel.settings, §host.cli).

- `swiss gui` opens the panel in its own window on Windows and macOS (SPEC §host.gui,
  ADR-038): the same one-time sign-in `open` mints, spent by a WebView2/WKWebView window
  with a tray (close hides, Open/Exit), one window per gateway — named by port and home — so
  a config switch under another `SWISS_HOME` is another window, never the standing one, and
  a second launcher for the same gateway raises it instead of exiting silently. The explicit
  command detaches and returns like `swiss start`; `swiss gui -f` stays in the foreground.
  The window and the tray carry the brand mark itself, drawn from `assets/logo.svg`; macOS
  also ships a signed `.app` bundle and Keychain-backed master-key storage. Linux stays
  headless: the gateway serves, a browser looks.
- The Data page has an agent beside its console on a MySQL, PostgreSQL, MongoDB or Redis
  connection (SPEC §data.agent, §llm.agents, §llm.api, §panel.ui).

  Before this change, a query had to be written by hand in the console.

  Now:
  - The head's sparkles button opens a pane at the page's end edge, folded until then. Ask
    in your own words.
  - The pane works with a DB agent: an agent of its own runtime, `db`, made for this page
    and used by nothing else. Its prompt and its tools are fixed, each kind of connection
    with its own; its own prompt, if any, follows. With none, *Make a DB agent* opens the Agents page's Add sheet preset to
    it. Memory's tasks and the Agents Test never use a DB agent, and the page never borrows
    a task agent.
  - The agent reads how the data is laid out, writes the statement or command for the
    connection and runs it. What it runs lands in its own **Agent** console tab - a
    statement, a MongoDB command in the console's shell syntax, a Redis line - and its
    results in result tabs. Your own consoles stay untouched. Your question shows as you
    typed it, and the answer ends with what it last ran, exactly as it ran.
  - On MySQL and PostgreSQL, a run is one read statement in a read-only transaction that
    is always rolled back. A write or DDL is only staged in the console for you to run.
  - On MongoDB, the agent lists collections, describes one from a sample of its documents
    and runs one read command document (find, aggregate, count, distinct, ...). An
    aggregation with `$out` or `$merge` is refused, and a write is only staged.
  - On Redis, the agent pages the keyspace by SCAN, reads a key of any type and runs one
    read command line. A scan page walks on until it holds 200 matching keys or two seconds
    pass, so a sparse pattern takes one call. KEYS, writes and scripts never run; a write is
    only staged. It also
    reads a stream newest first with its consumer groups, lists the pub/sub channels that
    have a subscriber (`PUBSUB` reads pass its gate) and listens to channels for a few
    seconds to see what is published. The opening prompt says how many channels have a
    subscriber.
  - Ask for a new tab and the agent opens a console tab of its own under a name
    (`open_console`). The statements it runs or stages naming that tab land there, their
    results under it; the rest stay in the Agent tab. Asked again, it brings the same tab back.
    Before this change, everything it ran landed in the Agent tab, and it could not put a
    report's queries apart. A conversation open from before this change has the tool from
    its next send; New brings the prompt's word on when to use it.
  - A console tab is addressed by its id, not only by its name: `list_console_tabs` lists the
    connection's tabs - open, then the ones still in Recently closed - and `read_console_tab`
    reads one statement by statement. `edit_console_sql` and `delete_console_sql` rewrite or
    drop one statement of one tab, by that tab's id and the statement's index, and
    `open_console` takes a tabId beside a name. The id outlives a rename, a close and a reload,
    which a name does not: two tabs called *SQL* used to be one address, a statement inside a
    tab could not be changed at all, and a tab the reader renamed no longer answered to the
    name the agent opened it under.
  - The prompt keeps one console tab to one kind of work: the agent opens a new tab, named
    for the work, when another kind of work begins or the tab in use already holds many
    statements, and goes back to a tab by its name when that work comes back. A tab of the
    agent's you closed comes back with its text when it names it again.
  - What the agent finds and describes shows on the page: a `list_tables` or
    `list_collections` puts its grep in the sidebar's search, so the tree shows the listing
    the agent read; a table `describe_table` read opens in its tab on Structure, and a
    collection `describe_collection` read opens on Schema, sampled there as Analyze samples
    it. Before this change, the two showed nothing on the page.
  - On MySQL and PostgreSQL, the button beside the model, *Staged* by default, turns on
    *Auto writes* for the connection (SPEC §data.agent Writes). In auto mode the agent runs
    one INSERT or REPLACE, or an UPDATE or DELETE whose WHERE names a column, and commits it
    when it changes at most 1000 rows; past that the write is rolled back. A DELETE or
    UPDATE without a WHERE, or with `1=1` or another WHERE that names no column, DDL, and a
    write to the server's own schemas never run on their own: they land in the console
    unrun, and a toast says why. A committed write's result reads *Committed · 3 rows
    changed* and the insert's id. MongoDB and Redis writes stay staged. Before this change,
    every write was staged for you to run.
  - What the agent reads on Redis opens on the page as it reads it: a key or stream in its
    own tab, a channel listing on the Pub/Sub page under the agent's pattern, and a listen
    there too, its names added to the watch set (the page goes on listening live) and what
    was heard in the log under an *Agent* line. That feed is the page's, so the agent ends it
    with `stop_listen` - the reader's own Stop, a line saying who asked - and says the ask went
    out rather than claiming a feed it cannot see is closed.
  - Each connection keeps its own conversation, so two databases never mix one dialogue.
    The conversation is restored when the pane opens; New starts over and Stop cancels.
  - A folded line above the conversation names the tools the model may call.
  - Words you send while a reply streams wait in a queue above the box (*Queue*), as in
    Codex. Edit or delete each one, or steer it into the reply now with ↑ (Ctrl+Enter steers
    them all); the agent reads a steered one between its steps. After the reply, the first
    one goes next on its own, one per turn. Stop keeps the queue.
  - *History* lists the connection's conversations. Open one to read it, and *Continue
    here* to carry it on.
  - *Compact*, or `/compact` and what to keep, condenses the conversation into a checkpoint
    and carries on from it. A failed compaction leaves the conversation as it was. Typing
    `/` in the box lists the command to complete (arrows, Tab or Enter, Escape).
  - The words box grows with what you type, up to 40 % of the window, then scrolls.
  - Under the box, as in dsh, a quiet line centred at the bottom counts the conversation.
    The turns, the model's steps and its output speed open Session statistics: LLM time,
    tool time, the average time to first token, tokens per second. The tokens used and the
    cache's hit rate open Token usage: uncached input, cached input, cache write and
    output, each exact. A ring at its end shows how full the next request is against the
    agent's Max context - the provider's count of the last round against a fixed limit
    (*124K / 256K* at the default), never a guess unless the conversation was just
    compacted - and opens on what fills it: the system prompt, the tool definitions, the
    messages.
  - Copy buttons: on each of your messages, on each answer (the whole markdown), and on
    each code block in an answer.
  - The agent keeps at it: it retries a call that timed out or lost its link, fixes an
    error and runs again, tries another way when one stays blocked, and no longer ends a
    turn to offer a retry it could make itself. A Redis listen retries a subscribe that
    timed out, three tries in all. A conversation open from before this change needs New.
  - The agent answers in plain prose: no emoji, decorative symbols or exclamation marks, the
    answer first, a table only for tabular data, the statement it last ran in a code block,
    and no closing menu of offers. It saves a note rarely - a route a query verified after
    several lookups, a pitfall that cost a failed attempt, or what you asked it to keep - and
    never mentions a save in its answer. A conversation open from before this change needs
    New.
  - Beside Send, a button shows the model and the thinking effort, as in dsh: *Model* lists
    every provider's chat models with a search box, *Effort* goes from Low to Max. The pick
    is kept per agent in this browser; it starts as the agent's own model, at the agent's
    effort or else Max. A conversation under way moves to the pick on its next send or
    compaction. Its earlier rounds replay without the other model's signed thinking, and the
    model is told once that it changed. The stream, compact and reopen routes take
    `provider`, `model` and `effort`.
  - The opening prompt names a database's tables only when there are 100 or fewer; past
    that it gives the count, and the agent finds tables by name.
  - Each connection has a memory of its own for the agent: notes on where its data lives
    and how to reach it, such as which table holds a live room's bound user and how to get
    from there to the user. A note is a markdown file under
    `~/.swiss/llm/db-memory/<connection>/`, with `MEMORY.md` as the index. This is separate
    from swiss memory. A new conversation reads the index first, opens a note that covers
    the question and follows its route. When a route took real finding, the agent saves it
    (`read_memory`, `save_memory`, `delete_memory`); it saves the way, never the data. The
    head's *Memory* button lists the notes: open one to read it, or add, edit or delete
    notes there. A conversation open from before this change carries on without the index;
    New starts one with it.
  - The panel library gains `chatPane` (with a `queued` slot, a words box that grows,
    `commands` a "/" completes, and a `stats` line), `chatQueue`, `chatStats`, `copyBtn`
    and `copyText`; a transcript turn takes `copy` and `markdownNode` a `copy` for its
    code blocks; `grip` gains `end`, `folded` and `gripFold`.
  - The Agents Test and Data agent streams send a `usage` event after each model round,
    and `GET /api/llm/db/context` weighs a conversation's next request.

- The Data console's closed tabs can be brought back (SPEC §data.tabs). A console holding
  text is filed when its × closes it, a bulk close takes it, the database changes, or the
  page reloads or closes; the tab strip's overflow menu lists them under *Recently closed*
  for the connection, and a pick reopens one with its text and name. Before this change, a
  closed console was gone for good.

- An agent's Test has write, edit, glob and grep tools beside read and the shell, and its
  conversation opens with an environment snapshot in the system prompt - the working directory,
  repository and branch, home, platform, shell, date, agent and model - with a moved directory
  told in a later turn (SPEC §llm.agents, §llm.api).
- A builtin agent's shell commands can use vault secrets by reference (SPEC §llm.agents
  Secrets, §llm surfaces Agents).

  Before this change, a credential reached an agent's command only if the model saw it.

  Now:
  - An agent's `secrets` grants vault names, `group:<group>` or `*`. The default grants
    nothing. The Agents sheet has a *Secrets* field.
  - A `${secret://name}` in a `bash` / `powershell` command, where the shell expands a
    variable, reaches the command through its environment. It never reaches the command
    line, the run record or the model.
  - In single quotes, a reference stays text, so a file can keep the reference itself.
  - Write tools, memory saves and `remote.write` never resolve a reference.
  - A reference the grant does not cover is refused before the command runs.
  - A Test's first turn names the granted secrets, so the model knows what it may use.
  - Every tool answer a builtin agent's model reads is masked against every stored value,
    as is `remote.exec` and `remote.cat` output.
  - A local command's stderr is masked as well as its stdout.
- An agent's Test dialog takes a message while the reply streams (SPEC §llm.api, §llm
  surfaces Agents).

  Before this change, *Ask* was dead until the reply ended.

  Now:
  - While the reply streams, *Ask* is *Queue*. A queued message waits in a queue above the
    prompt box, with Edit, Delete and ↑ (steer now); Ctrl+Enter steers them all.
  - A steered message goes to the live run's inbox (`POST agents/test/steer`); deleting it
    withdraws it (`…/steer/withdraw`).
  - The builtin loop reads it after a round's tool answers and before it asks again. A
    `steer` event paints it as your turn, and the history files it as one.
  - After a clean reply the first queued message is sent as the next turn on its own, one
    per turn.
  - Stop or a failure keeps the queue; ↑ then sends one.
- An agent run in flight, and a memory sync's agent runs, can now be followed as they work
  (SPEC §llm.history Live, §memory.sync).

  Before this change:
  - a run's turns filed only at its finish, so a builtin agent working for a quarter of an
    hour showed nothing but its calls;
  - a running sync run said only *running*.

  Now:
  - The LLM history lists the runs in flight first, each with its round and what it waits on
    (the model, with the characters streamed so far, or a named tool) and for how long.
  - A live run's trace re-reads every 3 s and keeps the steps the reader opened. Its last,
    pulsing step is the work in flight.
  - `GET /api/llm/history/runs?source=memory:<sync run id>` lists the runs one job started.
  - A sync run says what it sends and whom it asks before the answer: phase `ask`, with its
    commits, prompt size, runner and model, then phase `apply`.
  - The memory-sync sheet lists each run's agent runs under it, live ones first, and each
    line opens that run's trace on the LLM page.
- A redis connection's channels have a page of their own, Pub/Sub (SPEC §data.pubsub): a
  channel is not a key, so the key list never showed one, and the console refuses
  `SUBSCRIBE`. `GET /api/db/{name}/pubsub?pattern=` lists `PUBSUB CHANNELS` (sorted, at most
  1000) with each one's `NUMSUB` and the `NUMPAT` count; `POST /api/db/{name}/pubsub/subscribe
  {channels?, patterns?}` subscribes on a connection of its own and streams NDJSON
  (`subscribed`, `message`, `dropped` past 200 a second, a `tick` every 15 s so a dead tunnel
  ends the feed, `closed` with its reason), at most 4 feeds a connection. The panel's Pub/Sub
  tab (from the redis tree's first row, pinned above the keys, or the connection's or a key
  tab's ⋯) puts the live feed, the watch set and
  *Watch everything* beside the channel listing and a publish row; the feed stops when the tab
  is backgrounded and logs the gap when it comes back. Recent offers the names this browser
  watched on the connection before (20, newest first; *Clear recent* forgets them), an open
  message offers *Copy channel* and *Watch only this channel*, the log holds its repaint while
  text in it is selected, and a channel that a watched pattern also matches is said to arrive
  twice.
- A memory transfer is a knowledge PR, read before it merges (SPEC §memory.store, §memory.cli,
  §memory.api, §memory.panel): an export carries a header saying whose it is (project, domain,
  who reads it, how many notes a person confirmed), `swiss memory export --domain PATH` and
  `GET export?domain=` write one domain with its subtree, and an import (`--into P|.`,
  `--dry-run`, `--strategy fail`; `POST /api/knowledge/import`) lands each note under the domain
  of the same path and reports what it created, what it skipped as a duplicate and every
  conflict it kept out - the same id at another revision, or a live note of the same title with
  other text - with both sides shown. The Memory page's project menu gains *Export…* and
  *Import…* (Check is a dry run). Before, an import skipped a note whose id or title the store
  already held without a word.
- Memory's sync reads DSH sessions beside Claude Code's (SPEC §memory.sync): a transcript
  directory is recognised by its layout, DSH's `--<key>--` session directories are read at
  their newest log generation, plain or zstd-compressed frame by frame, and only what the
  person typed and the agent's text replies are kept - never reasoning, tool calls, injected
  instructions or a subagent's session. The default `transcriptDirs` add `$DSH_HOME/sessions`
  (when set) and `~/.dsh/sessions`.
- A retrieval that failed in use is recorded and comes back as an evaluation query (SPEC
  §memory.evaluation, §memory.mcp): `memory_missed` (the twelfth MCP tool), `GET|POST misses`
  and `swiss memory misses list|add|drop` keep a miss per project - the query or task, the
  notes shown, the ones that misled, the ones wanted or what was wanted in words - the newest
  500, never exported with the notes. `swiss memory misses export` writes a fixture the
  evaluation reads back, so a miss fails there as it failed in use. The store moves to schema
  v11.
- A search and a task brief end by judging what they found (SPEC §memory.search,
  §memory.brief): one closing line calls the hits strong (the first holds the query and the
  holders sit in one domain family), mixed (only partial hits carry it, or the holders are filed
  apart) or weak (every hit shares a word or two at most), each with what to do - answer from
  the first hit, confirm it or say which domain, rephrase. Every hit carries its word share
  (`wordShare` on the wire).
- A focused brief names what was tried and dropped where it points (SPEC §memory.brief): under
  *Rejected before*, at most three superseded or invalidated decisions that a given path anchors
  or that hold half the task's words, each with where its line of successors ends. A successor
  that only refines its predecessor's claim leaves it out. Before, an agent could propose again
  the Redis cache the project had replaced.
- A save whose title asserts the opposite of a live note on the same file names that note (SPEC
  §memory.capture): "Never retry the refund webhook" beside "Always retry the refund webhook"
  answers `contradicts` with the older note and the commands to mark it outdated or link the
  two. A hint, never a refusal; English and Chinese negations alike.
- A rule the user gave is kept in their words (SPEC §memory.capture): the save policy, the
  sync's rules and the skill ask a preference, or a decision the user made, to open with what
  they said, quoted and dated, and a save without the quote or the date answers the `user_words`
  formation hint. `swiss memory lint` asks it of any draft; a panel save is the user's own words.
- A project can review the notes whose code changed, once a day (SPEC §memory.init,
  §memory.settings, §memory.panel): `swiss memory review --resolve --changed` and the sync
  sheet's *Review what changed* judge only the model's notes an anchored file changed under, a
  project's `autoReview` time of day (`--auto-review`) queues one on the gateway's clock - a
  day the gateway was down is skipped, never caught up - and `autoReviewRuns` (1 to 10, default
  3; *Daily review runs*) lets the clock queue another the same day while the one before left
  questions past its 200. An empty review deletes the empty ones before it, so a quiet week
  leaves one row; a second review request merges into the queued one.
- The Memory page says how a project keeps up with its code, and sets it in one place (SPEC
  §memory.panel): the project head's *Auto-update* button reads *Auto-update · daily 21:30* or
  *off* and opens the auto-update sheet - the daily review's switch, time, runs a day and
  workers saved by one PATCH, what waits (the inbox's changed notes, a model's apart from a
  person's), when the clock looks next and how the last review went, *Review now*, and the
  shared sync block's auto switch and interval beside the commands that install the hooks. The
  Review tab's bar tags *no daily review* while a model's changed notes wait, one click from
  turning it on. The sync sheet's project fold no longer holds the daily review's fields: it
  says how the review stands and opens the sheet.
- A question asked from a linked worktree reads that worktree's code, its branch's new files
  included, while the project stays the main checkout (SPEC §memory.scope, §memory.rag):
  `memory_ask` and `swiss memory ask --worktree` take a worktree's path or a checked-out branch,
  and an agent's note anchored to a branch-only file is judged in its own worktree instead of
  held as missing.
- A save hints what a note restates (SPEC §memory.capture): a note whose lines are mostly its
  anchored files' own words gets the advisory `restates_code` formation hint, shown in the
  panel.
- A figma MCP def takes its own `proxy` (SPEC §mcp.figma, §mcp.oauth), and the panel's figma
  form a Proxy field: the OAuth handshake's discovery, registration and token exchange now go
  out through the def's proxy, else the system proxy, so a network that cannot reach Figma
  directly can authorize it. ADR-021's amendment rejects a central proxy settings page.
- `swiss-memory` is the one skill for project memory, reading and writing alike (SPEC
  §memory.mcp The skill): besides the brief, saving, the hub and the CLI it now says which read
  tool fits what an agent holds - `memory_search`, `memory_related`, `memory_domain`,
  `memory_get`, and `memory_ask` for one question answered from memory - how to search and
  read the documents a project keeps and find a name in the code index, and which runs keep
  memory current (queued only on the user's request). Its tool table names all eleven tools, a
  test holds it to the MCP's list, and `swiss skill install`'s usage, the README and the release
  archives carry it beside `swiss` and `swiss-remote`.
- A person can hand the review inbox to the sync agent (SPEC §memory.init The review run):
  `swiss memory review --resolve [--dry-run] [--wait] [--lang TAG] [--max-asks N] [--agent
  A[,B…]|off]`, `POST /api/knowledge/sync` with `review: true` and the sync sheet's **Resolve
  the review inbox** queue a review run (`trigger: "review"`). It is a recheck whose reach is
  every live note a model wrote that carries a marker, confirmed ones too; each question says
  why the note waits (the file that changed, the note it contradicts, the dependency gone, the
  days unchecked), and the notes a `contradicts` link joins are asked side by side first. What
  the code bears out (a keep, or a fix that landed) is confirmed on the person's request, and a
  contradiction whose both sides held is lifted unless the answer links them again. A note the
  code check holds stays held, and a person's note is never judged, rewritten or confirmed. The
  scope's `recheck` counts add `confirmed` and `lifted`. A review run settles the
  contradictions its own answers raise in the same run: an answer linking a note as
  contradicting a confirmed one outside the inbox no longer waits for a second run - the inbox
  is read again and each contradiction between two of the model's notes asked side by side, up
  to 3 rounds and a pair at most twice, within `maxAsks`; what still stands is counted `open`
  and said in the summary and the panel row, beside the `rounds` it took.
- A recheck settles contradictions too, and so does a full run's closing recheck (SPEC
  §memory.init The recheck): a `contradicts` link between two of the model's unconfirmed
  notes, one of them unjudged, is asked side by side first - the side checked before judged
  beside it - and lifted once the answer keeps or fixes both sides without linking them again.
  After its planned questions the run asks each contradiction its answers raised or left
  standing between two of the model's unconfirmed notes, one of them judged or linked by the
  run, in the same run: up to 3 rounds, a pair at most twice, within `maxAsks`. A recheck
  confirms nothing; a contradiction with a person's or a confirmed note, or between two notes
  the run never touched, waits in the review inbox. The scope's `lifted`, `rounds` and `open`
  are counted for a recheck and said in its summary, the CLI's progress line and the panel row
  of a recheck or full run.
- A project keeps its documents whole (SPEC §memory.docs): the design docs, plans, pitfall
  logs and references (`md`, `markdown`, `rst`, `adoc`) under the sources its settings name
  (`docs`, at most 8 paths inside the checkout; `swiss memory docs add|remove PATH…`).
  A docs run (`swiss memory docs sync`, `POST /api/knowledge/sync` with `docs: true`) reads
  them - a new or changed file redacted, cut at its headings into sections and indexed with
  CJK bigrams, a file gone deleted - then asks the workers for each new or changed document's
  card: its kind (`reference`, `design`, `plan`, `proposal`, `rejected`, `pitfalls`, `other`),
  a summary, its domain, its search keywords in English and the run's language, and the code
  names it is about, each looked up in the code index so a hit says where the code is.
  `swiss memory docs search|read`, `GET /api/knowledge/docs[?q=]` and `GET
  /api/knowledge/docs/read` serve them, and the asker (§memory.rag) gains `docs`, `doc` and
  `code` tools - a one-shot runtime gets the matching sections inlined - so "the code that
  pauses a copy after 30 idle days" reaches the job's file in a search and one read. Design,
  plan, proposal and rejected documents are said to be intent, not what the code does. The
  note kinds are unchanged. The store moves to schema v9 (`memory_docs`,
  `memory_doc_sections`, `memory_doc_fts`); a v8 build refuses it.
- One question, answered from the project's memory (SPEC §memory.rag): `POST
  /api/knowledge/ask`, the MCP `memory_ask` tool, `swiss memory ask "QUESTION" [--agent A]
  [--json]` and the Memory page's Ask box. The agent is picked by the same layers a run
  asks (the ask's own `agent`, the project's settings, the sync block, the newest agent - a
  read, so no nudge rule); a builtin agent works the question in a bounded loop over
  read-only memory tools (search, related, get, domain, brief), a spawned runtime is handed
  the gateway's retrieval inlined and answers once, and the answer names the note ids it
  rests on. A question is at most 4000 characters; a cwd asks with the settings of the
  project registered over it; no agent to answer, or the agent failing, is
  `memory_agent_unavailable` (503).
- The MCP `memory_related` tool (SPEC §memory.mcp): the graph out from one note or a
  passage - `id`/`text` exactly one, `depth` at most 3, `limit` at most 50 - with the
  semantic state appended when the similar tier did not run.
- The Memory page's sync sheet speaks the pooled-layers world (SPEC §memory.panel,
  §memory.settings): the overview names every worker in force and the lead, each with the
  layer that chose it (the project's settings, the sync block, the plugin's newest) and the
  sentence a run would fail with; the sync block edits agents as a three-way pick -
  inherit (written back absent, never silently off), off, or a checkbox pool of the llm
  plugin's agents (a lost name stays shown and checked) - with the lead inherit, off or one
  name; the project's own settings (agents, lead, embedding) sit in their own fold, saved by
  one PATCH and repainted from the answer; a map, full or recheck run carries this run's own
  workers and lead pick (the lead never beside a recheck); and a run that asked names each
  worker and how it fared.
- A project may turn embedding off for itself (SPEC §memory.embedding, §memory.settings):
  `embedding` in the project's settings - on|off|inherit on `swiss memory project settings
  --embedding`, true|false|null on the settings API, and the global project may set it too.
  Off, the project's notes are no embedding input (the indexer skips them, the status's
  indexed/pending never count them), its search, brief task section and association are
  lexical only and each answer says why (`semantic.state "off"` with the reason), and the
  full plan and a full run's scope say `off`. The vectors are kept, not dropped - switching
  back on re-embeds only what changed. The related reply adds `semantic` when the similar
  tier did not run.
- Memory's runs ask the agents its layers name, pooled (SPEC §memory.settings, ADR-037): the
  workers and the lead are each decided by the first layer that says anything - the run's own
  request (`agents`/`lead` on `POST /api/knowledge/sync`, `--agent`/`--lead` on the CLI),
  the project's own settings (`GET|PATCH /api/knowledge/projects/{id}/settings`,
  `swiss memory project settings`), the sync block (`sync.agent` now one name, a list or
  "off"), and for the workers alone the agent added last. Several workers form a pool: each
  question goes to the member with the most room at its own concurrency, a failed one is asked
  once more of another member, and the run records each member's asked/failed tally
  (`runner.workers`). A lead handed several workers reads a roster and may name the worker a
  delegated task goes to (`delegate`'s `agent` field). The sync overview answers
  `workers`, `lead`, `chosen` (which layer decided) and `settings`, and a run's scope
  carries its own pick.
- A nudge's run asks only an agent someone named (SPEC §memory.settings): the auto-sync after a
  Claude Code session never falls back to the newest agent - the nudge API answers
  `{"queued": false, "reason": "no-agent"}` - and the lead never falls back either.
- A recheck of the notes memory's initialization wrote (SPEC §memory.init The recheck): every
  full run now closes with one, and `swiss memory init --recheck`, *Recheck notes* on the sync
  sheet or `POST /api/knowledge/sync` with `recheck: true` runs one alone. The workers (the sync
  agent without them) are shown the notes a model wrote and nobody judged since, up to 20 at a
  time and one domain per question, with the code those notes name inlined (128 KiB a
  question, 48 KiB a file - a longer file shows the lines around the methods, fields and
  constants its notes name), one request a question at the workers' concurrency, and judge each
  against that code: kept (verified), fixed (the ordinary
  update, then verified), merged into the note that already says it (superseded, with a
  `supersedes` link) or dropped (marked wrong, with the reason), and the related ones linked. A
  note the code check holds is released only by a fix whose quotes the code bears out. A
  person's note or a confirmed one is never touched. `maxAsks` caps the questions (200);
  a dry run counts them; the run's scope and the sheet show what it came to.
- Confirm a memory domain whole (SPEC §memory.domains): *Confirm all* on the domain,
  `swiss memory domain confirm PATH --all` or `POST /api/knowledge/domains/{id}/confirm` with
  `all`. The domain, every domain under it and their unconfirmed active notes are confirmed
  in one go, so a project an init drew is a click a domain, not thousands of notes one by one.
  A note that needs review stays unconfirmed and is counted, waiting for its verdict.
- Stop a memory run without turning the plugin off (SPEC §memory.sync Cancel): *Stop* on the
  sync sheet, `swiss memory sync cancel ID` or `POST /api/knowledge/sync/runs/{id}/cancel`. A
  queued run never starts; a running one drops the question in hand (a spawned runner's process
  tree with it) and ends `cancelled`, keeping the map it landed and the notes it saved. The next
  initialize skips the slices any ended full run answered, a cancelled or failed one included,
  so a long run stopped and restarted picks up where it was. While it runs the scope says its
  phase (`review`, `areas`, `knowledge`, `wrapup`, `recheck`); `sync status` prints it with the areas,
  slices and notes so far and the time run, and the sheet says it in words.
- An agent's thinking `effort` and `concurrency` (SPEC §llm.agents): a builtin or chat agent
  sets `low`, `medium`, `high` or `max`, and every agent how many of its tasks one consumer
  runs at once (1..16; 4 for builtin and chat, 1 for claude and pi) - memory's map areas and init
  slices. The Agents page edits both.
- The Anthropic wire is swiss's own (SPEC §llm.providers), like the OpenAI one: an effort reaches
  GLM, Kimi or DeepSeek behind an Anthropic-compatible endpoint as adaptive thinking with
  `output_config.effort` (an older Claude gets a token budget), and a thinking model's signed
  thinking rides back ahead of its tool calls.
- A lead for memory's full runs (SPEC §memory.init The lead): `sync.lead` names a second llm
  agent, set to think harder, that answers the map's review and, after the slices are read,
  wraps up each domain the run saved notes in - the hub's summary and keywords refreshed, up to
  three notes that span the domain's files. A builtin lead takes the new `delegate` tool (SPEC
  §llm.agents): up to 8 tasks a call, each with its files and an effort of the lead's choosing,
  answered by the sync agent at its concurrency. The sync sheet picks the lead and says what it
  wrapped up; `sync status` counts the domains.

- The process history store (SPEC §process.apps, §host.history): managed-app outcomes
  moved from the `logs/process-history.jsonl` rotation onto the module-owned store seam as
  `logs/process.db` - the same seat mcp.db, remote.db, terminal.db and llm.db ride, with the
  bounded writer queue and the generation gate. Retention becomes a row bound (the newest
  5,000 rows, swept every 64 admits) in place of the 1 MiB rotation, and a read barriers
  first so a recorded outcome stays visible to the very next read, as the synchronous jsonl
  append always was. The legacy jsonl tree imports once on first open (oldest first,
  `INSERT OR IGNORE` against the natural key, stamped in `meta`, source files left in place
  for the operator to retire), and a refused history write still warns once and never fails
  the action it records. The process module is enrolled in the shared physical budget with a
  10,000,000-byte share (ten times its ~1 MiB steady state), growing the declared budget to
  610,000,000 bytes.

- The LLM plugin (SPEC §llm): every model the toolbox asks, configured once, on three pages.
  **Providers** are named endpoints - the OpenAI chat or Anthropic messages wire, the models
  the endpoint serves (each a chat or an embedding model, the first of each kind the default),
  the key a `${secret://name}`/`${ENV_VAR}` reference only, an optional proxy - with Ollama, GLM
  and OpenAI presets and a one-prompt Test on a picked chat model. *Discover* reads the
  endpoint's own `/models` listing and offers what the provider lacks, its kind guessed, for
  the operator to confirm. Anthropic-style providers send the key on both `x-api-key` and
  `Authorization: Bearer` (GLM's compatible endpoint reads the latter alone). Every consumer
  picks one of a provider's listed models of its kind, never a typed id: an unlisted pick is
  refused naming the listed ones, and a provider edit that would strand a consumer is refused
  naming it. **Embedding** names an OpenAI-style provider and one of its embedding models,
  with the width, batch, prefixes and similarity floor every consumer shares; the form sits
  beside the block in force. **Agents** say who executes a tool-using task: `builtin` (swiss's
  own tool loop over a provider, carried by genai), `chat` (one request, no tools), `claude` (a
  spawned Claude Code) or `pi` (a spawned pi-coding-agent), each with its limits, its own
  system prompt (the runtime's default shown when it has none; appended to a CLI's own) and
  the tools it takes from its runtime's catalog - ticked in the sheet, narrowing what a task
  offers and never widening it. A row opens to the prompt and the tools in force; a Test
  holds a live conversation with a provider-asking agent (the Test dialogue, below) and
  checks a spawned one's binary.
- Agent telemetry (SPEC §llm.telemetry): agent runs reported as OTLP logs over gRPC,
  HTTP/protobuf or HTTP/JSON in Claude Code's own event vocabulary (`user_prompt`,
  `api_request`, `api_error`, `tool_decision`, `tool_result`, with `session.id`, `prompt.id`
  and `swiss.agent`), so a collector built for Claude Code - cc-otel - reads them unchanged.
  A `claude` agent's child gets the OTEL env and reports itself; pi has no hook. The exporter
  queues without blocking a run, retries what OTLP calls retryable, and its Test is an empty
  export that stores nothing.
- The memory plugin's sync engine (SPEC §memory.sync): commits and Claude Code sessions
  distil into memory. The `sync` block names an LLM agent; it reads the walk since the synced
  head - commits, patch, transcript digest, all redacted - and answers a proposal the service
  applies like any agent write, unconfirmed. A `builtin` agent gets memory's `read` (pinned to
  the checkout), `bash` (read-only git) and `sync` (files one checked operation a call) tools;
  `read` and `bash` refuse credential-named files and redact what they answer like the walk.
  `swiss memory sync` queues and follows runs, `--nudge` merges bursts, `sync hooks install`
  wires Git's post-merge/pre-push; the panel's sync sheet shows the agent, the head, what waits
  and the runs, and edits the block.
- `swiss memory init --map` (SPEC §memory.map): the first project map drawn from the code as
  it is now, sized for a small model. The program reads the areas off the code index and the
  manifests (packages, directories, business areas cut across technical layers), then asks
  the sync agent one bounded question per area - children before parents - and the answer
  fills fixed fields (title, summary, overview, concepts, entry points, flow, code, data,
  dependencies, keywords) that are checked against the index before the program assembles
  the hub. Relations between areas are the names one uses that another defines, counted within
  one language and across packages only where a manifest declares the dependency; files the
  root `.gitattributes` marks generated or vendored are no area. `--lang` writes the hubs in
  the team's language; `--plan` shows the areas offline and, over a map that owns the code
  already, says nothing new is drawn (the panel's preview the same);
  with no agent the hubs are the index's outline. Areas already on the map are kept, and a
  project that never synced starts its sync from the drawn map. The panel's sync sheet has
  the same as *Draw the map from the code*, and the Claude Code hook snippet nudges the sync
  at `SessionEnd`.
- The map is cut by business, not by the outermost folder (SPEC §memory.map): the survey walks
  past source wrappers (`src/main/java/com/acme/...`, `src`, `lib`, `app`) to the package path,
  cuts the business names across controller/service/repository/model layers, and keeps `sql`,
  scripts and the like as technical areas beside the business domains. A business owns its
  files under the directory its layers lie in (never a `**/Name*` glob over the whole
  checkout), with `dal`, `remote` and `job` among the layers, and the layer directories named
  for it (`web/res/invoice`, `service/ecpay`); its parts are its longer stems
  (`invoice/line`), never a layer's whole directory; abbreviations (`EcPay`, `Pm`) are
  businesses too, and up to 48 of them reach the review beside 14 other areas. When three or
  more new top-level areas would be drawn, the sync agent reviews the draft once - groups,
  titles, merges (nested instead when the merged area would own more entries than a domain
  holds), its mistakes dropped by the program - before any area is described; a failed review
  keeps the draft. `--redraw` removes the domains an earlier map run drew that no person or
  later write touched (deepest first, their notes kept and refiled) and draws them again.
- `swiss memory init --full` (SPEC §memory.init): the whole project's knowledge initialized in
  one run - the map run, then every code and doc file read as it is now, sliced by domain
  (48 KiB and 12 files a slice, a long file read whole in parts), and the sync agent asked a
  bounded question per slice for the pitfalls, constraints, decisions and knowledge the code
  teaches. Each note goes through the ordinary checked save, anchored in the files read, its
  quotes checked against the code (a quote the code does not hold is held for review). A later
  run skips the slices whose text has not changed; `--max-asks` caps the questions (200 by
  default). With an embedding model on the LLM plugin the notes are indexed like any note and
  the run says the index's state; without one it says the notes are found by their words.
  `--plan` shows the slices and the index with no model asked; `--wait` follows the run and then
  the index. The panel's sync sheet (now *Map and initialize from the code*) has *Redraw*,
  *Preview initialize*, *Initialize memory* and *Max questions*, shows a full run's notes and
  index, and follows a queued or running run every 3 s. A live e2e (`e2e/memory-init.test.mjs`)
  runs it against the models a `.env` names and skips without one.
- A memory question that fails in transit (the network, the request timing out, 429, 5xx) is
  sent once more after 2 s inside the agent's deadline: four questions in flight on one
  connection died together when it broke, leaving map areas undescribed. A refusal is never
  re-sent, nor a streamed ask.
- Search keywords on notes and domains (SPEC §memory.model): up to 16 terms, in English and
  the team's language side by side, weighted between title and content in keyword search and
  counted as the title by the CJK tier and the brief, so a query in either language finds the
  note with no embedding model. `--keywords` on the CLI, `keywords` on the MCP tools, the API
  and sync proposals, and a keywords field in the panel's note and domain forms. The store
  moves to schema v7. A sync sees a touched hub's keywords and extends them when a change
  brings a new term to search it by.
- A `chat` or `builtin` sync agent whose answer came back empty, or stopped at `maxTokens`
  (a thinking model that spent its budget thinking), is asked once more and told why; a run
  that still fails names `maxTokens`.
- Memory's semantic search takes its model from the LLM plugin's Embedding page; the gear
  sheet shows the model in force and links there.
- The Memory and LLM pages are named in the panel's wire labels (they showed the raw served
  text).
- The UI library gains `editList`/`editLine` (a list a form edits in place), `checkGrid` (many
  checks in a bounded, scrolling frame) and `split` (a form beside its in-force block), each
  in the gallery.
- The memory plugin (SPEC §memory): a local, cross-agent, project-scoped graph of notes -
  pitfalls, constraints, decisions, preferences, durable facts - with revisions, provenance
  and anchors, filed under the project map's domains; deterministic search tiers (exact,
  symbol, word and CJK bands) fused with meaning when an embedding model is set. One engine
  and one rule set behind every entry point: `swiss memory` offline, `/mcp/memory` for MCP
  clients, `/api/knowledge` and the Memory page.
- Module-owned history (SPEC §host.history, ADR-033/034): MCP's calls and traffic move into
  the module's own SQLite database, `logs/mcp.db` - sealed body chunks, an archive with no
  row ceiling (the ring stays as the live window), literal deep search with honest cursors
  (an exhausted budget is an INCOMPLETE page with a Continue, never "no matches"), and
  physical quotas per module. The legacy jsonl tree and body files are imported idempotently
  on first open and left in place.
- Terminal recordings join the module-history store (SPEC §terminal.recording, ADR-035):
  sessions and their sealed asciicast event lines live in `logs/terminal.db`, and `.cast`
  becomes an export the route reconstructs byte-faithfully from the stored lines - the first
  open imports every legacy `.cast` idempotently (natural key = session id) and leaves the
  files in place. A 100 MB share and a 90-day sweep replace a folder that had no retention.
- The rail's default seats (SPEC §panel.nav): every served group rides by default, up to
  fifteen seats (`RAIL_LIMIT`) - a new module is on the rail the moment it serves, not
  waiting in the palette for a pin. The default applies only while no choice is stored: an
  operator who has pinned keeps their list exactly as it was.
- `llm.history` (SPEC §llm.history): every provider round trip - each chat request and reply,
  thinking included, and each embedding call with its full input texts and the vectors the
  endpoint answered (each component rounded to six decimals), so a round trip reproduces from
  the record - and every agent conversation, turn by turn, recorded in the module's own
  `logs/llm.db`. An embedding can leak the text it embeds, so the record stays admin-only
  behind the session, sealed past each row's 2 KiB inline preview, bounded by the share's
  quota, swept at 180 days and never exported. The read-only History page reads it back:
  *Calls* newest first with the sealed body a row away, *Conversations* with each turn's full
  text one expand away and whether the conversation is open or closed.
- The Test dialogue (SPEC §llm.api, §llm.history): a `builtin` or `chat` agent's Test is a
  real, persisted conversation that streams. Deltas arrive as server-sent events
  (`POST agents/test/stream`) and paint as they land - reasoning dimmed, tool asks as chips,
  the closing answer as markdown. Each prompt files as the run's next user turn and the turns
  already filed are never rewritten; the sheet resumes the agent's open conversation, *Stop*
  cancels the stream (`POST agents/test/cancel`) and keeps what was filed, and *New* closes
  the run (`POST history/runs/{id}/close`, nothing deleted) before starting fresh. A
  continuation is refused before any provider request with a 409 that names why
  (`open-run-exists`, `gone`, `closed`, `context-changed`, `unresolved-tail`,
  `over-budget`), never a silent fork. DeepSeek's official thinking mode gets every earlier
  turn's `reasoning_content` back in the replay. The OpenAI stream is parsed by `sse-stream`
  behind byte and frame caps, and every refusal - chat, embedding, telemetry - is scrubbed of
  the key and the proxy's credentials, including a key the message clip cut in half.
- Remote Runs search (SPEC §remote.panel): a search box leads the page head, always shown,
  and searches what the recorded runs printed (not the command - an `sh -s` script is sealed
  stdin). Typing searches after a pause, Enter at once, Escape brings the list back. The
  results replace the list, newest first, each row carrying the line its first match is on
  with the match marked; the scan walks the record oldest first in bounded rounds and *Keep
  searching* resumes it. An unfinished scan that found nothing says the newer part of the
  record is not searched yet, never "no matches".

- **Managed apps and the Process page** (SPEC §process): `swiss-process` (config,
  status, control, capture logs, history), the process plugin with its panel page
  (apps table, children by owner, the log follow sheet, the add/edit form whose
  save is the whole-apps PUT), and `swiss process` - `status [--json]` (exit 0
  only when every autostart app is running), `start|stop|restart <name>
  [--force]` (polls to the outcome, exit 2 when the gateway is unreachable or
  the plugin is off), and `logs <name> [--app] [-n N] [-f]` (follows with the
  after cursor). Every action names its actor (`cli` or `panel`) in the history;
  the plugin's config PUT answers with storable-but-not-runnable warnings.

- A provider's model states its display name, context window, max output tokens, whether it
  reads images and whether it takes a thinking effort (SPEC §llm.providers, §llm surfaces
  Providers, §data.agent).

  Before this change, a model in the provider sheet was an id and a kind, nothing more.

  Now:
  - Each model line has *Details*: *Display name*, *Context window* and *Max output tokens*
    (typed as `1M`, `256K` or `131072`), *Input types* (Text, Image) and *Thinking*. A
    quiet line under the id says what the model states.
  - Every preset fills them as its vendor states: DeepSeek V4.1 Flash reads 1M context, 384K
    output, image input; GPT-6 and Claude 5.5 cap their output at 128K; Qwen3.8 Flash and
    MiniMax M3 take no effort.
  - *Fill known details* fills the lines of a provider saved before, and Discover's take
    brings them with a known id.
  - An agent's answer asks at most the model's max output: the default 131 072 used to pass
    GPT-6's and Claude 5.5's 128K. A model that takes no thinking effort is sent none.
  - The Data agent's pane names the model by its display name, shows no effort for a model
    that takes none, and its context ring reads against the model's window when that is
    smaller than the agent's budget.

### Changed

- The LLM page's *Add provider* offers presets for the vendors a setup reaches for (SPEC
  §llm surfaces Providers). Before this change, it offered only Ollama, GLM and OpenAI (with
  `gpt-4.1-mini`). Now it also offers Anthropic, Google Gemini, DeepSeek, Kimi, Qwen (Alibaba
  Cloud Model Studio), MiniMax, Doubao (Volcengine Ark) and OpenRouter. Each fills the wire,
  the base URL, a `${secret://<preset>}` key reference and the vendor's current models, the
  everyday one first: DeepSeek V4.1 Flash (`deepseek-flash`) and V4 Pro, Kimi K3.1 Pro and its
  highspeed twin, GLM-5.3 Flash and GLM-5.3, Qwen3.8 Flash and Max, MiniMax M3.1 Flash and M3,
  Doubao Seed 2.1, OpenAI's GPT-6 line, Claude 5.5 and Gemini 3.8 Flash and 4 Argon. A Chinese
  vendor's preset uses its China mainland host.
- A provider's name may have capitals (`GLM`, `DeepSeek`, `OpenRouter`) (SPEC §llm.config).
  Before this change, the name had to be lowercase letters, digits and dashes. Agent names
  stay lowercase. Two provider names that differ only in case (`glm` and `GLM`) are refused.
  The presets now name each provider as the vendor writes itself.
- The builtin `list` tool is gone: `glob` finds files by pattern, and a saved `list` reads as
  `glob` (SPEC §llm.agents).
- A thinking round reads as dsh's Think line - the brain, Think and the thought's first line -
  in History and in the Agents Test stream (SPEC §llm surfaces).
- An agent's trace and a memory sync's runs read as a flow instead of a wall of text (SPEC
  §panel.ui, §llm.history, §memory.sync).

  Before this change:
  - the LLM history's run sheet used the event list, every step a dense log line;
  - the memory-sync sheet put every run's counts, summary and failure in one card, an *Agent
    runs* button under every finished run, and waiting conversations as a raw byte count
    (*348772267 unread conversation byte(s)*).

  Now:
  - A new library shape, the flow (`ui/flow.ts`), draws one quiet line per step: a glyph or a
    state dot, the name, a one-line summary in grey, the duration on the right, and space
    between steps instead of rules.
  - The run sheet's head is three fact lines. A round is one line with the model's words
    under it as prose. A tool call opens to its arguments and result on one code surface,
    scrolled past 280 px. The step in flight shimmers.
  - A sync run is one row: its state dot, what queued it, its phase or marks, its age, and
    its verdict under it. Its counts and agent runs open in place. A running run opens on its
    own; a finished run reads its agent runs the first time it is opened. Its agent runs are
    rows of the same flow, and a row opens that run's trace.
  - The waiting conversations read as a size (*332.6 MiB of conversations unread*).

- A memory sync over a backlog sends every commit it can, its patch giving way first (SPEC
  §memory.sync): over the prompt budget a run sent half as many commits, so the fixed parts of
  a large project's prompt (its 159-domain map, the touched hubs and notes) left room for two
  commits a run and 68 waiting commits took about 34 runs of ~14 minutes; the patch is now
  shaped again into the room the rest of the prompt leaves, and only a prompt still over sends
  fewer commits. The budget is five sixteenths of the agent's request cap (320 KiB at the
  1 MiB default, was 160 KiB at most), and the patch puts the code before the docs and data.

- A builtin or chat agent's answer runs to 128K tokens unless it says less (SPEC §llm.agents):
  `maxTokens` defaulted to 8192 and stopped at 32 768, so a thinking model cut mid-thought
  ended a memory sync's loop with nothing filed; both are now 131 072, the most GLM accepts
  (it refuses a larger `max_tokens`), and the LLM page's agent form takes up to that.

- A builtin or chat agent's request budget is its own and larger (SPEC §llm.agents,
  §memory.sync): the 512 KiB every request and every continued conversation stopped at is now
  the agent's `maxContextKiB` (64 to 8192, 1024 unless set), set on the LLM page's agent form
  as Request budget (KiB) and shown in the agent's setup. A loop round over it still refuses
  before it flies, now naming the budget, and a conversation whose record has passed it takes
  no further send. A memory sync run keeps its 160 KiB prompt, the rest of the larger budget
  left to the reads and the thinking; under a budget below 512 KiB the prompt takes five
  sixteenths of it.

- The Memory page is faster on a large project (SPEC §memory.store, §memory.freshness,
  §memory.related, §memory.panel). A project's notes and links, read whole by the review inbox,
  listings, the map, the brief, the graph and the association, are kept between reads until
  anything commits to the store (`PRAGMA data_version` on a connection of the cache's own, so a
  CLI or agent write is seen too), and released after ten minutes without a read. A file's hash
  is reused while its size and modified time stand. The association builds its reasons for the
  notes it keeps, not for every candidate. A note opens before its related notes are found. On a
  copy of a 1,962-note project (release build), the review inbox went from 444 ms to 65 ms after
  its first read, the association from 137 ms to 52 ms, the graph from 65 ms to under 1 ms, and a
  200-note listing and the map to a few milliseconds.
- The plugin wrappers are their own crate, `swiss-plugins` (SPEC §arch.crates): the
  descriptors, routes and adapters that put terminal, remote, process, memory and llm on the
  host were the root crate's `src/plugins/` - 20k of its 38k lines - and now build as one
  crate only the root depends on, so an edit to the root's own files (the CLI, the admin API,
  the server bootstrap) no longer recompiles them. No behaviour changes.
- A database MCP holds no connection while nobody uses it (SPEC §mcp.registry, §mcp.adapters).
  The 15 s health probe runs only while someone reads health (the MCP list, an MCP's pages, the
  Data page's connection list) and parks 30 s after the last read, so boot no longer builds
  every database pool just to ping it; the first MCP-list read after a lapse waits up to a
  second for the round it starts instead of showing every database unknown. A connection
  unused for ten minutes is closed (mysql and pg gracefully) and the next call opens a fresh
  one. The idle reaper sleeps toward its earliest deadline instead of sweeping every second.
  Measured A/B on 19998 over one real-state snapshot, 6.7 idle minutes with nothing reading
  (SPEC §product.memory): resident database connections 0 against 5, the gateway's CPU over
  the window 31 ms against 171 ms, working set 27.5 MB against 29.1, private unchanged; two
  minutes of MCP-list reads brought the five back, and ten minutes after the last read all
  five were closed again.
- The jobs scheduler sleeps toward its earliest next-due entry instead of waking every second
  for the life of the process (SPEC §jobs.triggers): at least 1 s once an entry is due, so the
  scan cannot spin and a boot still finds overdue jobs a second in; at most 60 s, so a clock
  step or a resume from sleep is caught within a minute; parked when no job is defined.
  Applying a config wakes it.
- The panel ships gzip-compressed and is no longer copied per request (SPEC §panel): a release
  build embeds `admin_assets/` gzipped (6.96 MB of files to 2.00 MB; the binary 23.49 MiB to
  18.78 MiB) and sends the stored bytes as `Content-Encoding: gzip`, inflating only for a
  client that refuses gzip. Every body is borrowed from the binary, where each request used to
  copy its file into a fresh string (mermaid 2.8 MB a load). Assets are `no-cache` with an ETag
  instead of `no-store`, so a reload still revalidates every module but gets a bodiless 304 for
  each unchanged one; the shell stays `no-store`. The panel version stamp hashes each file's
  SHA-256 rather than its length, so a same-length edit flips it too. Measured A/B on one
  scratch home (SPEC §product.memory): idle unchanged, and after the whole panel is loaded
  three times the working set is 21.99 MB against 26.83 (peak 21.99 against 29.57), private
  7.93 against 8.61 MB, a full load 1.90 MB on the wire against 6.63.
- A task brief shows what helps (SPEC §memory.brief, §memory.evaluation): a task hit after the
  first that shares only a word or two is a line under *Also matching your task*, its links not
  followed, and a strong readout leaves it out; *In <path>* counts the in-play domain's notes
  instead of listing them every turn. A judged pass had found only about a quarter of the 8-10
  notes a brief showed helping; briefs now show 4-5 notes and the share fully correct rose from
  0.17/0.36 to 0.44/0.52 on the two fixtures, recall unchanged.
- Memory's search ranks a note a domain hub links or names a little higher (SPEC
  §memory.search): a tie no longer goes to a random id, and the boost stays inside the word
  band, never over an exact hit.
- A word hit must hold a fifth of the query's word weight, in search and in the brief's task
  hits (SPEC §memory.search, §memory.brief): a context of five no longer fills up with notes from
  other domains sharing one word each. A path in a query counts the words that name its part
  (`…/store.rs` counts `store`, not `crates`, `src` and `rs`), the brief's linked notes follow
  the subject rather than a stray word hit, and an unconfirmed rule a confirmed note contradicts
  leaves the brief's Rules. Search's wrong-domain contexts fell from 0.55/0.44 to 0.43/0.29 with
  recall unchanged.
- A linked worktree's root is the main checkout (SPEC §memory.scope): its first write names
  the project after the repository and registers the checkout that outlives the branch, a
  question asked from a subdirectory reads at that root, and a stored root that named a
  worktree heals on the next write. A bare repository's worktree and a submodule stay their own.
- `swiss skill install` also writes `$CLAUDE_CONFIG_DIR/skills` (SPEC §host.cli): Claude Code
  reads its user skills there when that is set, so an account with its own config directory got
  no swiss skills. A config directory that is `~/.claude` itself is written once.
- A memory question checks memory against the code (SPEC §memory.rag): a builtin agent
  answering one also takes the recheck's `read` and `bash` over the scope's checkout -
  pinned there, credential files refused, redacted, read-only git - and its prompt has it
  check each specific it states (a route and its class, a file or script path, a count, a
  retry) before stating it, cite a file only once it is shown to be there now, and report
  the code where a note or a document disagrees, naming that one as out of date. Before,
  an answer could repeat a stale document: a retry count the code never had, a deleted
  DDL script, a v3 controller credited with a v4 route. A scope with no checkout answers
  from memory alone, as before. Where memory disagrees with itself (a note says a step
  never retries, a document's table says three times) the code decides, read in the
  method that does the step.
- A memory agent's `bash` splits its command as a shell would (SPEC §memory.sync): a
  quoted pattern reaches git without its quotes, an unquoted pipe, `;` or redirection is
  refused out loud, and a miss says "nothing matched". Before, the words were split on
  spaces, so `git grep -n "open/real"` searched for the quotes too and came back empty,
  and the agent read that as a miss.
- Redaction (`secrets::redact`, SPEC §memory.sync) takes a short password too: the value
  after `password`, `passwd`, `pwd` or `密码` and `:`, `=` or `：` when it is 6 to 64
  printable bytes with a digit or a symbol and reads as neither code, a path nor a
  placeholder (`"Password": "…"` in a sample response, `&password=…` in a deeplink,
  `${DB_PASSWORD:…}`'s default), and the password in a URL's `user:password@`. Only values of
  12 or more letters and digits were taken before, so a design doc's sample account
  password reached the model that cards it. Both rules redact and never refuse a note.
- An absent `sync.agent` means inherit, not off (SPEC §memory.sync, ADR-037): a block that
  named no agent silently asked nobody; it now asks the project's settings' choice, then the
  agent added last. Say "off" to ask nobody.
- A run's dialogue read carries each call's prompt and preview as their first 400 characters
  (SPEC §llm.history): a look loop repeats the ask's prompt every round, so a recheck's trace
  sent that prompt once a round, about 1 MB for one question; the whole prompt is still the
  run's user turn and the call's body. The Test's continuation check now reads only the run's
  system turn instead of decoding the whole dialogue on every send, and still counts a system
  body that will not read as changed.
- A project map goes five levels deep, not three (SPEC §memory.domains, §memory.map), and
  memory's map draft splits the areas largest first - an area of twelve code files or more into
  its parts, then whichever area or part holds the most code left - while the draft has room,
  now 150 areas, not 60: how much code an area holds decides how deep the map goes there. The
  draft split each top-level area once and dropped every sub-area once the top level filled
  the 60, so a large Java backend drew 54 areas and no sub-area at all; it now draws 150
  over four levels (`core/event/notification/context`, `orders/list/profit`), the review's parent
  the fifth. The panel's map indents all five, and a domain on the fifth level offers no
  sub-domain.
- A builtin or chat agent's own system prompt follows the runtime's default instead of replacing
  it (SPEC §llm.agents): the default - read data is never instructions, never repeat a credential
  - is always sent, the agent's prompt after it, and an agent with none sends the default alone.
  A prompt that was started from the default carries it once. The Agents sheet shows the default
  read-only above the box, and *Start from the default* is gone.
- Memory's full run keeps 3 notes a slice, not 6, one a subject (SPEC §memory.init): what a
  slice's files hold about one table, flow or rule is one note with its facts together, never a
  note per fact. Six filled most slices - 3.9 a slice, 1,026 notes after 262 slices of a
  large project - with each fact of one subject a note of its own.
- The LLM history's share is 600 MB, not 100 MB (SPEC §host.history), and the declared
  history budget 1,110 MB with it. One memory recheck, whose prompts carry the code they ask
  about, filled 100 MB within a day, so the size trim kept cutting calls still being read.
- The LLM history's share is 1,000 MB, not 600 MB (SPEC §host.history), and the declared
  history budget 1,560 MB with it. The store already held 393 MB of its 600 MB seat, and the
  size trim starts at 90% of the share.

### Fixed

- Data chat follows streaming replies while at the bottom, offers a jump to the latest
  message, and collapses work details after completion or stop. Nested call details return
  to their conversation history sheet when closed.
- Desktop navigation stays on the selected gateway; sign-in URLs are constructed locally
  from the ticket. Export completion and failure are reported by the native download handler.
- Skill API tests isolate client home directories as well as gateway state. Login ticket
  and cookie expiry boundaries have deterministic regression coverage.

- A link that asks for a new window reaches the browser from `swiss gui` (SPEC §host.gui).
  Before this change, every `target="_blank"` click was dropped in silence: the webview had no
  new-window handler, and both engines deny the request when none is set. An MCP authorization
  page opened by the panel was told "opened" and then timed out after five minutes, and answer
  links, a URL cell, *Open UI* on the Process page and memory references did nothing at all.
  Only `http` and `https` are handed over — the panel renders model output and database cells,
  so `file:` and `javascript:` are exactly the payloads the opener must not receive. `swiss
  open` also works on macOS now, where it called `xdg-open`.
- The window goes with the gateway it was showing (SPEC §host.gui). The panel's Quit stopped
  the gateway and left the shell standing: its page could never load again, and because the
  shell still held the single-instance claim, the next `swiss gui` could only raise that dead
  window — the tray's Exit was the only way back. The shell now closes with the gateway, and a
  launch that finds a window for a gateway nothing answers on waits for it to leave and takes
  the identity over.
- `swiss start` recognises its own gateway, not merely a 200 (SPEC §host.daemon). The health
  probe read the status code alone, so a dev server or any other app on the port answered
  "already running", `stop` refused to act, and the panel link was minted for a page that is not
  the panel. `/health` names the process (`{"ok":true,"build":…}`); the probe reads it.
- A racing `swiss start` no longer erases the pid file the winner just wrote (SPEC
  §host.daemon). Two starts on one port both wrote a record; the loser's child died on
  EADDRINUSE, and the loser then deleted the file — by then the winner's — so `swiss stop`
  answered "not running" over a live gateway and `restart` was a no-op. A record that names a
  standing process is nobody else's to replace or delete; the start that actually serves the
  port writes its own.
- `swiss gui` no longer turns a temporary `SWISS_PORT` into a permanent port (SPEC §host.gui).
  The shell resolved the port and handed it to the start, and a handed port is persisted into
  `gateway.config.json`: one launch under the variable pinned that port for every later start.
  The shell passes none, and the start resolves it exactly as the shell did.
- The console keeps its own font size in the desktop window (SPEC §panel.design). The window's
  zoom listener runs on `document` in the capture phase and took Ctrl+= / Ctrl+- / Ctrl+0
  before xterm's textarea saw them, so the terminal page's zoom was dead in the app while it
  worked in a browser tab. Keys typed inside a console are left to it, and on a Mac the window
  takes Cmd, which is the platform's modifier and had no menu item to fall back on.
- A skill's status and its install read `$CLAUDE_CONFIG_DIR` (SPEC §host.skills). The batch
  install honoured the variable; the per-skill path and the panel's rows did not, so on a
  machine that sets it the row said "installed" for `~/.claude`, where Claude Code reads
  nothing, and *Install* wrote there too.
- The Data console's head says what the console runs by on hover (SPEC §data.tabs): the rule
  was a two-line paragraph above every editor, and it is now a help mark beside the title with
  the words in its tooltip. *Run* carries the primary weight on the one line the head now is.
- A DB agent's pane keeps its opening guidance until the first question (SPEC §data.agent).
  `render` decided on the transcript *container*, which exists from the first paint, and
  `follow` hid the line in the same tick, so an empty conversation never said what to type.
- The Data console's result tabs all stay reachable when a script answers many (SPEC
  §data.console). Before this change, the tabs wrapped onto two lines and the last ones hid
  under the bar's facts and buttons. Each tab now stays on one line in a strip that scrolls
  sideways, the mouse wheel too, with the tab showing scrolled into view, and an *All results*
  list after it names every result, the one showing checked, with a find box that matches a
  table or a statement.
- A model id ending in `-max` goes to the provider whole (SPEC §llm.agents). Before this
  change, the suffix was read as a Max effort and cut off when the agent set no effort, so
  `qwen3.8-max` went out as `qwen3.8`, and `qwen-max` or `gpt-5.1-codex-max` likewise. The
  `-low`, `-medium` and `-high` suffixes still count as an effort.
- A GPT-6 model on the OpenAI wire gets `max_completion_tokens` (SPEC §llm.providers). Before
  this change, only `gpt-5*` and the o-series did, so `gpt-6-astra` or `gpt-6.1-terra` got
  `max_tokens`, which OpenAI refuses for its reasoning models. Every `gpt-<N>` from 5 on now
  gets it; `gpt-4o` and `gpt-oss` keep `max_tokens`.

- `swiss remote` (and every CLI call to the admin API) works when run from a sandbox that
  scrubs the environment. With neither USERPROFILE nor HOME set, the home fell back to the
  working directory, so the CLI looked for `.swiss/session.json` under the project it ran in,
  sent no CLI key and got `401 not signed in` (Codex's code run in another project,
  2026-10-29). The home now falls back to the OS's record of the account's profile, the order
  Node's `os.homedir()` uses; an empty variable counts as unset.

- A JSON cell's editor sheet on the Data page shows its value in syntax colour (SPEC
  §panel.code). The sheet had a bare textarea, so a JSON value showed in one flat colour. It is
  now the SQL console's layered editor: keys, strings, numbers and `true`/`false`/`null` in the
  code scheme's colours, repainted as you type, on Format JSON and on the boolean toggle.
  Other text stays plain. A JSON value now opens at the taller JSON height. Both editors now
  reserve the scrollbar's gutter in the text layer too, so a long wrapped line no longer
  shifts the colours below it once the box scrolls.
- An agent's `api_request` telemetry records now carry `ttft_ms`, the time to first token
  (SPEC §llm.telemetry). A collector such as cc-otel showed TTFT as "–" for every swiss
  request, while Claude Code's own rows had it. The time runs from the request's start to
  the model's first thinking, text or tool-call output on the stream, and the opening frames
  don't count. A reply with no output carries none.
- A tool call in the Test stream and in History reads as its own description when it gives
  one (SPEC §llm.history, §llm surfaces Agents). Before this change, the line showed the raw
  command even when the agent had said what the command does. The command is still in the
  opened step.
- An agent's Test dialogue streams its work in the order it happens (SPEC §llm surfaces
  Agents Test): what the model said before each tool ask was appended below the whole
  "Working" fold, so every round's words piled up under the work, out of order, while the
  thinking and tool steps stood inside it. A round's words now stand inside the work
  between its thinking and its tool asks, rendered as markdown once the round moves on, as
  the History sheet already shows them; the closing answer still stands once below.
- An agent's Test conversation no longer refuses its next send as over its "continuation
  budget" long before any request is near the agent's `maxContextKiB` (SPEC §llm.agents):
  the guard summed every sealed call's body, and each call re-sends the dialogue before it,
  so the sum grows with the square of the rounds - lead's run 1889 was refused at 1.58 MB
  over 16 calls whose largest request was 215 KB, against a 1024 KiB per-request cap. The
  guard now weighs what the next send replays (the canonical prior context) against the
  cap, the loop still weighs each round's exact ask, and the 400-turn and 512-call caps
  stand; the refusal names which ceiling was passed.
- A sync run whose builtin agent ran out of answer while thinking no longer passes its
  commits as synced (SPEC §memory.sync): the loop took a closing answer cut at `maxTokens`
  with no text as the end, so r-a8315d1b on shop filed nothing, was recorded ok and moved
  past its two commits, its summary blaming a 24-turn cap it never reached (seven rounds). A
  loop that ends without a closing line now says why - the token cap, the turn cap or neither -
  and when nothing was filed either the run fails and the commits wait for the next one.
- A sync run over a large backlog fits its builtin agent (SPEC §memory.sync): the prompt was
  sized only by its parts' caps, so 40 commits on shop made a 315 KB prompt, and the agent's
  reads plus the thinking it replays every round passed the 512 KiB request cap ten minutes
  in, failing the run with nothing written. The prompt now stays within 160 KiB: over it the
  run takes half as many commits, down to one, and the rest wait for the next run.
- A sync run over merged history sends every commit it chose (SPEC §memory.sync): it ended
  on the last chosen commit in reverse topological order and read `base..that`, so behind a
  merge HEAD the other side dropped out - shop chose 68 commits and sent 40, and the next
  run would have diffed across the merge. A run now ends on a commit of HEAD's first-parent
  line, the furthest whose commits fit, and commits already synced no longer count as waiting.
- Jobs' `retention.maxHistoryBytes` (64 MiB by default) is enforced (SPEC §jobs.runlog): it
  was accepted, stored and enforced nowhere, so the history's only bound was `maxBytesPerJob`
  times the number of jobs. Past it the oldest records of the whole `logs/jobs/` directory go
  first, whichever job wrote them, until three quarters of the ceiling is left; the hourly
  sweep checks every file, an append the files written since boot.
- A database MCP's reply cut to the output budget (1000 items, 256 KB) is still valid JSON
  (SPEC §mcp.adapters): the cut used to trail the JSON as a prose note, so a caller parsing
  `content[0].text` threw, and one whose fallback was an empty list read the largest answers as
  no rows. The account now rides inside the reply - `truncated: true` and `outputBudget: {
  shown, total, note }` - the list keeps the most rows that fit instead of being halved below
  that, one row wider than the budget keeps its other fields with only the long string clipped
  (`…[+N bytes]`), and a value with nothing to drop comes back as a `partial` string.
- A release build no longer embeds a stale panel (SPEC §panel): an asset edited after the
  last build could be missed and a deleted one stayed embedded, so `touch
  crates/swiss-panel/src/lib.rs` was a standing chore before every release build.
  swiss-panel's build script now tracks the asset tree, the directory for an edit or an
  added file and each file by name for a deletion.
- A password typed into a remote command no longer lands in every run record (SPEC
  §remote.security, §remote.history, §mcp.calls): `REDISCLI_AUTH=<value>` inside a `bash -c`
  word sat in the far side's `ps`, the Runs page, `swiss run audit` and its exports.
  `remote.exec` now refuses a credential typed into argv or cwd as text - an assignment, a
  `--password`-style option, an `Authorization:` header, a JSON member, a URL's password,
  `redis-cli -a`, `sshpass -p`, `mysql -p` - naming the word, never the value, and pointing at
  the vault and `--env`. Records and the MCP call log mask such a value, and the records written
  before are masked at the next start. The remote skill puts `sudo` on the one command that
  needs root rather than on `python3 -` or `bash -s`, and passes a secret through it with
  `--preserve-env`.
- A redis walk through a tunnel no longer dies at half a second (SPEC §mcp.db, §data.redis):
  the connection kept redis-rs's own 1 s connect and 500 ms reply wait under swiss's 10 s
  `commandTimeout`, so through an SSH tunnel a slow `SCAN` round came back "timed out" and
  `redis_scan` and the Data view stopped mid-keyspace. The connect is now 5 s and the
  `commandTimeout` the one bound on a reply. The Data view's key page walks on past rounds that
  match nothing, `COUNT` doubling up to 1000, instead of an empty page and a *More* click per
  round.
- `redis_scan`'s type filter works on a server older than 6.0 (SPEC §mcp.db): `SCAN … TYPE`
  arrived in 6.0, so a typed scan of a 5.x died on its first round with a syntax error. The
  engine asks the server's version once and, on an older one, filters each page by `TYPE` in
  one pipeline.
- `swiss skill install` tries again when Windows denies the staged rename for a moment (SPEC
  §host.cli): with an AI client watching `~/.agents/skills`, the install failed "Access is
  denied" after removing the old copy, leaving no swiss skill there. The remove and the rename
  now retry a denial every 25 ms for about a second.
- A confirmed note in the brief no longer reads "⚠ contradicted by" an unconfirmed guess it
  overrules (SPEC §memory.brief); the guess, where shown, keeps its `contradicts` line, and two
  confirmed notes at odds stay marked.
- A deploy's build stamp names the tree it built (SPEC §host.ops): a `-dirty` mark could
  outlive the stray file that caused it, and an edit after a build kept a clean stamp. The deploy
  script touches `build.rs` when the last build's stamp is not the tree's, and fails before
  stopping production when the new binary is still stamped otherwise.
- The code index holds Java-family methods (SPEC §memory.code): an indented `Type name(args)`
  in a Java, Kotlin, Scala or C# body - past annotations, modifiers and type parameters - is a
  `method` definition, so `swiss memory code find` and the asker's `code` tool say which file
  and line define it. Only types were indexed before, so a question about a method got "no
  definition" and the answer named a file guessed from a class name. The scan has a version
  now: an index an older scan read is read again file by file on its next refresh, and a find
  refreshes an index past its 30 s gap first. The asker places code only where a tool said it
  is, and with no definition names the files that mention the name.
- The code index tells a file that names a thing only in a comment from one whose code uses it
  (SPEC §memory.code, §memory.rag): `swiss memory code find`, the asker's `code` tool and `GET
  /api/knowledge/code/find` (`inComments`, `inCommentsTotal`) list the files whose code names it
  first and the rest apart, as files that do not use it. A notifier whose comment named a job
  was listed like the job's caller, and an answer said the notifier sent the job's push. The
  asker says what a file does only from a definition or lines a tool showed, else calls it a
  file that mentions the name, and applies a document's general rule to one case only when the
  code shows that case using what the rule names. The index's schema is 2: it is dropped and
  built again on its next `code index`.
- A definition or a document section past a redacted private key block is at its own line
  (SPEC §memory.code, §memory.docs): the redaction kept one line for the whole block, so every
  line after it was numbered too low. The code index reads each file again once; a document is
  numbered anew when it next changes.
- A document's card follows the code (SPEC §memory.docs): it keeps the code names it gave, and
  every docs run looks them up again in the refreshed code index, so a definition indexed
  since - new code, or a method the older scan missed - joins the card and a moved one is
  where it is now, with no model asked (`R card(s) re-linked`). A name the index lacked when
  the card was written used to be dropped for good. The store moves to schema 10; a card
  written before is asked for again on the next docs run.
- A recheck shows a line past 2 KiB in pieces (SPEC §memory.init The recheck): its head and
  the stretches around the code names its notes name, `⋯ N bytes not shown ⋯` between them.
  A one-line SQL dump's `INSERT` used to fill a question alone, so the notes about its rows
  were never judged and stayed in the review inbox. A recheck's `⋯ … not shown ⋯` marker a
  model copies into an evidence excerpt now reads as an elision in the code check
  (`excerpt_mismatch`, SPEC §memory.code) instead of holding the note.
- `swiss memory ask` waits up to 20 minutes for its answer, not the 30 seconds every other
  gateway call gets: the answering agent works the question over the memory's tools for
  minutes, and the CLI gave up while the gateway was still answering.
- Daily cron jobs no longer lose days around a gateway restart (SPEC §jobs.triggers). A claim
  kept the clock read's milliseconds, so after a restart the first tick inside the occurrence
  minute could read "before" it and drop the day, surfacing as a `missed` line one day late
  (or a run carrying yesterday's key); scheduling now works in whole local minutes and old
  claims are floored on read. An occurrence that passed while the gateway was down is now
  accounted for at the first tick after start instead of one period later. Under
  `misfire: "skip"` an occurrence more than a minute past due is recorded as missed and never
  run late, while an on-time one runs even after a gap (it used to be skipped with the
  summary); the Jobs history no longer prints "0 more missed".
- A recheck's fix now releases the notes the code check held only for how a model quotes
  (SPEC §memory.code): an excerpt compares across a SQL or JSON string's escape level
  (`lang=\"en\"` quoted as `lang="en"`) and across a `[redacted:<rule>]` marker standing for
  a value the model was never shown, and a name that begins a family of keys in the anchored
  files (`shop_email_coupon` for `shop_email_coupon_name_1..9`) is no unknown name. On
  a large Java project 24 of the 26 notes five rechecks had rewritten without releasing
  pass; the two left misquote for real. Redaction no longer takes a long camelCase method
  (`handleOrderNoticeV1`) for a key, so a note names it instead of the marker.
- LLM proxy validation checks the literal credential spans independently of references
  elsewhere in the URL, rejecting mixed literal/reference userinfo without echoing secrets.
- Disabling Memory closes its notes and code-index database pools on stop instead of
  retaining a factory-lifetime service. Re-enable uses fresh lazy pools over the same data;
  stale callers cannot reopen a stopped service, and closing an unused service creates no databases.

- The memory sync sheet says a recheck's outcome once, in the panel's language (SPEC
  §memory.panel). In Chinese it showed the engine's English summary above the Chinese count
  of the same run. A recheck run, or a full run that was not a dry run, now keeps that summary
  as the hover of its first line. A dry recheck says in words what it would ask, and an empty
  one says that no note waits.
- Opening a memory note takes about 0.3 s, not 1-3 s (SPEC §memory.search). The lexical
  tier's query now runs its full-text MATCH once and looks each hit's note up. SQLite 3.51
  had planned it the other way round: it walked every note of the project and re-ran the MATCH
  for each one. That cost 1.3 s per related-notes lookup on a 2,000-note project, and the
  note's other requests waited behind it. Searches with words outside CJK took the same path.
- The LLM history keeps recording past its 100 MB share (SPEC §llm.history). Once the file
  passes 90% of the share, the oldest calls and finished runs go until the live data fits 70%,
  then the pieces nothing names any more, and the file is compacted. An open conversation and
  its calls are never trimmed. A write refused over the share starts that trim within a minute.
  Before this, only the 180-day sweep deleted anything, so a home whose agents recorded
  146 MB in a week had every LLM call and run refused from then on.
- Memory's map draft ranks a layered project's businesses by the files they own, the
  directories named for them included (SPEC §memory.init). It had ranked them by the files
  their name matched, so a business living mostly in its own directories was cut past the 48
  kept. That was `Pm` in a large layered Java project: a few classes named for it, dozens in `web/req/pm`,
  `web/res/pm` and `remote/pm`. Its files went unfiled and its notes came last.
- A map run that draws a domain files the unfiled notes by their anchors, so notes saved before
  their code had a domain follow it (SPEC §memory.init Redraw).
- A brief given a path no domain owns puts the caller in no domain (SPEC §memory.brief). It had
  fallen back on the task's hits, so `PmService.java` was "in" the live room.
- A styled select's open list follows its trigger when the page or a sheet scrolls, instead
  of staying where it opened.
- A `chat` agent's Test asks the way its runtime does, in every lane: one request with no
  tools and the prompt as given. The live dialogue had offered it the Test's `read`/`list`
  tools and a builtin's turn cap, and the blocking Test filed its run with no turns. A Test or
  memory-agent ask that outruns its deadline now files the dropped round trip under its own
  run instead of as a call no run joins. A spawned agent's Test closes its run row whatever
  the check found, including a program that does not resolve. A live Test whose provider no
  longer resolves is refused with a 400 that names it, not a 200 the event reader cannot parse.

## [0.1.4] - 2026-10-07

### Added

- `remote.exec` takes `stdin`: up to 1 MiB of text sent to the command's standard input
  verbatim, then closed - never quoted, so SQL for `psql` or a script for `sh -s` needs no
  escaping for any shell. `${secret://}` references resolve in it; run records keep only its
  size. Without it stdin closes at once, so a command that reads stdin ends instead of
  waiting out its deadline.
- `swiss remote exec --stdin-file PATH|-` sends a file (or its own stdin) that way; a UTF-8
  BOM is dropped and a UTF-16 file with a BOM is decoded. A `sh -c` script that a local
  shell split at its quotes is refused before anything runs, with the two ways out.

### Fixed

- `swiss remote exec` (and `sync`, `push`, `swiss run logs`) no longer drops stretches of a
  fast command's output: a gap the live window lost is printed from the run record.

## [0.1.3] - 2026-10-03

### Added

- Code colour schemes: every code block (the JSON view, Mongo document cards, the SQL console,
  DDL panes and previews) is an editor surface in IntelliJ Light, Darcula, IntelliJ Dark or
  GitHub, picked from the header's palette button; the default follows the theme. SQL is
  highlighted the way DataGrip shows it - keywords and types, functions, columns, comments -
  and DDL panes number their lines.
- MongoDB connections (`"type": "mongo"`, a `mongodb://` URL, MongoDB 4.4 or later): six tools —
  `mongo_list_collections`, `mongo_find`, `mongo_aggregate`, `mongo_describe_collection`,
  `mongo_command` (server-damaging, session and cursor commands refused; dropping data needs
  `allowDestructive`) and `mongo_inspect` (activity, slow queries, per-collection time, unused
  indexes, storage, server, replication) — plus `mongo://` resources.
- The Data page's documents workspace for MongoDB: collections in Collections / Views / System
  bands, a query bar in the shell's syntax (filter, projection, sort, skip, field completion),
  documents as a list, canonical JSON or a table, an editor that writes back only if nobody
  changed the document since it was read, multi-document writes in one transaction on a replica
  set, bulk update and delete, an aggregation pipeline builder with per-stage previews, schema
  analysis, indexes with usage, explain plans with a collection-scan verdict, validation rules,
  JSON / NDJSON / CSV export and import, and a command console with templates.

### Fixed

- The SQL console no longer splits a statement at a `;` inside a string that follows punctuation
  (`VALUES ('a;b')`), and the highlighter reads `` (`id`) `` as a quoted name.

## [0.1.2] - 2026-10-02

### Added

- `swiss path [on|off]` and a "Put swiss on your PATH" switch in the panel: this exe's folder in
  the current user's PATH on Windows, a `~/.local/bin/swiss` symlink elsewhere. A PATH entry
  added by hand counts as on.
- `pg_inspect` and `mysql_inspect`: one named diagnostic check per call — running statements,
  lock waits with their blockers, top queries, sequential scans, unused indexes and tables
  without a primary key, cache hit ratio, connections, plus vacuum (PostgreSQL) or
  fragmentation (MySQL). A missing privilege is answered with the grant that fixes it.
- `mysql_describe_table`: the table's own `CREATE` statement plus the foreign keys that point
  at it.

### Changed

- `pg_describe_table` answers in one query with the primary key, indexes, foreign keys both
  ways and constraints; `schema` is optional and resolves through the `search_path`.
- `mysql_list_tables` takes a `database` and names it once at the top of the reply.
- `redis_scan` takes `limit` (keys to collect, default 100) instead of `count`, and keeps
  scanning until it has them — a sparse pattern no longer returns empty pages.

### Fixed

- Running the test suite no longer turns start at sign-in off on the machine that runs it; the
  tests write no OS registration of the operator's.
- `swiss autostart` no longer runs its label into the registry location it prints.

## [0.1.1] - 2026-10-02

### Fixed

- The Linux archives are built on Ubuntu 22.04 and run on glibc 2.35 or newer (Ubuntu 22.04,
  Debian 12, and later); 0.1.0's needed glibc 2.38.

## [0.1.0] - 2026-10-02

The first release.

### Added
- The gateway: every MCP an AI client needs on `/mcp/<name>` behind a bearer token — stdio
  children (idle until their first request, reaped after idling), remote HTTP with OAuth, REST
  APIs declared in config, MySQL, PostgreSQL, Redis and zai-vision. **Import .mcp.json** brings
  an existing client config across, and each MCP copies a ready client command.
- The admin panel, embedded in the binary, in English and 简体中文. It needs a sign-in: a
  single-use link from `swiss start` / `swiss open` sets a session cookie that only the panel's
  own pages can use, and the CLI signs its calls with a key rotated on every start (`swiss api`
  for scripts).
- Data: tables and rows with buffered edits, a SQL console with server-side completion, export
  and import, structural operations, live sessions; Redis keys, streams with Follow and
  read-only consumer groups, and a console that completes commands and keys.
- Tunnels: SSH connections, forward rules, proxy dialling and jump hosts, and MCPs that ride a
  tunnel.
- Jobs: configuration-driven scheduled jobs with overlap, misfire and capture modes and a run
  history.
- Terminal: local PTY and remote SSH shells in the browser, with recording.
- Remote execution: targets with declared capabilities, `swiss remote exec / sync / push / cat
  / write / pull` and `swiss run`, a run log with an audit, `/mcp/remote`, and the Targets and
  Runs pages; each target runs in its own lane.
- The secret vault: `${secret://name}` (with an optional `:default`) and `${ENV_VAR}` references
  resolved only at use time; stored values are write-only and masked wherever they could come
  back out, `swiss export` included. Remote addresses stay masked in the panel until revealed.
- Groups on every list, start at sign-in (`swiss autostart`), `swiss update` to check for a new
  release, `swiss export` / `import` to move machines, and the embedded `swiss` and
  `swiss-remote` AI skills (`swiss skill install`).

[Unreleased]: https://github.com/young1lin/swiss/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/young1lin/swiss/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/young1lin/swiss/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/young1lin/swiss/releases/tag/v0.1.0
