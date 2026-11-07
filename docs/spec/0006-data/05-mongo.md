# MongoDB

Part 05 of [§data](README.md), chapter 0006 of the [swiss specification](../../SPEC.md).

## §data.mongo — MongoDB: the routes and the sidebar

A MongoDB connection browses documents, not rows. Its browser is `MongoBrowser`
(`swiss-host/src/mongobrowser.rs`), a second browser flavour beside `DbBrowser`
(`BrowserFlavor::Mongo`); it shares the MCP tools' lazily built client like every engine, and
every write it makes passes the MCP's command guard (§mcp.db). Every value crosses the seam as
**canonical** Extended JSON, so types are exact both ways; the panel prints and reads the mongo
shell's syntax (§data.mongo-panel).

**Servers: MongoDB 4.4 and later** — the driver's floor (wire version 9). The mongo group of
§testing.it passes unchanged on 4.4, 5.0, 6.0, 7.0, 8.0 and 8.3; a standalone answers everything
but the multi-document transaction, which it reports instead of faking (§data.mongo-edits).

| Route (under `/api/db/{name}/mongo/`) | Answer |
|---|---|
| `GET info` | `{version, topology, setName, maxWireVersion, transactions, allowDestructive, defaultDb}` |
| `GET collections?db=` | `{db, collections:[{name, type, count, size, storageSize, avgObjSize, indexes, indexSize, capped, viewOn, pipeline, validator, validationLevel, validationAction}], statsOmitted}` — statistics for at most 300 collections (`MONGO_STATS_MAX`), eight at a time |
| `POST find` | `{docs, offset, limit, more, elapsedMs}`; `limit` 1–500 (default 20; the panel offers 10/20/50/100/200/500), `maxTimeMS` default 15 s, at most 300 s |
| `POST count` | `{total, estimated}` — an empty filter reads the collection's estimate; a filter counts exactly under a 5 s bound and answers `{total: null, timedOut: true}` past it |
| `POST aggregate` | `{docs, more}` or `{wrote}` for a pipeline ending in `$out`/`$merge` (§data.mongo-pipeline) |
| `POST explain` | `{explain, summary}` (§data.mongo-pipeline) |
| `POST schema` | the inference over a `$sample` (§data.mongo-schema) |
| `GET indexes?db=&collection=`, `POST index` | §data.mongo-schema |
| `POST edits` | §data.mongo-edits; a lost race is a 409 |
| `POST command` | `{reply, elapsedMs}` — one command document against `db`, through the guard; the console's own `text`, when sent, is what its history files (§data.history) |
| `POST collection` | `op` create (options from the create command's own set), createView (a pipeline that does not write), rename, drop (needs `allowDestructive`) |
| `POST import`, `GET export` | §data.mongo-edits |

`GET /databases` answers Mongo's catalog in the shared shape (§data.databases): every database
the login may list, its `sizeOnDisk` as `size`, system databases (`admin`, `config`, `local`)
last. The row marked `primary` is the database the connection opens on — the URL's (or the
def's `database`), else the first user database — so a URL that names none still lands
somewhere. `/activity` and `/activity-kill` answer from `$currentOp` and `killOp` in the shared
row shape, the driver's own idle hello monitors dropped. A login without `listDatabases` falls
back to its authorized databases, then to the configured one.

**The sidebar** lists the current database's collections in three bands — Collections, Views,
System (the last only when it has rows) — each row carrying its document count, its title the
type, size and source. The search filters names client-side with the tree's grammar (§data.browse);
sorting (name, documents, size) is the Collections band's ⋯. A row opens a collection tab
(`kind: "coll"`, identity db + collection); a mongo connection's strip falls back to an empty
collection tab, and its console is a command console (§data.mongo-panel).

## §data.mongo-edits — Writes: one document, many, and transfers

- **An edit is optimistic.** The editor re-reads the document by `_id` before it opens, then
  writes back a `replace` whose filter is the `_id` **and** the whole stored document equal to
  the one read (`{_id, $expr: {$eq: [{$cmp: ["$$ROOT", {$literal: original}]}, 0]}}`) — an
  index lookup that matches nothing once anyone else has written. `$cmp`, not a bare
  `{$eq: ["$$ROOT", …]}`: the same comparator, but 4.4 rewrites the bare form for the index and
  fails it (code 16409). A miss is a 409 naming the fields that moved;
  the sheet keeps the operator's text and says so. A save that changed nothing writes nothing;
  the `_id` cannot change (compared as BSON, not as text).
- **Ops.** `POST edits` takes at most 1000 (`MONGO_EDITS_MAX`) of `insert`, `replace`,
  `delete` (optimistic, like replace), `updateMany` (an update document of operators, or a
  pipeline) and `deleteMany`. More than one edit runs in **one transaction** where the topology
  has them (a replica set at wire 7+, a sharded cluster at wire 8+), else in order — and a
  failure then says exactly which edits stay applied ("Edit 1 was applied and stays applied…")
  instead of pretending to roll back. A commit whose reply was lost (labelled
  `UnknownTransactionCommitResult`) is sent again, at most twice more; still unknown, the reply
  says the edits may have been written — never "nothing was written", which invites a resubmit
  that duplicates the inserts.
- **Bulk.** Update / Delete matching documents act on the query bar's filter; the confirm quotes
  the count first, and an empty filter is called out as the whole collection.
- **Import** reads a JSON array, one document, or NDJSON, in Extended JSON or the shell's syntax;
  documents go in batches under 1000 per request and 1.5 MB per body (counted in UTF-8 bytes, as
  sent), inserted unordered, so a duplicate key rejects one document, not the batch; the first 20
  errors are named with their index and the server's `errInfo`. At most 10 000 documents per
  import.
- **Export** streams the query bar's find (filter, projection, sort) as JSON (an array), NDJSON
  or CSV — relaxed Extended JSON by default, canonical on request — capped at 100 000
  documents (`X-Export-Capped` says when the cap cut it). CSV columns are the flattened paths of
  the first 1000 documents unless `fields` names them. The file is `<db>.<collection>.<ext>`:
  `filename` carries an ASCII stand-in (anything else becomes `_`), `filename*` (RFC 5987) the
  exact name, which the panel prefers.

## §data.mongo-pipeline — Aggregation and explain

- **The pipeline builder** is a list of stage cards: operator, body in the shell's syntax, an
  include switch, move up/down, remove. Each stage previews the pipeline up to itself (10
  documents; a run shows 20); `$out` and `$merge` never preview — they run only with the whole
  pipeline, after a confirm. A text mode edits the whole pipeline as one array; pipelines save per
  collection in the browser (`swiss.mongoPipelines`). At most 200 stages; `$changeStream` is
  refused.
- **Runs** append `$limit` (limit + 1, to answer `more`) unless the pipeline writes, and may
  spill to disk (`allowDiskUse`). A write needs `allowDestructive` only for `$out`.
- **Explain** runs `explain` over the query bar's find or the pipeline, at `executionStats` by
  default (`queryPlanner` and `allPlansExecution` in ⋯), and summarises it
  (`explain_summary`): the winning plan's stages as a table (stage, index, returned, keys and
  documents examined, ms), the verdict as tags — collection scan (red), in-memory sort (amber),
  the indexes used, rejected plans — and the examined/returned ratio when it is poor. The raw
  explain stays one disclosure away. The query bar's Find re-explains while Explain is in front.

## §data.mongo-schema — Schema, indexes, validation

- **Schema analysis** samples (`$sample`, 100 to 10 000, default 1000) the documents the query
  bar's filter matches and infers every field (`infer_schema`): presence, each BSON type with
  its share, the top values with counts (one click filters the documents on that value),
  distinct counts (a floor past 100), min/max (numbers and dates, reported as the values
  themselves, never their doubles), string and array lengths, nested documents and array
  elements. Bounded: depth 8, 300 fields per level, 8 sample values, values over 200 characters
  untracked — the reply says where it stopped.
- **Indexes** lists each index with its keys, its properties (unique, sparse, partial, TTL,
  hidden, collation, text/geo/hashed), its size and its use count since the statistics started
  (`$indexStats`); an unused one says it is a candidate to drop. Create takes keys and the
  createIndexes options; hide/unhide and drop act per row (dropping an index loses no data, so
  it needs no `allowDestructive`).
- **Validation** edits the collection's validator, level and action, applied with `collMod`;
  existing documents are not rewritten.

## §data.mongo-panel — The documents workspace

- **Shell syntax, both ways** (`mongo-ejson.ts`, a pure leaf). Values print the way mongosh
  prints them — `ObjectId("…")`, `ISODate("…")`, `NumberLong("…")`, `NumberDecimal("…")`,
  `UUID("…")`, `BinData`, `Timestamp`, regex literals — and the inputs read the same forms plus
  JSON, unquoted keys, single quotes, comments and trailing commas. An Int32 stays a plain
  number, a longer integer becomes `$numberLong` **from its digits** (never through a JS number),
  and a point or an exponent makes a Double. A number wrapper given a bare number
  (`{"$numberDecimal": 299.00}`, `{"$numberLong": 5}`, as a model often writes one) reads as
  its constructor would: the digits as typed, the type kept. The gateway reads such JSON too
  (`to_doc`), the type kept, but its JSON parser has already made the number a double, so the
  digits are the double's (`299.00` lands as `299.0`); the string form is the exact one. The
  suite pins that every type prints and re-reads to the same BSON value, type included; a
  parse error names its line and column.
- **The collection tab** heads with the collection's facts (database, count, size, indexes,
  server version and topology) and a pane switch — Documents, Aggregation, Schema, Indexes,
  Explain, Validation (views hide the last two and every write) — with one primary action per
  pane and the rest in ⋯ (Refresh, export, import, bulk update/delete, rename, drop, the
  console, Activity).
- **The query bar** is filter, then (behind Options) projection, sort and skip, each a shell
  document; Enter finds, Tab completes a field path learned from the pages seen, and field
  chips insert a path. A document that does not parse is said in the bar and never sent.
- **Documents** read as a list of labelled code blocks (40 lines, then Show all), as one
  canonical JSON array (a lossless copy), or as a table of the top-level fields (a cell's
  double-click opens the whole value). Each document carries copy, edit, clone (without its
  `_id`) and delete. The status line pages by skip with the count beside it — exact, estimated
  (`~`), or "count timed out".
- **The console** runs one command document per run (a blank line starts the next) against the
  sidebar's database, prints the reply as a code block, and offers templates by scope (server,
  database, collection, write) and Format.
- **The command goes first** (`command_first`), on every path that sends a document - the
  console, the Data agent's reads and stages, `mongo_command` - before a gate reads it: the
  server takes the first key as the command, and a model writes the keys in any order
  (`{"documents": [...], "insert": "orders"}` was "no such command: 'documents'"). A key that
  names a command, holding what a command takes (a name, a number or a flag; a document only
  under `explain`), moves to the front, the rest keep their order - the `find` document of a
  `split` stays an option. A name wins over a flag (`aggregate` beside `explain: true`); two
  that could each be it, neither first, are refused rather than guessed.
