# §data — The Data plugin

Chapter 0006 of the [swiss specification](../../SPEC.md).

A DBeaver-style database browser that **owns no connections**: every `/api/db` request rents
one from the MCP plugin through the host's connection catalog (§mcp.catalog). It browses
tables and rows, runs a SQL console, commits buffered edits in one transaction, imports and
exports, browses Redis keys, values and streams, and gives a MongoDB connection a documents
workspace (§data.mongo). Code: `crates/swiss-data` (`dbbrowser_api/`, every SQL and Redis
route; `mongo_api.rs`, the `/mongo/*` routes), the browser model in
`crates/swiss-host/src/dbbrowser/` (traits, shapes, SQL builders, clamps; a file per concern) and
`mongobrowser.rs` (the Mongo trait, option parsers, schema inference, explain summary), the
four browsers in `crates/swiss-mcp/src/adapters/{mysql,pg,redis,mongo}_browser.rs`, and the
panel's `data-*.ts`, `mongo-ejson.ts` and `db-state.ts`.

**Descriptor.** Id `data`; page `data` (order 40, `workspace` layout — a full-bleed body under
the context bar); route `/api/db`; `requires: ["connection-catalog"]` — a capability, not a
plugin id, so the Plugins page shows the dependency unmet while MCP is off. Zero config and zero
disk; `start` and `stop` are no-ops. There are no background tasks and no eager resources: the
only things Data holds are request-scoped leases, which die with their requests.

**Leases.** Every handler first checks the catalog (serving / stopping / absent), then calls
`catalog.lease(name, "data")`; the lease counts +1 on grant and −1 on `Drop`, so there is no
"remember to return it" API to forget. The provider's stop is withdraw → drain (5 s,
`DRAIN_TIMEOUT_MS`, an honest warning naming the unreleased count on timeout) → close
(§mcp). Browsers are minted on demand and share the MCP tools' own `Arc<Lazy<Pool>>` — one pool
per connection serves tools, resources, the ping and the Data page; a second pool is never
built.

**Connection identity.** A connection is a registry entry and its key is the entry's name. Two
definitions sharing host and port but differing in credentials are two entries and never merge
(a test pins it). **There is no read-only flag**: Data is editable and queryable, and the
operator's databases are the operator's to change. An old definition may still store
`readonly`; nothing reads it and the echo drops it (§host.refs). The one read-only
surface is a table in a database other than the configured one (§data.databases). The browser
`label` names only the target (`db @ host:port`), never a credential.

**Boundary and errors.** `/api/db` sits inside the host's loopback guard, 2 MiB body limit and
plugin boundary (§host); it does no bearer auth — loopback is the gate. The error body is
`{error}`:

| Status | When | Text |
|---|---|---|
| 400 | a driver or guard refusal, a bad parameter | the driver's message **verbatim** — "Data too long for column 'x'" is exactly what a grid user needs |
| 404 | unknown name; not a database | `unknown MCP: <name>`; `MCP '<name>' (echo) has no database to browse`; `MCP '<name>' (mysql) is not a redis connection` |
| 409 | a buffered edit lost a race (§data.edits) | `{error, conflictColumns, row}` |
| 503 | no catalog provider, or it is stopping | names the missing party: `the mcp plugin provides database connections and is currently disabled` |

Query parameters keep the route-literal semantics: missing and empty both mean "not sent";
numbers are coerced like JavaScript's `Number()`.

**Logs.** `data view import` (info), `data view ddl` (**warn**), `data view commit` (info),
`data view redis commit` (info, with the command count), `data view activity kill` (**warn** —
the one route that interrupts someone else's work).

**Parts.** The sections live in six files beside this one, in reading order:

1. [Browsing and editing](01-browse.md) — the routes, tables and rows, the database axis, the grid, form and value views, buffered edits
2. [The console and the agent](02-console.md) — the SQL console, what it ran per database, the agent beside it
3. [Completion, live activity, export and DDL](03-workspace.md) — server-side completion, live sessions, export and import, structural operations
4. [Redis](04-redis.md) — keys and values, the Redis console, streams, pub/sub
5. [MongoDB](05-mongo.md) — the routes and sidebar, writes, aggregation and explain, schema and indexes, the documents workspace
6. [Object tabs and the session cache](06-panel.md) — object tabs, the tree and the bars, the session cache
