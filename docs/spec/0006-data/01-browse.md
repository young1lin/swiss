# Browsing and editing

Part 01 of [§data](README.md), chapter 0006 of the [swiss specification](../../SPEC.md).

## §data.api — The routes

All under `/api/db`; `{name}` is the registry name.

| Route | Answer |
|---|---|
| `GET /` | `{connections:[{name,dialect,label,state,group}]}` — browsable entries only (`dialect != "none"`), in the sidebar's visual order (§panel.groups): groups in stored order, members in the manual order, uncovered names by name. The order is a closure read per request, so a sidebar drag reorders the picker on the next poll |
| `GET /{name}/tables` | `{tables:[{schema,name,type,approxRows,size}],total,page,limit,more}` (§data.browse) |
| `GET /{name}/databases` | the database catalog (§data.databases) |
| `GET /{name}/data` | `{schema,table,columns,rows,total,offset,limit,primaryKey,editable,editNote?,nextPage?}` |
| `GET /{name}/schema` | `{schema,table,columns,primaryKey,indexes,foreignKeys,ddl}` |
| `GET /{name}/export` | an attachment (§data.export) |
| `POST /{name}/import` | `{inserted}` (§data.export) |
| `POST /{name}/ddl`, `/ddl-preview` | `{ran}`; `{sql}` (§data.ddl) |
| `POST /{name}/query` | `{columns,rows,rowCount,limitApplied?,note?,elapsedMs}` (§data.console) |
| `GET /{name}/history`, `DELETE /{name}/history` | `{target,runs,more}`; `{target,cleared}` — the console history of the database `{name}` reaches (§data.history) |
| `POST /{name}/edits` | `{results:[{op,affected,row?}]}` — at most 1000 edits (`MAX_EDITS`) (§data.edits) |
| `POST /{name}/completion` | `{items:[{label,kind,detail}]}` (§data.completion) |
| `GET /{name}/activity`, `POST /{name}/activity-kill` | §data.activity |
| `GET /{name}/keys` | `{keys:[{key,type,ttl,binary?}],cursor,done,total}` (§data.redis) |
| `GET /{name}/key` | the `redis_read` shape; a stream key answers the newest window |
| `POST /{name}/command` | `{reply,elapsedMs}` (§data.redis-console) |
| `POST /{name}/redis-pipeline` | the replies of one pipelined round trip (§data.redis) |
| `GET /{name}/redis-commands` | the server's own command catalog (§data.redis-console) |
| `GET /{name}/stream`, `/stream/groups` | §data.streams |
| `GET /{name}/pubsub` | `{pattern,channels:[{name,binary?,subscribers}],total,truncated,patterns}` (§data.pubsub) |
| `POST /{name}/pubsub/subscribe` | an NDJSON feed of pub/sub events (§data.pubsub) |
| `/{name}/mongo/*` | the MongoDB routes (§data.mongo); on any other dialect a 404 naming it (`MCP 'x' (mysql) is not a MongoDB connection`) |

## §data.browse — Tables and rows

**The table list.** `GET /tables` takes `grep`, `page`, `limit`, `sort`, `dir`. The browsers
clamp `limit` to 200 by default and **5000** at most; the panel fetches the whole catalog at
once (`DB_TREE_FETCH_LIMIT = 2000`, §data.tabs). `sort` is `name`, `rows` or `size`, `dir` is
`asc`/`desc`; missing means name ascending and an unknown key or direction is a **400 — a sort
is never silently dropped**. The sort is an `ORDER BY` inside the adapter's own list statement,
never in memory: MySQL orders by the statistics it already computes with the name as an
ascending tiebreaker; pg orders by `lower(relname)`, `approx_rows NULLS LAST` or
`pg_total_relation_size`, so never-analysed tables sit at the bottom. The MCP tools keep their
standing order — only the browser passes a sort.

**The grep grammar.** A comma is AND, `|` is OR, `*` is a wildcard, matching is
case-insensitive; each piece becomes a bound `LIKE` pattern, at most 32 per request
(`BROWSE_GREP_MAX`).

**Rows.** `GET /data` takes `table`, `schema`, `offset`, `limit`, `order`, `dir`, `filters`.
Page sizes are 10/20/50/100/200/500, default 50, cap 500. The page asks for `limit + 1` rows and
answers `nextPage` from the extra one. Sorting and filtering run on the server.

**Filters.** `filters` is a JSON array of `{column, op, value}`, at most 16 (`MAX_FILTERS`).
Thirteen operators (`BROWSE_FILTER_OPS`): `eq ne gt gte lt lte in notIn between like notLike
isNull isNotNull`; `in`/`notIn` take at most 100 values (`BROWSE_IN_MAX`). Values are always
bound parameters; a column not in the table or an unknown operator is refused while the
`WHERE` is built. The same `browse_where` builds the rows and the export, so an export carries
the grid's filters. A JSON comparison on MySQL binds as `CAST(? AS JSON)`.

**Numbers.** An integer a double cannot hold exactly (beyond 2^53) travels as a string, both
ways — a snowflake id is never rounded on its way to the grid or back into a filter or an edit.

**pg schemas.** pg takes a `schema` parameter on every table route; MySQL's `schema` is the
database (§data.databases).

## §data.databases — The database axis (ADR-027)

`GET /databases` answers
`{primary, current, databases:[{name,primary,browsable,system,tables?,reason?}]}`; the trait's
default is an empty catalog.

- **MySQL** lists every database on the instance. The `schema` parameter names the database;
  it is checked against that list **before any SQL is built**, then quoted with `quote_ident`,
  and applies to `/tables`, `/data`, `/schema` and `/export`. The write paths stay on the
  configured database: a secondary database answers `editable:false` with the editNote
  `Read-only: <db> is not this connection's configured database (<primary>).`
- **pg** browses only the connection's own database; the others are listed with a `reason`.
- **Redis** reads `INFO keyspace` and `CLIENT INFO`; the current db is always listed, even when
  empty, and a proxy that refuses `CLIENT INFO` falls back to db0.

System databases are flagged and sorted last. Switching database resets the tables and tabs,
behind the pending-edits confirm.

## §data.grid — The grid, form and value views

- **Headers** carry the type, a PK marker and the column comment; a click cycles
  asc → desc → none (server-side). `NULL` is drawn explicitly.
- **Widths and hidden columns** persist in localStorage under
  `swiss.dbGrid.<conn>_<schema.table>`; widths are clamped to 48–1200 px. A column is at most
  240 px until it is sized: a drag on the grip at the header's right edge sizes it, a
  double-click fits it to its widest cell, the header's included (`data-columns`, shared with
  a console's results).
- **Find a column** (the table's ⋯) lists every column, hidden or not, under a find box
  (`popupMenu`'s filter): typing narrows the list, Enter or a click takes the column into
  sight at the grid's left edge and underlines its header a moment; a hidden column is shown
  again first.
- **Keyboard.** Arrow navigation, TSV paste into a range, Ctrl+C copies rows. Right-click copies
  a cell, or a row as JSON/CSV/INSERT (dialect-aware quoting, buffered values win); checked rows
  copy as CSV/TSV/Markdown/JSON. The focus ring moves between the live cells without rebuilding
  the grid, and a cell mousedown keeps focus on the keyboard layer, so the click and double-click
  that follow still land on the cell they edit.
- **Cells** are type-aware (`dbCellView`): JSON longer than 100 characters folds, a URL is a
  link, numbers use tabular figures, booleans read `TRUE`/`FALSE`, binary is marked.
- **The value sheet** shows a value as a JSON tree, text, a link, or hex (the first 512 bytes).
- **Form view** shows one row as a form; **FK jump** opens the referenced table in a new tab
  with the filter preset.
- **Column stats** ("Value distribution…", "Numeric stats…") and **filter by this value** come
  from the cell menu; the stats run through `/query` with whitelisted identifiers.
- **Structure** is a tab with Columns / Indexes / Foreign Keys and the DDL (MySQL
  `SHOW CREATE TABLE`, pg assembled from the catalog); the panel re-lays CREATE TABLE out
  aligned.

## §data.edits — Buffered edits

Every change — a cell, a new row, a delete — goes into a client-side buffer first; the amber
bar counts it and says it is local only. "SQL" previews exactly the statements Commit will run;
"Commit (1 transaction)" POSTs `/edits` after a confirm; "Discard" is a pure client-side drop.

- **One transaction.** The server runs the batch on one connection and rolls the whole batch
  back on the first error. Each edit reads the stored row back **in the same transaction**, so
  the grid shows what the database kept (defaults, triggers, truncation).
- **Optimistic concurrency.** An update carries the row as it was read plus the diff; if the
  row moved underneath (affected = 0), the whole batch rolls back with a **409 naming the moved
  columns**, the cell turns red and the buffer is kept.
- **Rows without a primary key** are addressed by every column; a value over 64 bytes is
  compared by `MD5(col)`. MySQL adds `LIMIT 1`; pg rolls back if more than one row would be
  affected.
- **Read-only** connections and secondary MySQL databases refuse edits; the editNote says why in
  the server's words.

## §data.sqlite — The built-in memory store

The Data page lists each open plugin's own SQLite file, and only that plugin's. A module
publishes the path when it opens the file and withdraws every file of its own when it stops.
A plugin that is not started has no row. Nothing hands one module another module's path.

Two doors, one seat. A history owner enrolls its `db_file` by registering on the history-query
seat (§host.history); unregister withdraws it. A module that opens a SQLite file and does not
register a history adapter publishes the path itself on start: Data (`logs/data.db`, the
console's SQL history — open console tabs stay sealed JSON, not this file), LLM
(`logs/llm.db`), Process (`logs/process.db`), and Memory (`memory/store.sqlite3`, plus
`memory/code.sqlite3` only when that file already exists). A later module that already
registers a history adapter appears on its own; the catalog has no list of module names.

The connection id is `swiss-{plugin}` when the file stem is `store` or the plugin id, and
`swiss-{plugin}-{stem}` otherwise. The label is the parent directory plus the file name when
that parent is `logs` or `memory` (`logs/data.db`, `memory/store.sqlite3`); otherwise just
the file name. The home directory is never in the label. The memory store is `swiss-memory`,
not a shared connection other modules write.

A file is listed even when it is not there yet (`state` `unknown`); the first query then
names the label and does not create the file. While the file is there, `state` is `running`.
A user MCP registered under the same id hides that row — the definition wins, and the list
does not show both. Disabling MCP removes the catalog route (`GET /api/db` is 503) even
while other plugins still hold their files; disabling Data is the same 503 because Data owns
the route. Enabling the plugin again republishes.

Every such connection is query-only. The file is opened read-only, each connection sets
`query_only`, and a statement that is not a read is refused before it is sent. `INSERT`,
`UPDATE`, `DELETE`, DDL, `PRAGMA` and `ANALYZE` (it rewrites `sqlite_stat1`) are refused
with `this connection is query-only ({label}): a write would change this module's own
database`. The wire text stays that English. The panel maps the known sentence, including
the read-gate reason appended in parentheses, through `moduleDbMessage`
(SPEC §panel.i18n). The grid's `editable` is false and `editNote` carries that sentence. Imports,
structure changes and session kill are the same refusal. The pool stays closed until the
first browse and holds no connection while idle.

Browsing is the file's own catalog: tables and views, not `sqlite_%`, one database `main`.
Name order is the list statement's. A rows sort counts each table and orders those counts;
size is unknown, so a size sort ties and the name breaks the tie. Filters bind `?` and quote
identifiers with double quotes. The console and the Data agent use the SQL tools
(`list_tables`, `describe_table`, `run_sql`, `stage_sql`, `analysis`); `run_sql` is a read,
and a write the agent tries is held rather than run. The Writes switch is not offered.
`a_query_only_store_lists_and_reads_and_refuses_a_write` and
`an_open_module_database_is_a_connection_until_its_plugin_stops` pin it.
