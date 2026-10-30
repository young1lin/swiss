# Completion, live activity, export and DDL

Part 03 of [§data](README.md), chapter 0006 of the [swiss specification](../../SPEC.md).

## §data.completion — Server-side completion (ADR-017)

`POST /completion {sql, caret}` → `{items:[{label,kind,detail}]}`: keywords (a core list plus
pg and MySQL extras), table names, and the columns of the FROM-nearest table, for the word
ending at the caret. Columns come from a lazy per-connection cache (10 minutes, 64 KB budget)
that a schema-touching statement invalidates. The panel asks 150 ms after a keystroke that ends
a word and shows at most 8 items.

## §data.activity — Live sessions

`GET /activity` lists the server's sessions in one shape for both dialects (pg
`pg_stat_activity` with `pg_blocking_pids`, MySQL's processlist). `POST /activity-kill
{pid, mode}` cancels (`pg_cancel_backend` / `KILL QUERY`) or terminates (`pg_terminate_backend`
/ `KILL`); mode and pid are validated and the kill is logged at warn. The Activity tab polls
every 5 s while active. A redis connection has no sessions to list: its ⋯ opens Pub/Sub
(§data.pubsub) where a SQL connection's opens Activity.

## §data.export — Export and import

- **Export** (`GET /export`, `format=csv|json|sql`): CSV (RFC 4180), NDJSON, or a **streamed
  SQL dump** — CREATE TABLE, foreign-key checks off, multi-row INSERTs batched up to 1 MB
  (a single larger row ships alone), the dialect's foot — sent through `Body::from_stream` with
  the lease riding inside the stream. Every format stops at 100 000 rows (`EXPORT_ROW_CAP`),
  read in 5 000-row chunks; the headers `x-export-rows`, `x-export-capped` and
  `x-export-format` say what was saved. The export carries the grid's filters.
- **The panel's CSV** button exports the current page or result client-side (CSV + BOM).
- **Import** (`POST /import {table, schema, header, lines, mapping}`): at most 10 000 rows
  (`IMPORT_ROW_CAP`), one transaction. The wizard: paste or upload → map each column (by name
  by default, skippable) → preview three rows → confirm. Numbers bind as numbers only when
  `Number()` is lossless. **Upsert** uses MySQL `ON DUPLICATE KEY` or pg `ON CONFLICT (pk)`; a
  table without a primary key says so and falls back to plain inserts. On failure nothing on
  screen changes.

## §data.ddl — Structural operations

`POST /ddl {op, …}`: `rename`, `truncate`, `drop`, and the minimal create set `create_table`,
`add_column`, `create_index`. Rename validates `[A-Za-z0-9_$]{1,64}`; truncate and drop require
typing the table name. The New table / Add column / New index sheets show
`POST /ddl-preview`, built by **the same builder** the commit runs, so the preview is exactly
the executed SQL. Read-only connections refuse DDL. Not built: ALTER TYPE, constraints, views,
triggers.
