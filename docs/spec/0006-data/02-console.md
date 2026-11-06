# The console and the agent

Part 02 of [§data](README.md), chapter 0006 of the [swiss specification](../../SPEC.md).

## §data.console — The SQL console

- **One statement per request** (`assert_single_statement`) is the console's one rule. Reads
  and writes alike go through: the console belongs to the operator's own machine, and no
  statement-shape classification refuses a write. Every module database on the Data page
  (§data.sqlite) is the exception: its console refuses anything but a read.
- **Automatic LIMIT.** No MySQL or PostgreSQL console statement reads without a bound: one
  without a LIMIT of its own gets `with_row_limit` at the console's **row limit** - 500
  (`CONSOLE_ROW_LIMIT`) unless the operator picked another on the status line (100, 500, 1000,
  2000, 5000, 10 000; per browser, `swiss.dbRowLimit`), and never past 10 000
  (`MAX_ROW_LIMIT`). A LIMIT the statement writes itself wins. When the bound actually cut the
  rows the reply says `limitApplied` and the results bar says so (`500 rows · cut at the row
  limit 500`). The reply carries `elapsedMs`.
- **Blocks and batches.** Run and Ctrl+Enter run the selection when there is one, else the
  blank-line-separated block under the caret (`dbRunText`, `dbSubqueryAt`); **Run all**
  (Ctrl+Shift+Enter, Alt+X) runs the whole console. The text is cut into blank-line blocks
  (`dbConsoleBlocks`), each block split on statement-level semicolons, and sent **one statement
  per request**, each answer its own result tab (`dbResultTabLabel`) named by the table it
  reads, a write by its verb and table, else by its first word, plus its row count - a
  reply's `affectedRows` when it carries one, the Data agent's committed write
  (§data.agent Writes), its facts then *Committed · 3 rows changed* and the insert's id:
  `t_user · 10`, `UPDATE orders · 3`, `SELECT · 1`; the tab's tooltip is its statement.
  The first failure stops the batch: the tabs that answered stay, and the failure gets a tab of
  its own in front (`SELECT · failed`: the server's error, the statement, how many after it did
  not run).
- **The layout** reads top to bottom: the head (SQL or Command with the kind's hint on hover,
  Run all, Run, ⋯), the editor, the **results bar** (the result tabs, or the reply's kind when
  there is one;
  its rows · note · ms at the right, said only there; the dock's two buttons after them), the
  grid, and the status line naming the connection. The result tabs sit in a strip that
  scrolls sideways, each on one line - the wheel scrolls it too, and the tab showing is
  scrolled into view - and a list button after it (*All results*) names every result by its
  tab, its statement on hover, the one showing checked, under a find box that matches a tab's
  name or its statement (the rows scroll); a pick shows it. With several results
  the tabs once wrapped onto two lines and the last hid under the facts (2026-10-29; `a long
  script's strip scrolls sideways`).
- **The console's height** is the reader's: the editor and its results are a dock
  (`db-console`, §panel.ui) whose handle is the results bar - a drag on it trades the editor's
  rows for the results' and is remembered per browser; the results keep at least 200 px. Hide
  Results (or a drag past the bottom, or a double-click on the bar) folds the results down to
  their bar and the editor takes the room; Maximize Results gives the results the column (not
  remembered: the next paint shows the editor). A run, and a click on a result tab, bring
  folded results back (`dockShow`).
- **A wide answer.** A result's columns do not sort but size like a table's (the grip's drag
  and double-click, §data.grid), and their widths are kept per connection and column list
  (`swiss.dbResultCols`: FNV-1a of the connection and the column names → {column: px}, the 50
  most recently resized), so the same SELECT - or any answer with those columns - comes back
  at the widths it was left at. With more than one column the results bar carries **Find a
  column** (a search icon after the facts): the result's columns under a find box, the one
  picked brought into sight.
- **Explain.** Explain and Explain Analyze prefix each statement (idempotently); the driver's
  own error is passed through.
- **History** is the server's, per database (§data.history); **favourites** live in
  localStorage (`swiss.dbFavorites`, 50).
- **Format** (⋯ Format, **Ctrl+Alt+L** / Cmd+Alt+L as a JetBrains IDE has it, Shift+Alt+F as
  VS Code does; read by `code`, AltGr+L left to the letter it types) lays SQL out the way
  DataGrip's default code style does (`dbFormatSql(text, {dialect, indent})`, its own module
  `data-sql-format`): every clause at the query's column; a select list (and RETURNING, SET,
  VALUES rows, WITH's CTEs) one element a line aligned after its keyword, modifiers (DISTINCT, a
  hint comment) on the keyword's line; GROUP BY and ORDER BY on one line while they fit 120
  columns; a join four past the tables with ON on its line; AND/OR right-aligned under
  WHERE/HAVING (a BETWEEN keeps its AND); a subquery laid out the same way at the column after
  its bracket; a CASE with one WHEN on its line when it fits, else a branch a line with END
  under the WHENs; a comma list past 120 columns wrapped under its bracket; `delete` over its
  `from`; `create table t` over a column a line; EXPLAIN and CREATE VIEW ... AS over their
  query. Spacing: none inside brackets, before , ; or around . and ::; a function on its
  bracket, a keyword or a table name spaced from it; one space around a binary operator, a
  unary one on its operand. It changes **only whitespace**: the lexer knows the dialect
  (MySQL's backslash escapes, # comments, @ variables and ?; PostgreSQL's $$ quotes, $1, E''
  and operators like #>>), case, strings and comments ride byte-identical, a blank line (where
  Run splits blocks) stays one, a statement of another kind keeps the lines it was written on,
  and the output is lexed again: a token or a blank line moved hands the text back untouched.
  A selection formats alone, at the column it starts on, and stays selected; else the caret
  keeps its place among the words. The edit goes in through the box's own `insertText`, so
  Ctrl+Z takes it back. A MongoDB command formats as a document; a Redis console has nothing
  to format.
- **Templates.** Generate SELECT / INSERT / UPDATE / DELETE for the open table.
- A statement that touches the schema drops the completion cache.

## §data.history — What a console ran, per database

Agent reads, persisted analysis executions and permitted automatic writes use the same durable
history. The host carries an optional execution observer into each agent SQL operation; an adapter
marks the actual statement just before submitting it. Validation refusals, drafts and console edits
produce no run. A submitted failure and a submitted write rolled back by its row cap are failed
attempts, with the driver's result or rollback reason. The statement is the SQL actually submitted,
including an applied limit. One observation writes one row; callers do not also append it.

Every statement or command a console sends - SQL, a redis command line, a mongo command
document; runs, explains and failures alike - is filed by the gateway when its route answers,
in `logs/data.db` (owner `data`, §host.history), never in the browser. A row is
`{id, ts, conn, dialect, stmt, ok, cut?, error?, rows?, ms?}`.

- **Filed by the database reached, not the connection name.** A run's `target` is the
  browser's own label for the database the request really reached: `app @ host:3306` (MySQL,
  pg), `host:6379 db 0` (redis), `<db> @ <hosts>` (a mongo command's `db`). A pg schema switch
  is not another database and a MySQL console runs on its configured one, so no statement is
  listed under a database it did not run in; two names pointing at one database read one list,
  each run saying which name sent it.
- **Kept**: the newest 100 000 rows (`KEEP_ROWS`), within the history's 50 MB share; a
  statement past 64 KB keeps its head and says it was cut. A refused write warns once and never
  fails the run it records.
- **Routes**: `GET /api/db/{name}/history?q=&before=&limit=&db=` answers `{target, runs,
  more}`, newest first, 100 a page (at most 500); `q` matches words inside a statement,
  `before` is the oldest id shown, `db` names a mongo connection's database. `DELETE` the same
  path forgets that database's runs only: `{target, cleared}`. Without the store, 503.
- **In the console**: the ⋯ lists this database's last eight distinct statements (read when
  the console opens on a database and after every run) and **All history…**, a sheet of
  every run as one line (state dot, the statement, rows · ms · `via` another name, age; a
  failure's words under it) that searches, pages back (**Load earlier**), clears this
  database's list, and opens a run to its whole statement and **Open in console**.

## §data.agent — The agent beside the console

`open_console` accepts `tabId` alone. An unknown ID fails; a known closed console is restored
under that ID before any editing. A new console receives its ID before the tool answers and enters
the running conversation's tab snapshot, so later tools in the same send can address it. This also
covers purpose-named SQL run/stage creation and restoration. Raw SQL
uses the same blank-line block indices as the panel; semicolons within a block remain intact.
An oversized console's statements may be omitted from the bounded Agent snapshot, explicitly
marked as omitted; the model may open that console but cannot read or edit unknown indices.
An incomplete tab list is reported and cannot justify creating an apparently missing tab.
Agent-owned SQL consoles always display through the dialect formatter. The stored SQL is the
canonical text; formatting is a view and never triggers a save. Manual consoles retain exact
bytes. Redis and MongoDB do not go through the SQL formatter. Opening never executes a statement.

Consoles keep `createdAt` and `updatedAt` in epoch milliseconds, independently of the monotonic
LRU stamp and of the closed-history `at`. Only a changed name or text updates `updatedAt`.
Restoration preserves both times; old entries with missing or invalid times display unknown.
Recently closed is sorted by valid update time descending, then close time and stable ID. It is
grouped by browser-local calendar day (Today, Yesterday, then date), with unknown updates in their
own final group. Search removes empty groups. No within-day time windows are used. A row exposes
its SQL summary and update time; its details include ID and creation, update and close times.
A console holding no text (whitespace counts as none) is not filed, whatever its name or owner:
closing one forgets it, its record goes with a REMOVE at its revision, and its ID then answers as
unknown. A clean tab whose record changed elsewhere files the newer record instead, and a draft the
store refused keeps its tab and conflict bar. A blank closed record an older page filed stays out of
the list, and the connection's first read in a page removes it the same way. An agent edit that
deletes a closed console's last statement takes it off the list. An open blank console stays on the
strip across a reload. Closed consoles deduplicate by connection and ID, and two consoles holding
identical SQL stay separate. The 200-entry and one-million-character bounds reject a close that
would exceed them, with a storage warning; they never silently discard an older console to make
room. A failed history removal prevents restoration and warns; a failed clear warns and retains the
history.

A MySQL, PostgreSQL, SQLite, MongoDB or Redis connection's page has an agent at its end edge: the
reader asks in their own words, and the agent reads how the data is laid out (tables,
collections, keys), writes the statement or command for this connection, runs it and shows the
result on the page - the reader does not have to think the query through. Each kind has tools
of its own (below) and every kind the connection's memory; the pane, the conversations, Queue,
History, Memory and Compact are one.
Code: `crates/swiss-plugins/src/llm_db/` (snapshot, tools, routes), `llm_db_memory.rs` (the
memory), the `db` runtime and its prompt in `crates/swiss-llm/src/agent.rs`, and the panel's
`data-agent.ts` and `data-agent-memory.ts`, with `dbAgentStatement` in `data-sql.ts`.

- **The pane.** Folded until the reader opens it - the head's sparkles button, or the edge's
  strip - then a column after the page's main one (`chatPane`, §panel.ui), its width and fold
  remembered (`grip` key `db-agent`, 280 px up to 60 % of the window). A connection of any
  other kind has neither the pane nor the button. It needs the LLM plugin and a **db**
  agent (§llm.agents): the runtime made for this page, which nothing else runs, and no task
  agent is ever borrowed. The pane offers the db agents (the picker stays visible beside the connection, remembered as
  `swiss.dbAgent`; first `db`, else the first listed); with none it says what one is, and
  *Make a DB agent* opens the Agents page's Add sheet preset to the db runtime and named `db`
  (the name only while free); it says why when the plugin does not answer - read once per visit,
  never retried by its own repaint. Enter sends, Shift+Enter breaks the line. The words box
  starts at two rows and grows with its words up to 40 % of the window, then scrolls
  (`chatPane`'s own, §panel.ui), so a long question stays readable whole. Streaming text and
  thinking follow the bottom while the reader is there; scrolling up pauses following and
  reveals **Back to bottom** above the composer. Its press resumes following, as does an
  explicit send; queued sends preserve a reader's position. Work opens while running and
  folds as soon as the final answer arrives or Stop is pressed, without waiting for the
  transport to close. Stopped partial answer text stays visible outside the folded work.
- **First ask and tools.** The first screen has a short introduction and no fixed question buttons.
  The header puts the connection beside the DB-agent picker. Connection identity is not repeated
  below the conversation. One input card contains the words box and its bottom toolbar: write
  permissions at the left, model and one round button at the right: Send, *Queue* while a reply
  streams and the box holds words, the stop square while it streams and the box is empty (or a
  compaction runs). The header stays one line at any width: the connection ellipsizes, then the
  caption gives way, and the tools never wrap under it. Editable page references stay
  above that card and take no space when absent.
  The folded Tools summary shows a count; opening discloses localized names and descriptions.
  Completed process folds; the answer, thinking and tool details retain their transcript behavior.
- **Next-send references.** The composer shows its fixed connection plus removable database,
  object, SQL, analysis-result and selected-row references. Removing one preserves source selection
  and drafts. Console tools still see their separately bounded tab snapshot. Ordinary selected
  rows come only from the visible result page: at most 20 object rows and 16,000 encoded bytes.
  Analyses use stable row IDs resolved against their frozen result. SQL is capped at 20,000
  characters, object/database names at 256. The server rejects mismatched connection ownership,
  unknown fields and excessive counts. References are quoted data, explicitly never instructions.
  Analysis row selection depends on including that result; the UI shows effective inclusion.
  The first send shares the complete selected references. Later sends append only changed
  fields; `null` removes a current reference while earlier shared data remains historical.
  Equality is checked against consumer tags in the canonical replay, never parsed from user
  text or a browser-only cache. Tags contain only changes, are sealed with their user turn,
  and never enter the provider wire. Reload and Continue retain that basis; New, compaction
  and old untagged history establish a fresh snapshot without rewriting earlier messages.
  The page shows what went the same way live and restored: the reader's words, and under them
  one quiet fold, "Sent with: database · table · SQL · rows · analysis", holding where the
  patch points, the statement and the rows. Live, the stream's `references` frame (right after
  `run`, only when a patch went) carries the patch; restored, the page splits the appended
  blocks off the stored prompt by their fixed leads - display only, never a basis for the
  next patch. A removal (`null`) shows nothing; a send that changed nothing has no fold; the
  turn's Copy gives the words alone.
  Agent-owned SQL consoles display formatted committed text, including restoration and edits.
  The page never writes formatting back. SQL execution receives the original `sql` argument;
  `description` becomes a single `-- description` line above its saved/displayed statement.
  Manual drafts retain their exact bytes and their normal compare-and-swap protection.
- **The prompt is the db runtime's.** The conversation opens under `DB_AGENT_PROMPT`, the
  agent's own prompt after it, a blank line between (a house style, a language to answer in);
  the agent's provider, model and effort run it, without a fixed round limit. After the prompts an `# Environment` snapshot names the connection, its
  dialect with that dialect's notes (identifier quoting, `_` and `%` as LIKE wildcards and
  their escape, top-N per group through `ROW_NUMBER() OVER (PARTITION BY …)`), the schema in
  view on the page, its tables - every name when there are 100 at most, else only how many,
  `list_tables` finding them by name (a thousand names would ride every request of the
  conversation) - the date and the agent. On MongoDB the snapshot names the database in view
  (the open collection's, else the sidebar's: the tools' default, else the connection's own),
  its collections by the same rule (`list_collections` finding them past 100) and the
  connection's other user databases (50 named at most); the notes are a command document's -
  never shell syntax, Extended JSON values, a `$regex`'s escapes, top-N per group as an
  aggregate. On Redis it names the logical database and how many keys it holds, any other
  database holding keys being out of this connection's reach, then what pub/sub is up to -
  how many channels have a subscriber (the first 20 named) and how many pattern
  subscriptions, or why that went unread; the notes: one command line, never KEYS, SELECT
  refused, writes and scripts staged, a stream a key of type `stream` (`read_stream` reads
  it), a channel no key at all (`list_channels`, `listen_channels`), SUBSCRIBE and XREAD never
  a command line. The prompt also has the agent keep at it: a timeout or a dropped link is
  tried again - twice more at least - before it changes course, an error naming its cause is
  fixed and run again, a blocked way gives way to another route to the same data, and it
  never ends its turn to offer a read or a retry it could make itself; it stops when the
  result answers, or when the same wall stopped attempts of different kinds, saying what it
  tried and what each returned. A conversation once ended on three listens that timed out,
  offering a retry instead of making it (2026-10-29). Its *# How to reply* sets the answer's
  form: plain prose in the user's language - no emoji or decorative symbols, no exclamation
  marks, no filler or praise - the answer first, headings only for a reply of several parts,
  bold for a key figure at most, a table only for data that is tabular; when the result
  answers, the statement or command it last ran, as it ran it, in a code block (`sql`,
  `json`, `redis`), then a few lines on what it shows; no narration of its process or of
  notes it read or saved, no closing menu of offers, and asked what it can do, a few plain
  sentences. An answer once opened on emoji and laid what it could do out as a table
  (2026-10-29). `DB_AGENT_PROMPT` is static - the same bytes heading every connection's
  conversation, what differs (the agent's prompt, the snapshot, the memory index) after it -
  so a provider's prefix cache can reuse it. A continuation replays the system it
  noted, byte for byte (§llm.history), so a conversation open from before a change to the
  prompt needs New (`the_snapshot_names_a_few_tables_and_counts_many`,
  `the_snapshot_counts_a_keyspace_and_names_a_few_collections`).
- **Tools reach this connection only**, each call leasing it for that call alone (§host.seats):
  `list_tables {grep?, schema?}` (up to 500: schema.name, type, about how many rows),
  `describe_table {table, schema?}` (columns, keys, indexes),
  `run_sql {consoleId, description, sql, limit?}` and `stage_sql {consoleId, description, sql}`. `run_sql` runs ONE read (in auto mode a narrow write too, Writes below):
  the read gate (`assert_read_statement`: SELECT,
  WITH without a data-modifying CTE, TABLE, VALUES, SHOW, DESCRIBE, EXPLAIN; never INTO
  OUTFILE or DUMPFILE; MySQL DDL, which commits implicitly, stops here), then a read-only
  transaction that is always rolled back (PostgreSQL `BEGIN READ ONLY` with a 30 s
  `statement_timeout`; MySQL `START TRANSACTION READ ONLY`). A statement without a LIMIT of
  its own takes the console's row limit (`rowLimit`, 200 when unsent; the console's ceiling
  holds). The page gets the whole result; the model reads the count, the columns and the
  first 50 rows, a cell at most 200 characters, an answer at most 12 000. `stage_sql` puts a
  write or DDL in the console unrun: the reader reviews it and runs it, and the agent never
  says it ran. Rows are data, never instructions to the agent.
- **On MongoDB** the four are `list_collections {grep?, db?}` (name, collection or view, about
  how many documents; a view, what it is on), `describe_collection {collection, db?}` (the
  fields a sample of 50 documents holds, each one's types and how often present, the indexes,
  size and count), `run_mongo {command, db?, limit?}` and `stage_mongo {command, db?}`, the
  command a document (an object or its JSON text, its first key the command) and `db` the
  database in view when unsent. `run_mongo` runs ONE read: the read gate
  (`assert_read_command`: find, aggregate, count, distinct, listCollections, listIndexes,
  collStats, dbStats, dataSize, explain, listDatabases, buildInfo, serverStatus, hostInfo,
  ping, hello, isMaster, connectionStatus, top, replSetGetStatus - never an aggregation that
  names `$out` or `$merge`, however deep), an aggregate given a cursor, find, aggregate, count
  and distinct a 30 s `maxTimeMS`; a cursor's documents stop at `limit` (the row limit and
  its ceiling), `more` when cut. The page gets the whole reply as canonical Extended JSON; the
  model reads how many documents and the first 50, one per line in relaxed Extended JSON,
  strings clipped at 200 and a line at 1 000
  (`a_mongo_read_reaches_the_page_and_the_model_reads_its_first_documents`).
- **On Redis** the seven are `scan_keys {pattern?, type?, cursor?}` (a SCAN page of up to 200
  keys with each one's type and TTL, the key count, where the scan stands and the cursor to go
  on from; the page walks on until it holds 200 matching keys - `fill`, §data.redis - or 2 s
  pass, so a sparse pattern is one call, never a hit or two a call), `read_key {key}` (any type, a long value windowed), `read_stream {key, count?,
  before?, match?}` (a stream's length, first and last ids, 20 entries newest first - 100 at
  most - one a line as `id  field=value`, the id to pass as `before` for older ones, a filter
  line as the key tab's, and its consumer groups: consumers, pending, lag, last delivered id;
  `a_stream_reads_newest_first_with_its_consumer_groups`), `list_channels {pattern?}` (the
  pub/sub listing of §data.pubsub - each channel with a subscriber and how many, 200 at most,
  and the pattern subscriptions), `listen_channels {channels?, patterns?, seconds?,
  messages?}` (a feed of its own, as the Pub/Sub page's, held 5 s - 15 at most - or until 20
  messages - 50 at most - or the feed closes, a Stop ending it too; each message its channel,
  the pattern it matched and its payload cut at 300 characters, a binary one as its size, and
  what the feed dropped; a subscribe that times out is tried again, three tries in all, and
  the third failure says the link to the server is busy - an error of any other kind fails at
  once; `the_channels_and_what_a_listen_hears_read_as_lines`), `run_redis
  {command}` and `stage_redis {command}`, the command ONE line as the console reads it.
  Nothing a listen hears is kept: a quiet channel answers no messages. `run_redis` runs it
  through the read gate (`assert_read_command`): the reads it names - the string, hash, list,
  set, sorted set, stream, geo, bit and HyperLogLog reads, SCAN and its kin, TYPE, TTL, DBSIZE,
  PING, INFO and the like - and a container's read subcommands (OBJECT ENCODING, XINFO STREAM,
  MEMORY USAGE, CLIENT LIST, SLOWLOG GET, PUBSUB CHANNELS, NUMSUB, NUMPAT and their shard
  forms, ...); KEYS is refused for SCAN, and CONFIG, SELECT, SUBSCRIBE and its kin, PUBLISH,
  writes and scripts never run; the console's own guard holds after it. An array past the
  console's reply cap comes back cut, saying so
  (`a_redis_read_reaches_the_page_and_a_scan_says_where_it_stands`).
- **It works on the page.** Every statement lands in the agent's own console tab:
  purpose-named for SQL, the default **Agent** slot for commands (Tabs, below)
  - each as its own block after the ones before, the caret
  in the newest so Ctrl+Enter reruns it unless the reader is typing there; the reader's own
  consoles are never touched. A tab the agent opens or addresses comes to the front without
  taking the keyboard focus, so the reader's words stay in the ask box while a run lands; the
  reader's own + or restore does give the console the caret. A statement ends with its `;`; a
  MongoDB command lands in the console's shell syntax (its Format), a Redis command as its line.
  A run's reply opens as a
  result tab like a Run's (a failure as the failed tab), the runs of one send gathering as
  tabs with the newest in front, a send's first run in a tab starting that tab's over; the
  agent's run supersedes a Run still in flight. A MongoDB reply is the console's code block, its tab *find posts* (the
  database before it when not the one the sidebar shows), the meta line the documents it
  brought and where the row limit cut it (`limitApplied`); a Redis reply is the console's,
  its tab the command and key, an array's item count on the meta line. What the agent reads
  on Redis shows too, the last read in front: a key `read_key` or `read_stream` read opens
  in its own key tab (a missing key opens nothing); `list_channels` opens the Pub/Sub page
  (§data.pubsub) on its listing, the agent's pattern in the box and the page's poll following
  it; `listen_channels` opens it with the names listened on added to the watch set - the
  page's own feed listening on them from there while the page is in front - and what was
  heard in its log under one *Agent* line (how long, on what, how many, what was dropped).
  That feed is the page's alone - it outlives the listen that opened it, and nothing on this
  side holds a handle on it - so `stop_listen {}` asks the page to end it, exactly as the
  reader's own Stop does: the subscription goes back to the server, the log keeps what was
  heard before it under a line saying the agent ended it, and the answer says the ask went
  out, never that the feed is confirmed closed. Before this, a page left listening with the
  agent's names in its watch set kept the subscription up with nothing in the conversation able
  to end it (2026-11-01; `the agent can end the reader's live feed`).
  What it finds and describes on MySQL, PostgreSQL and MongoDB shows the same way: a
  `list_tables` or `list_collections` puts its grep in the sidebar's search (none clears it;
  a MongoDB listing only while the tree shows that database), so the tree is the listing the
  agent read; a table `describe_table` read opens in its tab on Structure, and a collection
  `describe_collection` read opens on Schema, sampled there as Analyze samples it (a sample
  already in hand is kept). A refused call opens nothing, and a full strip that refuses the tab
  leaves the tab in front alone. The two pairs had shown nothing, so the reader asked whether
  to drop them; they stay, since they are cheap, the same on every dialect and the snapshot
  points to them past 100 names, and the page now shows what they read (2026-10-29;
  `a_listing_and_a_description_show_on_the_page`, `what the agent finds shows as the tree it
  found`).
  A prompt naming pub/sub once ended on a `run_redis` result, the channels the agent read
  never shown; the reader wants what the agent reads visible as it reads (2026-10-29;
  `a_redis_read_reaches_the_page_and_a_scan_says_where_it_stands`,
  `the_channels_and_what_a_listen_hears_read_as_lines`, `on Redis what the agent reads opens
  on the page`). Each of the four tools' descriptions tells the model so. The dialogue streams
  as the Agents Test does (§llm.panel): thinking, tool steps and words in one flow. The
  reader's question shows as typed, never read as markdown (a pasted `\_` or `*` stays),
  and the answer closes with the statement or command last run, exactly as run, in a code
  block (`sql`, `json` for a MongoDB command document, `redis` for a Redis line), then a
  few lines on what the result shows - not the rows again, the page has them.
- **Tabs.** SQL `run_sql` and `stage_sql` require `consoleId`, `description` and `sql`.
  First create a purpose-named console with `open_console {tab}`, whose JSON answer includes
  `consoleId`, or select an existing ID from `list_console_tabs`. `open_console {tabId}` restores
  that ID; supplying only a name always creates a new SQL console. Names are display labels
  and may be duplicated; execution only addresses an existing ID, surviving rename and close.
  Creation requires one purpose line of at most 40 characters, never generic Agent, SQL,
  Console or Query. Related queries reuse the ID; a different question or write workflow gets
  its own console. `description` is one nonempty line of at most 500 characters in the user's
  language, saved as `-- description` above the SQL; it never enters database execution.
  Newlines and other control characters in the description are refused before any effects.
  SQL run/stage accept neither `console`, `tab`, `tabId` nor `format`. SQL may use arbitrary
  whitespace: the application formats the display, while execution receives the original SQL.
  SQL layout and display formatting are not discussed in the agent prompt or run/stage tool instructions.
  Explicitly targeting a manual console appends there and marks it agent-managed, preserving
  its existing stored bytes. MongoDB and Redis retain optional `tab`/`tabId` and their default
  Agent slot. Names and IDs are validated before any side effects.
- **A tab's id.** Every console tab carries an id (the strip's own, §data.tabs), and the tools
  address a tab by it: `list_console_tabs {grep?, state?, limit?, offset?}` pages the
  connection's tabs - 50 by default, at most 200 - open ones first, then the ones still in
  Recently closed, each newest first (a page snapshot keeps the page's order). `grep` matches
  a tab's name or any of its statements
  (case-insensitive) and shows the first matching line of each tab that matched by statement;
  `state` is `open` or `closed`. The answer states the totals, the shown range and
  `More with offset=N.` when more remain; a row is never cut, and the answer stays within the
  answer budget. `read_console_tab {tabId | tab, offset?}` reads one in whole statements, each
  with its 0-based index, its text and the name its first line comments it with; a long console
  pages by statement under the same budget and ends with `More with offset=N.` when cut.
  `edit_console_sql {tabId, index, sql}` replaces
  exactly that statement and `delete_console_sql {tabId, index}` drops it, the tab staying -
  empty, when that was its last. `open_console` takes `tabId` beside `tab`, and wins: a closed
  tab comes back under its own id, its statements with it. The id outlives a rename, the close
  of a console holding text (Recently closed files the tab under its own id and restores it with
  that id), another console of the same name and a page reload; a name outlives none of those,
  and two tabs called *SQL* were otherwise one address. The server owns the consoles. Each one
  is a sealed record under
  `llm/consoles/` (the sibling of `llm/analysis`): the connection and the id are hex-encoded, a
  component past 200 hex characters is split, and a short path already on disk is still read.
  A record holds the id, the title, whether it is the agent's and the name it knows, whether it
  is open, the text, a monotonic revision, `createdAt`, `updatedAt` and `closedAt`. Those
  survive a reload and a gateway restart. A read creates no file, and nothing new is started to
  hold them (`sealed_consoles_survive_store_restart_with_identity_close_state_and_cas`).
  DATA.CONSOLES on the event bus (SPEC §bus.registry) carries every read and write, after
  the catalog leases the connection. LIST takes `{conn, after}` and answers records in id
  order, cut before 2 MiB, with `next` the last id or empty at the end. PUT takes
  `{conn, console, mode}`: auto creates a missing record and compares an existing one;
  create refuses an existing id; update refuses a removed record. The write compares the
  revision and refuses a stale one. Refusal faults retain `console.exists`, `console.removed`,
  `console.changed`, `console.changedDelete` and `console.tooLarge`; a connection the catalog
  will not lease answers `console.connection` (the module is serving, so never
  `bus.unavailable`). The page says the code in the reader's language (SPEC §panel.i18n), and
  keeps the draft and its conflict bar.
  REMOVE compares the revision, refuses stale deletion, and answers `removed: false` for
  an absent record. CLEAR_CLOSED removes only closed records; open work stays. Every write
  publishes CHANGED, REMOVED or CLEARED while holding the store's write lock, so events leave
  in commit order. The requesting page's id is the origin; agent saves use an empty origin.
  The LLM plugin instance owns the handler guard: stopping it unregisters DATA.CONSOLES and
  later requests answer `bus.unavailable` (SPEC §bus.requests).
  The page reads a connection on entry, puts the open records back on the strip, fills Recently
  closed from the closed ones holding text (§data.tabs), and imports older local drafts holding
  text with `create` only, once, never over a record the server already has.

  **Another window on the same connection** (the desktop GUI beside a browser tab).
  Subscribe, then read: `applyServerConsole(conn, record)` reconciles events, the first read
  and reconnect reads by revision (SPEC §bus.topics). Duplicate, late and already-applied
  records do nothing. There is no focus or visibility catch-up:
  - a clean tab takes a newer text, title and revision, keeping the caret;
  - a clean tab closed elsewhere closes here and enters Recently closed;
  - a console opened elsewhere joins the strip without taking the front;
  - a draft is never rewritten: a stale save meets the conflict bar;
  - a pending tab matching the server's saved state adopts its revision and becomes clean,
    including a save whose response was lost before reconnect;
  - REMOVED drops a clean tab and its history entry, keeping drafts; CLEARED drops closed
    records only. A reconnect read also reconciles deletions missed while offline.

  One person has two views of the same work, so nothing is lost or merged silently
  (SPEC §bus.editing). A local close, restore or conflict resolution bumps the epoch and
  drops an older read whole. Closing a clean tab changed elsewhere reads and files the
  server's newer revision on `console.changed`, retaining the close retry and conflict bar.

  Typing debounces 500 ms per connection; editing another
  connection cannot cancel a parked draft's save. A send flushes first and is
  not sent when that flush fails: the words stay in the box (`dataAgent.consoleSyncBlocked`).
  With `consoleStore: true` on `POST /api/llm/db/stream` the tools reload the records before
  each call and commit the text before the frame, so a page that has gone still leaves the
  console (`separate_agent_requests_resume_the_server_owned_console_by_identity`). A snapshot
  that carries statements and not the original text is refused and writes nothing
  (`store_refuses_to_reconstruct_manual_console_from_statement_snapshot`). With the store on,
  the page sends no `consoleTabs`: the tools that address a tab read the store, which is what
  a later send and a restart read. Each console frame
  carries the committed `console` record and `consoleRevision`. SSE and bus records share
  one revision gate: either may arrive first; duplicate and older snapshots do nothing.
  Result frames add results only, never append the same SQL again or save formatted text.
  Thus overlapping tool calls cannot create an artificial human-draft conflict. Actual human
  drafts still keep their conflict bar rather than being overwritten. Neither tool runs a statement: an edit or a
  delete is text, not execution. Before this a tab could only be named and the statements inside
  one could not be touched at all (2026-11-01; `is minted at birth, survives the close into
  Recently closed, and the snapshot lists open and closed tabs alike`, `two consoles sharing a
  name are told apart by id`, `edit and delete reach a tab that is closed`, `a tab the reader
  renamed answers to its id`).
- **Inspect the model instructions.** Above the conversation, Tools and System prompt (current)
  start collapsed. Each tool expands to its actual model-facing description and complete
  parameter JSON Schema, fetched on demand from the execution definitions for this connection
  kind. The current prompt reads the run's saved system note, including the original connection
  and memory snapshot; before the first send it shows the base and says the snapshot is pending.
  Reopening or starting New selects that conversation's prompt; late reads cannot overwrite
  another run or generation. Updating defaults, the agent prompt or tools never makes saved
  dialogue unusable: Continue and compaction keep the recorded prompt and messages, and the
  next send uses the current tools. Inspection shows prompt/tool differences as notices;
  the latest default and agent prompts expand separately for comparison. Work summaries count model **steps**, matching the footer, while
  user sends remain **turns**.
- **Its account under the box** (`chatStats`, §panel.ui), centred: the turns the reader
  took, the steps the model ran and its output speed, opening Session statistics (LLM time,
  tool time, the average time to first token, tokens per second); the tokens the
  conversation used and the cache's share of them, opening Token usage (each bucket
  exact); and the context ring - how full the next request starts against the agent's Max
  context (`maxContextKiB`, read as 250 tokens a KiB: the default 1024 KiB reads *256K*, a
  fixed figure; the server still holds the budget in bytes), or against the picked model's
  context window where its provider entry states a smaller one (§llm.providers) - the window
  is where the provider refuses, opening on what fills it (the
  system prompt with the snapshot, the connection kind's tool definitions, the messages)
  under a note: a send past the budget is refused, `/compact` carries the conversation on
  in less. The used figure is the provider's own count of the last round - its request and
  its answer, every input bucket and the output, so nothing is guessed. Estimates read with
  `~`: before the first provider count, or after compaction when nothing has counted the
  checkpoint yet and the last count over its bytes estimates it. A send counts a turn, each `usage` frame a step with
  its tokens, time and time to first token, each `toolResult` frame its tool's time; a
  restored or reopened conversation counts its turns and its calls' tokens, times and
  `ttftMs` from the dialogue, a tool's time as the gap between its answer's turn and the
  turn before it, and the ring's parts from `GET /api/llm/db/context`, asked again after
  every send, compaction and Continue here. With no runId the same route weighs the base
  prompt and tools without querying the database or calling a model, so the percentage is
  visible before the first send. Until a provider count calibrates tokens per byte, the same
  250 tokens/KiB conversion estimates usage against the smaller of the agent budget and model
  window; estimated percentages carry `~`. New clears the turn/token totals and reweighs this baseline;
  the first send adds the connection snapshot (`under the box the conversation's account`,
  `shows the base context percentage before the first message and after New`).
- **Copy.** Each turn ends on a Copy (`copyBtn`, §panel.ui) - the reader's question as typed,
  the answer whole as markdown - and each code block in an answer has its own, the statement
  exactly (`each turn closes on its Copy`).
- **Each connection keeps its own conversation.** The run files under task `data.agent` with
  the connection's name as its subject (§llm.history); the one-open rule, a continuation and
  the open lookup all key on that scope, so two connections never share a dialogue, a run id
  sent from another connection reads as `gone`, and the agent's Test never joins one (its
  restore skips a run with a task). The page keeps one column per (agent, connection): a
  switch shows the other connection's, and a frame that arrives for a connection the page
  has left fills its own column but never touches the page.
- **Restore, New, Stop.** Opening the pane on a connection asks for its open conversation
  (`GET /api/llm/db/conversation`) and paints its dialogue; **New** closes that run
  (`history/runs/{id}/close`, the record kept) and starts clean; **Stop** (the round button while
  the box is empty) cancels by run id
  (`agents/test/cancel`), the turns already filed staying filed. One stream per agent and
  connection (409 `busy`); a send that meets an open run it did not know (409
  `open-run-exists`) reads that run again instead of failing silently.
- **Queue.** Words sent while a reply streams wait above the box in a queue (`chatQueue`,
  §panel.ui) - the round button reads *Queue* once the box holds words, Enter queues - and are
  posted nowhere until the
  reader steers them: ↑ on a row puts it first and steers it now, Ctrl/Cmd+Enter queues the
  box's words and steers every waiting row. A steered row reaches the loop between its steps
  as the Agents Test's do (`agents/test/steer`, §llm.api, posted once the run frame named the
  run), and leaves the queue to land in the dialogue as the reader's turn when the loop takes
  it. Each row can be edited in place or deleted; deleting a steered one withdraws it
  (`steer/withdraw`). After a clean answer the first row goes as the next ask, by itself - one
  per turn, in order (a row being edited goes when its edit ends); a steered row the loop
  never read is unmarked and waits first in line. Stop or a failure leaves the queue to the
  reader, the box untouched; ↑ with nothing streaming sends that row now; a compaction takes
  none.
- **History.** The head's History button shows, in place of the column, the connection's
  conversations with this agent, newest first, twenty at a time (`GET
  /api/llm/db/conversations`, *Older* for the next page): each one a flow step - its dot
  open, closed or failed, its first question (a compaction's *compacted from run N*), when
  - the one in front marked. Opening one reads its whole dialogue; *Continue here* makes it
  the connection's open conversation (`POST /api/llm/db/reopen`): the one open before
  closes, kept in History, and the reader asks on. A failed conversation, one that left a
  round unresolved, another connection's, or one opened under a prompt changed since cannot
  go on (409 `gone`, `context-changed`; both runs stay as they were), and nothing reopens
  while a reply streams (409 `busy`).
- **Memory.** What earlier conversations on a connection learned about where its data lives and
  how to reach it - a live room's bound user sits in `live_room.anchor_uid`, then `users` by
  that id - kept so the next conversation walks the route instead of finding it again. Not
  swiss memory (§memory): no graph, no embedding, no sync - a folder of markdown per
  connection under the home, `llm/db-memory/<connection>/` (the name as it is when it is
  plain, else a cleaned one and an 8-hex hash), one note a file, `<name>.md`: a front matter
  with its `name`, its one-line `description` and when it was `updated`, then the markdown
  body. `MEMORY.md` beside them is the index swiss rewrites on every change, for a person
  reading the folder; what the agent sees is built from the notes themselves, so a note edited
  by hand is never stale and a file without a front matter reads as a body. A name is a
  kebab-case slug (ASCII letters and digits, at most 64; `memory` is the index's own), a
  description one line of at most 200 characters, a body at most 8 000, a hundred notes a
  connection; a write lands whole (a `.partial` file renamed). Every kind's agent takes
  three tools beside its own: `read_memory {name?}` - the index, or one note whole -
  `save_memory {name, description, body}`, replacing the one of that name, and
  `delete_memory {name}`; save's description asks for the route, never the data: no values
  read from rows, no personal data, no credentials. A new conversation's opening carries the
  index after the snapshot - *## Memory of this connection* and a line on what follows, then
  a line a note (its name and description) up to 6 000 bytes and how many more
  `read_memory` lists - so the agent checks it before it explores. When to read, save,
  correct or delete a note is `DB_AGENT_PROMPT`'s *# Memory*, static where the index is not:
  save rarely, most conversations saving nothing - only a route that took several lookups
  and that a query it ran verified, a pitfall that cost a failed attempt, or what the user
  asked it to keep; never what one list or describe call shows, a route it did not run or
  an overview of what it can do; one note a conversation at most unless the user asks for
  more, correcting the note on that kind of question rather than adding a second; never a
  word in the answer about a note read or saved. `save_memory`'s description says the same.
  Conversations once saved a note or two each, one after describe calls alone (2026-10-29). A save or a delete sends the page a `db` frame `{kind: "memory", op: "saved" |
  "deleted", name}`. The head's Memory button (`i-brain`) shows the connection's notes in
  place of the column, as History does: a flow step each (its name, its description, when
  saved), the folder named in the hint; a note opens whole as markdown with *Edit* and
  *Delete* (asked first); *New note* and *Edit* open a sheet - its name only when new, its
  description, its body - whose refusal it says in place. A memory frame reads the view in
  front again, the open note too (back to the list when it is gone)
  (`the_memory_saves_reads_and_deletes_a_note_and_the_page_hears_each_change`,
  `a_note_saves_reads_back_and_lists_in_the_index`).
- **Compact.** The head's Compact button, or `/compact` in the box with what to keep after
  it - a "/" at the box's start lists it above the box with what it does, Tab or Enter
  completing it as `/compact ` and sending nothing, Enter on `/compact` typed whole running
  it (`chatPane`'s `commands`, §panel.ui) - condenses the open conversation into a checkpoint and carries the dialogue on from it
  (`POST /api/llm/db/compact`, §llm.agents Compaction). The model reads the whole dialogue
  and `DB_COMPACT_INSTRUCTION` after it - fixed sections: the requests, the database facts,
  the statements that mattered exactly as run and what they showed, errors and fixes,
  pending, the current work, critical context - and the checkpoint opens a new conversation
  in the same scope (source `compact:<id>`) whose next send replays the framing and the
  checkpoint, never the condensed turns; History keeps those whole under the closed run. The
  column marks it *Conversation compacted*, the checkpoint folded under it. Any failure -
  the model's error, Stop, a checkpoint cut at `maxTokens` or never written, the time
  bound - files the new run failed and reopens the old one as it was, and the column says
  so. Never while a reply streams, nor before a first answer
  (`a_compaction_carries_the_dialogue_on_from_its_checkpoint`,
  `a_failed_compaction_reopens_the_old_conversation`).
- **Model and effort.** Beside Send, a button names the model the conversation answers with
  and how hard it thinks, *glm-5.3 Max* and a chevron up (dsh's composer button,
  `#dbAgentModel`). Its menu has two rows. *Model · <model>* opens every provider's chat models
  under the provider's name, never an embedding model, the current one checked, with a find
  box over them. *Effort · <word>* opens Low, Medium, High and Max. The button and the menu read
  a model by the name its provider entry gives it (*GLM-5.3 Max*), its id one hover away and
  matched by the find box; a model its entry lists as taking no thinking effort shows no effort
  word and its menu has no Effort row, the pick's effort kept for the next model that takes one. The pick is the reader's,
  kept per agent in this browser (`swiss.dbAgent.model.<agent>`). Until one is made it is
  the agent's own model, with the agent's effort or else Max: the reader thinks at Max as a
  rule. A pick whose provider no longer offers its model gives way to the agent's own, its
  effort kept. Every send, compaction and carry-on names the pick - `provider`, `model`,
  `effort` in the body (§llm.api) - and the server runs the agent under it. A provider swiss
  does not have, a model that provider does not list as a chat model, or an effort other than
  the four is a 400. A conversation under way moves to the pick
  (`retarget_conversation`, §llm.history): its row names the pick, the endpoint check waits
  for a call to it, and the dialogue replays without the other model's signed thinking - its
  text, its reasoning and its calls stay, but a signature is the model's that wrote it and
  another wire refuses one it cannot verify. A refused carry-on puts the row back. A
  conversation moved and then sent without a pick is still `context-changed`. The model
  learns of a move from a note after the reader's words, sent once, when the model answering
  is not the one the dialogue last named: `<environment>` *The model answering changed since
  you were last told.* and the line the opening's snapshot carries, *You are the agent
  <name>, on model <model> from provider <provider>*. The column never shows or copies the
  note. Effort is not guarded and changes freely between sends. The reader asked for this so
  that a db agent is not tied to one model (DeepSeek one day, GLM-5.3 the next) and thinks at
  Max (2026-10-29) (`the_pane_picks_the_model_and_a_continuation_moves_to_it`,
  `a_moved_conversation_continues_on_the_pick_without_another_models_signatures`).
- **Writes.** On a MySQL or PostgreSQL connection a second button beside Send
  (`#dbAgentWrites`, a pencil) says how the agent's writes go, as Claude Code's auto mode
  lets a safe action through and stops a risky one: *Staged*, the default - every write it
  drafts lands in the console for the reader to run - or *Auto writes*, where `run_sql`
  runs a narrow write itself. Its menu has the two rows; the choice is kept per connection
  in this browser (`swiss.dbAgent.writes.<connection>`, only `auto` stored). A MongoDB or
  Redis connection has no button: its writes stay staged. Every SQL send names the choice
  (`writes`, `stage` or `auto`, in the stream body, §llm.api; another word, or `auto` on
  another kind, a 400). In auto mode a statement the read gate refuses goes to
  `run_agent_write` under a cap of 1 000 rows (`WRITE_ROWS_MAX`):
  - **The gate** (`assert_auto_write`): ONE INSERT, REPLACE, UPDATE or DELETE; an UPDATE or
    DELETE only with a top-level WHERE whose every OR branch names a column - none, or
    `1=1`, `true`, `'a'='a'` alone or OR'd beside a real test, changes every row - and no
    name qualified by the server's own schemas (`mysql`, `sys`, `information_schema`,
    `performance_schema`, `pg_catalog`, `pg_toast`). DDL (MySQL commits it implicitly, so
    no rollback undoes it), a data-modifying CTE, CALL, COPY, MERGE, LOCK, SET and the rest
    stay the reader's.
  - **The run**: one transaction, committed when the write changed at most the cap, else
    rolled back. MySQL bounds a single-table UPDATE or DELETE at the cap plus one
    (`bounded_write`, a LIMIT), so a broad WHERE stops early and the refusal says *more than
    1000*; PostgreSQL runs it under a 30 s `statement_timeout` and a 5 s `lock_timeout`,
    counts a RETURNING write by the rows it returns and names the count.
  - **On the page**: a committed write is a `db` frame `{kind: "run", sql, tab?, reply,
    wrote: true}` whose reply carries `affectedRows` (and MySQL's `insertId`, a
    RETURNING's rows): its result tab reads `UPDATE orders · 3` and the facts *Committed · 3
    rows changed · insert id 41* (§data.console). A held one is a stage frame `{kind:
    "stage", sql, tab?, held}`: the statement lands unrun where `stage_sql` would place it
    and a toast says *Not run, left in the console:* and why. A failure the server gives is
    the run's failed tab. The model reads *It ran and was committed: N row(s) changed* (the
    insert's id, the rows returned), or *Held, not run:* why, and that it is staged.
  - **The model** learns the mode from the snapshot's facts, a `Writes: staged - ...` or
    `Writes: auto - ... at most 1000 rows ...` line, which `DB_AGENT_PROMPT`'s *Auto writes*
    rule reads - count what a WHERE matches with a read first, DDL still staged, a held write
    told as not run and why and never split into smaller writes to get past the limit - so
    the static prompt keeps its bytes. A flip of the switch is told once after the reader's
    words: `<environment>` *The write mode changed since you were last told.* and the line,
    beside the model note when both changed; a conversation older than the switch is told
    only when it is now auto. The column never shows the note.
  - **Its limits**: the count is the statement's own - rows a trigger or a cascade changes
    are not counted; a MyISAM table keeps what a rolled-back write changed (at most the cap
    plus one, by the LIMIT); a MySQL INSERT ... SELECT or a multi-table write runs unbounded
    until the count; the cap is fixed, not the reader's.
  The reader asked for low-risk writes such as INSERT to run without them each time, and a
  DELETE without a WHERE, or one whose WHERE reaches very many rows, never (2026-10-29;
  `the_auto_write_gate_runs_narrow_writes_and_holds_the_rest`,
  `a_capped_write_answers_its_count_or_is_held_past_the_cap`,
  `an_auto_write_runs_when_narrow_and_is_held_past_the_gate_or_the_cap`,
  `a_staged_write_never_reaches_the_write_path`,
  `the_write_switch_is_in_the_snapshot_and_a_flip_is_told_once`, swiss-it's
  `run_agent_write_commits_narrow_writes_and_holds_the_rest`, `the Writes switch beside the
  model`).
- **Routes** (§llm.api): `POST /api/llm/db/stream`, `GET /api/llm/db/conversation`, `GET
  /api/llm/db/conversations`, `GET /api/llm/db/context`, `POST /api/llm/db/reopen`,
  `POST /api/llm/db/compact`, and the memory's `GET /api/llm/db/memory`, `GET
  /api/llm/db/memory/note`, `POST /api/llm/db/memory/save` and `POST
  /api/llm/db/memory/delete`. A send
  naming any agent but a db one is refused, and so is a db agent's Agents Test
  (`only_a_db_agent_on_a_sql_connection_streams`).

## §data.analysis — The analysis ledger

The server-side truth behind a multi-step analysis, as opposed to the chat text and the
browser's memory. Code: `crates/swiss-plugins/src/analysis.rs` (the store) and
`analysis_api.rs` (the routes), mounted at `/api/analysis`. The SQL agent's `analysis` tool
creates, revises, executes and reads this same store; it addresses tasks, revisions and results
by immutable ids, and refuses a task belonging to another connection. Each mutation emits an
analysis reference to the Data workspace. The workspace lists saved and recent analyses, reads
bounded result pages, and exposes revision, rerun, comparison, export and validation controls.
The next chat send carries the selected task/revision/result and selected row ids as view
context; these are resolved against the store, never treated as a database query supplied by the
browser. Leaving the page does not erase committed results.

**Measured checks, scoped conclusions.** `POST /{id}/results/{result}/validate` runs a declared
`unique`, `nonNull` or `rowCount` rule over the stored result. The check records its result id,
rule, measured value and expected value. A truncated result cannot confirm a whole-result
check. `/checks` records annotations only: caller-supplied `confirmed` is refused, even when it
cites a successful execution. Passing local checks yields partial verification; it does not
certify business definitions, source coverage or a historical database snapshot. Failed checks
dominate. The page presents the measured checks and remaining limitations together.

**One folder per analysis**, `llm/analysis/<taskId>/`: `task.json` for the ledger, one NDJSON
file per result for its rows. Four objects with four lifetimes, and the design refuses to merge
them: a **revision** is one version of the business conditions plus the SQL that answers them
(editing either makes a new one; naming an older one as the parent makes a branch); an
**execution** is one attempt to run one revision (two runs of one SQL are two executions and may
legitimately differ); a **result** is what one execution committed - a stable `resultId`, its
columns, its row count, whether it was truncated, and its rows - immutable, so a re-run makes a
new one and never rewrites the old; a **check** is one piece of evidence about a result.

**Rows are stored exactly as the driver replied**: one JSON object per row, keyed by column, in
the driver's own order. Anything that reads them (a chart's series, a comparison's keys) looks a
cell up by its column name. A positional array is still read, for results written by an earlier
build. Reading positionally was how a chart over a real answer grouped every row into one empty
bucket and a keyed comparison called every row the same row (2026-11-01;
`a_chart_reads_the_rows_the_driver_really_wrote`,
`a_keyed_comparison_tells_object_rows_apart`).

**Truncation is the driver's word as well as the budget's.** The analysis may ask for
`ROWS_MAX` (20,000), but a browser's own ceiling (`MAX_ROW_LIMIT`, 10,000) clamps it and reports
`limitApplied`; a result is `truncated` when either the budget ran out OR that field is present,
and its note names what was kept and, when it is lower, the connection's ceiling. A result marked
complete is a claim that the query had nothing more (2026-11-01;
`a_driver_that_caps_below_the_budget_still_makes_a_truncated_result`).

Materialization also stops at 20 MiB, independently of row count. Integers outside JavaScript's
exact integer range travel as strings, as driver decimal strings already do. Nonempty `params`
are refused because this runner does not bind parameters; storing them must not pretend they
affected execution. Result comparisons refuse a key absent from either column list.

**Verification is derived, never asserted.** Failure dominates partial checks and open questions.
Passing measured checks yields partial verification of the result: source coverage and business
meaning remain open. `record_check` accepts annotations but refuses `confirmed`, including an
annotation citing a successful execution. Only `validate` produces passing checks, with the rule,
observed value and immutable result reference recorded by the runtime. Old self-asserted checks
are not promoted on read (tests `a_successful_execution_does_not_prove_a_claim`,
`measured_constraints_find_duplicates_and_never_certify_business_meaning`). Zero rows is a result,
not proof of absence, and its note says so.

**Restart, cancel and late results.** An execution still `running` when the store is opened is
marked `interrupted`: the process died with it, and it must never look like it is still going or
like it finished. Cancelling is its own state, distinct from failing and from the page stopping
its spinner. A result committed after its revision stopped being current is marked superseded: it
stays readable as history and never becomes the analysis's current answer.

**Version presentation.** Result labels name revision/run ordinals and local/relative execution
 time. Current revision, its current result and a selected historical result remain distinct.
 Conditions, changes from the parent, SQL, duration, row count, truncation and check evidence are
 readable. IDs live in provenance with a copy action. Editing conditions preserves JSON structure
 and params. A chat reference opens its exact immutable result after newer runs exist. Tool answers
 carry a compact server-authored reference (task/result, revision/run ordinals and execution time);
 the live transcript and restored conversation draw its action outside the process disclosure.
 Read/validate/chart of a historical result preserve that result in the reference and page event.

**Comparison.** Left/right are full saved results, independent of pagination. Empty keys compare
 complete rows as a multiset. Keyed comparison matches exact rows first, then remaining duplicate
 keys in stored order; diagnostics disclose duplicates and null keys as ambiguous identities.
 Column names normalize positional legacy rows, including reordered columns; absent differs from
 JSON null. Schema differences, counts and at most 20 samples expose field-level before/after
 values. Truncated inputs describe saved subsets, never the whole database.

**Charts.** Default series sums each category in first-seen order. Line/area connect those
 categories; date text is categorical, not a continuous time axis. Scatter requests mode=scatter,
 keeps individual observations including duplicate X and plots actual finite numeric X. Axes,
 ticks, full-value tooltips and aggregation wording accompany charts. Invalid observations are
 counted and categories without numeric observations are omitted; categories retain JSON types,
 so null and the string "null" remain distinct. A non-finite sum is refused
 with an overflow error instead of plotting a false value. Approximate conversions and truncated inputs are flagged. Tables/exports preserve exact
 strings. The SVG evenly samples beyond 1,000 points and states displayed/available counts.
 Series points carry measures under `values`, so business columns named `x`, `rows` or `values`
 cannot overwrite coordinate/row metadata. Nonreserved legacy measure keys remain available.
 Existing saved chart configs retain their default series mode.

**Routes.** `POST /api/analysis`, `POST /api/analysis/{id}/revisions`,
`POST /api/analysis/{id}/execute`, `POST /api/analysis/{id}/executions/{run}/cancel`,
`GET /api/analysis/{id}`, `GET /api/analysis/{id}/results/{result}/rows?offset&limit`,
`POST /api/analysis/{id}/compare`, `POST /api/analysis/{id}/results/{result}/checks`,
`POST /api/analysis/{id}/results/{result}/chart`,
`GET /api/analysis/{id}/results/{result}/series?x&y`, `POST /api/analysis/{id}/save`,
and `DELETE /api/analysis/{id}`.
