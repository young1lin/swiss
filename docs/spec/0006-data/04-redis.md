# Redis

Part 04 of [§data](README.md), chapter 0006 of the [swiss specification](../../SPEC.md).

## §data.redis — Keys and values

- **Keys.** `GET /keys` walks `SCAN` (`pattern`, `cursor`, `count` 1–1000 default 200, `type`
  from string/hash/list/set/zset/stream); TYPE and TTL ride a pipeline 200 at a time. One
  request walks on past rounds that match nothing, COUNT doubling up to 1000, and answers at
  its first match, the walk's end, 32 rounds or 2 s - a sparse type or pattern no longer
  costs a More click per empty round (`a_sparse_type_walks_on_until_it_matches`,
  `an_empty_round_moves_the_cursor_and_doubles_count`); the "no hits yet" hint is left for a
  walk that spent its budget. `fill` (1-1000, the Data agent's `scan_keys` sends 200) answers
  once the page holds that many instead of at its first match, every short round widening as
  an empty one does - a model paging `*HEARTBEAT*` a hit or two a call spent many calls. Keys are de-duplicated on arrival (SCAN promises only "at least
  once").
- **The tree** groups keys by `:` (`redisNamespaceTree`) with single-child path compression.
  Each row shows **only the segment it adds at its level**; the full key sits in the right slot.
  A band's collapse state is keyed by its full path. Depth is capped at 32
  (`REDIS_NS_MAX_DEPTH`); deeper keys are rows of the deepest band. The collapse map has no
  prototype, so a key named `constructor` or `__proto__` behaves.
- **Names that the page would alter** (leading or trailing space, newline, zero-width, bidi
  controls, the empty name) are shown quoted and escaped like redis-cli (`dbKeyShown`) —
  display only; every action sends the key's own bytes.
- **Non-UTF-8 names.** SCAN keeps names as bytes; a non-UTF-8 name is printed the way
  redis-cli prints it and marked `binary:true`. The panel keeps it at the root, labels it,
  refuses to open it ("use redis-cli") and skips it for bulk delete and completion. There is no
  byte-addressed action channel.
- **Values.** `GET /key` reads by type (string, hash, list, set, zset; windows of 1000, hash
  and set capped at 1000 and marked `truncated`); a module type names its own commands; a
  stream key answers the newest window (§data.streams).
- **Structured edits** buffer typed changes (+Field / +Item / +Member, string SET, EXPIRE),
  preview them as commands, and commit them as **one pipelined round trip**
  (`POST /redis-pipeline`); every command passes the console guard first and the first refusal
  rejects the batch whole.
- **Key actions** go through the pipeline as argv, never as a command line: Delete (typed
  confirm), Rename (`RENAMENX`, a taken name is reported beside the input), Set TTL (0 is
  refused — use Delete; `EXPIRE` answering 0 says the key is gone). Values are submitted
  exactly, whitespace included.
- **TTL** is one readout at the head's top-right: read once with the value, then counted down
  by one shared 1 s ticker (`10s`, `1:31`, `2:05:00`); clicking it edits (Enter = EXPIRE,
  empty = PERSIST). The tree can sort by TTL but never prints it as a clock.
- **Bulk delete.** A band's ⋯ offers "Delete N keys" (`keysUnder`). `DEL` is one atomic
  command; past 1000 keys (`REDIS_DEL_CHUNK`) it splits, and past 1.5 MB of JSON per request
  it splits into several requests — the confirm says the guarantee then is one round trip, not
  one command, and a partial SCAN changes the confirm to "only the loaded keys". The toast
  reports what redis deleted, and the gap separately; a failure stops and reports what was
  done. Tabs on the deleted keys are cleared and the tree re-walked. No MULTI/EXEC: it is not a
  rollback and would promise nothing more.

## §data.redis-console — The Redis console

- **One command a line.** A run covers what an SQL console's does (the selection, the caret's
  block, or Run all) and sends each line as its own `POST /command` (`dbSplitCommands`: a
  newline inside quotes stays in its argument), one result tab each (`GET user:1`); one
  command keeps the single reply. The server splits a command by `split_words` (shlex with a
  word-leading `#` kept as a character, not a comment), where a newline is only a space - so
  the cutting is the panel's. The guard is the MCP's redis policy (§mcp.db). Array replies over
  1000 items are truncated and labelled. The first failure stops the run as in §data.console.
- **The command catalog comes from the server.** `GET /redis-commands` asks `COMMAND DOCS`
  (words: summary, syntax, group, since, tokens) and `COMMAND INFO` (arity, first/last key,
  step, and redis 7 key specs) **separately** — redis 6 has no DOCS, and a pipelined failure
  would sink both — and merges them (`documented:false` when DOCS failed). Container commands
  keep their own row and flatten their subcommands (`XINFO STREAM`). The panel asks once per
  connection; nothing is cached server-side and the panel keeps no command table.
- **Completion** reads only the caret's line: word 0 completes command names (never on an
  empty prefix), word 1 of a container its subcommands, a key position (first/last/step, or the
  key specs for commands whose keys move) the keys the tree has walked, any other position the
  command's own tokens not yet typed; a value position gets nothing. A key that would not split
  back to itself is inserted quoted. Lines over 64 KB are not completed; the word-span search
  looks back at most 4096 characters.
- **The signature line** under the console shows the recognised command's
  `name syntax — summary`, else the fixed hint.
- **Templates** is its own button left of Run: 32 hand-written templates grouped string / key /
  hash / list / set / zset / stream / server.
- **One splitter.** The panel and the server split the same way; the shared corpus
  `tests/fixtures/console-split.json` is read by both test suites, with the three known
  divergences documented.

## §data.streams — Redis streams

- **Windows.** `GET /stream {key, before?, after?, count?}` reads with XREVRANGE only: newest
  `+ -`, older `(before -`, newer `+ (after` — exclusive bounds need Redis ≥ 6.2. The answer is
  `{key,type,ttl,length,firstId,lastId,entries:[{id,ts,fields}],columns,more}`, entries always
  newest first, `ts` ISO with milliseconds from the id, `columns` the union of fields in
  first-seen order, `more` = the page is full. `count` defaults to 100, max 1000; ≤ 0 or
  non-numeric, or `before` with `after`, is a 400; a non-stream key is a 400
  `X is a hash, not a stream`; a missing key is `type:"none"`.
- **Budget.** Exactly three commands per window — XREVRANGE, XLEN, XINFO STREAM — in one
  pipeline; a test pins it. `redis_read` on a stream keeps its oldest-first offset/limit.
- **The view** (`data-stream.ts`) is a read-only table (id | time | columns), no virtual
  scroll, at most 500 rows; Load earlier appends and drops from the top.
- **Follow** polls at 1, 2 or 5 s, only while the tab is active and the document visible. A
  view at the top rides the live edge; otherwise new rows pool behind an "↑ N new" pill, and
  hovering the table holds them too ("paused while reading"). A full catch-up page shows a gap
  bar with "jump to latest", issued on the same request token as the ticks. The rate is the
  delta of `length` and may be negative. A tick error stops Follow with the reason; a tick
  repaints only the table.
- **Filter.** `match` is parsed on the server (`parse_stream_filter`, shlex): `field=value`
  exact (case-insensitive value), `field~value` contains, a bare word matches the id, time or
  any value; terms AND; no OR, negation or regex; a bad filter is a 400 before any lease. A
  filtered read scans back in pages of 500 until `count` hits, 20 000 entries scanned, or the
  range ends, and adds `scanned`, `scannedFrom` (the Follow cursor) and `scannedTo` (the Load
  earlier cursor) — present only with a filter. The input survives repaints; debounce 400 ms,
  Enter applies at once; zero hits says "no scanned entries match".
- **Summary chips** (panel-only): "value N · r/s" for an auto-chosen field (the first column
  with 2–24 distinct values and distinct × 2 ≤ rows, ≥ 4 rows; sticky; the user's choice,
  including none, wins), at most 12 chips; a click sets the filter.
- **Consumer groups** are read-only (`/stream/groups`: group, consumers, pending, lag,
  last-delivered-id; lag is "—" on 6.2), fetched when the fold opens and every 5th Follow tick.
  No XACK/XCLAIM/XTRIM/XDEL — those move someone else's consumption.
- XREAD and XREADGROUP stay refused: one shared connection must never block.

## §data.pubsub — Redis pub/sub

A channel is not a key: SCAN never sees one, and the console refuses SUBSCRIBE and PSUBSCRIBE,
which would take the shared connection out of service (§mcp.db). So a redis connection gets a
Pub/Sub page of its own: an open object like Activity (§data.tabs), one per connection, opened
from the tree's first row or from the connection's ⋯ or a key tab's. That row, *Pub/Sub* with
the tab's glyph and *channels* where a key row names its type, is pinned above the keys on
every redis connection, an empty keyspace too, marked while the page is in front: the ⋯'s
last entry alone left a reader looking in the tree for pub/sub finding nothing (2026-10-28;
`the tree's first row is Pub/Sub`). The Data agent's channel listings and listens open it
too, its pattern, watch set and log taking what the agent read (§data.agent).

- **Channels.** `GET /pubsub?pattern=` asks PUBSUB CHANNELS under the pattern (default `*`; a
  blank one is `*`), sorts the names, cuts them at 1000 (`PUBSUB_LIST_MAX`, `truncated`) and
  pairs each with its PUBSUB NUMSUB count, PUBSUB NUMPAT riding along as `patterns`: the counts
  in one pipeline over the shared connection (`the_channel_listing_pairs_each_name_with_its_numsub_count`,
  `the_channel_listing_passes_its_pattern_through`). A non-UTF-8 name is printed the way
  redis-cli prints it and marked `binary`, so it is listed but cannot be watched. A channel
  heard only through a pattern has no NUMSUB subscriber and is not listed; the page shows the
  NUMPAT count instead. The page polls the listing every 5 s while it is active and visible.
- **The feed.** `POST /pubsub/subscribe {channels?, patterns?}` takes two optional sides that
  together are never empty. Each side holds at most 32 names (`PUBSUB_NAMES_MAX`) of at most 512
  bytes, with duplicates dropped. A bad set is a 400 before any lease
  (`a_watch_set_is_names_on_two_sides_checked_before_any_socket`,
  `a_bad_watch_set_is_a_400_before_any_lease`); a connection of another flavour is a 404
  (`the_pubsub_routes_answer_only_a_redis_connection`). The server subscribes on a connection of
  its own, never the shared one: one SUBSCRIBE or PSUBSCRIBE per name, then a PING to check it
  took. A server or proxy that refuses SUBSCRIBE answers that PING with a plain PONG, and the
  request is the 400 `the server refused SUBSCRIBE…`
  (`a_refused_subscribe_is_the_error_and_holds_no_place`). Any failure before the first event
  is the usual JSON error. After the 200 the body is NDJSON (`application/x-ndjson`), one event
  a line (`a_feed_relays_what_the_server_pushes_then_ticks`,
  `a_feed_streams_ndjson_and_holds_its_lease_until_it_ends`):
  - `subscribed {channels, patterns}` comes first.
  - `message {channel, channelBinary?, pattern?, payload, binary?, size, truncated?, at}`. A
    UTF-8 payload is cut at a character inside 16 KiB (`PAYLOAD_MAX`, `truncated`; `size` is the
    whole length). Any other payload is printed the way redis-cli prints its bytes (`binary`).
    `pattern` is the PSUBSCRIBE that matched
    (`a_payload_is_shown_as_text_cut_at_a_character_or_as_redis_cli_prints_it`).
  - `dropped {count}` counts what was not relayed: messages past 200 a second (`RATE_MAX`), or
    past a 256-event buffer for a reader that fell behind. The count goes out before the next
    event that does (`a_burst_past_the_rate_is_counted_not_relayed`).
  - `tick {at}` comes every 15 s, after a PING the server answered, so a dead tunnel ends the
    feed instead of leaving it silent.
  - `closed {reason}` comes last: the server hung up, a PING failed or went unanswered for
    10 s, or the MCP plugin is stopping (`the_feed_ends_with_closed_when_the_server_hangs_up`).
- **Bounds.** A connection holds at most 4 feeds (`SUBSCRIPTIONS_MAX`); a fifth is a 400 naming
  the cap (`a_connection_holds_at_most_four_feeds`). A feed holds its connection lease as long as
  its body is open. When the reader goes away (the panel aborts the fetch), the body drops,
  which closes the subscribing connection and frees the place. A stopping MCP plugin does not
  wait for a live feed to end: it recalls the lease, and the body ends at once with `closed
  {reason: "the mcp plugin is stopping"}` (`a_stopping_plugin_recalls_a_live_feed_instead_of_waiting_it_out`,
  `a_drain_recalls_a_holder_that_would_otherwise_hold_on`).
- **Publish** has no route of its own. The page sends `["PUBLISH", channel, message]` through
  `POST /redis-pipeline`, so the console guard applies and the message stays one argument
  whatever it holds. The toast reports how many subscribers received it, this page's own feed
  included.
- **The page** (`data-pubsub.ts`) is drawn with the library alone (§panel.ui). `split()` sets
  two columns side by side:
  - The live column holds the feed's state with Listen or Stop, the watch set and the log.
  - The aside holds the channel listing and the publish row. The listing has a pattern
    filter and a Watch button per channel, disabled once the channel is watched or when its
    name is binary.

  The watch set follows a "Watching" label, one ghost button per name, and a click lets that
  name go. The bare `*` reads "All channels (*)". A typed name holding `*`, `?` or `[` is a
  pattern (PSUBSCRIBE); anything else is a channel. "Watch everything (*)" is offered while
  the set is empty. A change to the set resubscribes after 300 ms of quiet. A watched channel
  that a watched pattern also matches gets a note under the set: redis delivers each of its
  messages twice, once for each subscription. The page matches the pattern the way redis's
  stringmatch does, for the note alone (`names the watched * and says when a channel arrives
  twice`, `reads a pattern the way redis matches it, to say when a channel arrives twice`).

  **Recent** sits under the watch set. It lists the names this browser watched on the
  connection before, newest first, one ghost button each, and a click watches that name
  again. A name already in the watch set is not offered, so letting a name go returns it to
  Recent. Each name added to the set goes to the front of the list, and a repeat moves up
  instead of appearing twice. The list holds 20 names per connection, and the oldest fall off
  past that. It lives in localStorage (`swiss.pubsubRecent`, keyed by connection), so it is a
  convenience of one browser and not state. ⋯ Clear recent forgets the connection's list
  (`offers the names watched here before, a click watching one again and moving it to the
  front`, `remembers a watched name at the front, a repeat moving up, the oldest falling
  off`).

  The log is the event list, newest first, at most 500 lines (`DB_PUBSUB_LOG_MAX`). A
  message's row is its channel and payload: the pattern sits in the who column when the rows
  differ, and a cut payload is tagged. Identical consecutive messages fold into ×N. A row
  opens on its summary to show these:
  - the channel as text that can be selected (the summary is a button, whose text cannot be),
    then the size and the pattern;
  - Copy channel, and Watch only this channel, which makes that channel the whole watch set.
    That is the way from a feed watching everything to the one channel worth following. It is
    disabled once the channel is the whole set, or when its name is binary;
  - the payload the way Logs shows a value (JSON as the code block), with its own Copy.

  An open row stays open through repaints (`an open message copies its channel and payload
  and can become the one channel watched`).

  The page's own notes ride the same list:
  - Listening
  - Not listening, for the stretch between a feed's end and the next subscribe, tagged
    missed
  - Dropped, tagged missed
  - Closed
  - Cut off

  The log repaints at most every 250 ms (`the Pub/Sub page`,
  `reads the log into event-list rows, newest first, identical messages folding`). It holds
  the repaint while text in it is selected or a button in it is held down. A repaint replaces
  the nodes, so it would eat the selection being copied or the click. Only the count moves
  meanwhile (`holds the log's repaint while text in it is selected`).
- **The feed follows the page in front.** The feed is aborted and the poll stops when any of
  these happens: the tab is backgrounded or closed, the connection is switched, or the view
  is left. The abort makes the server give the subscription back at once. Coming back
  subscribes again and logs the gap.

  A feed that ended on its own is opened again in 5 s while the page is in front. That covers
  both a `closed` line and a body cut off without one, which the log notes as Cut off. A
  refusal before the first event is shown and not retried. Stop unsubscribes and keeps the
  set and the log. While nothing is watched, no feed is opened.

  Tests: `subscribes while the page is in front, aborts when it leaves, and notes the gap on
  return`, `a body that ends without a closed line says the feed was cut off`, `opens no feed
  while nothing is watched`.
- **The toolbar** is Refresh plus ⋯, which holds Clear messages, Clear recent and Close all
  (`a pubsub tab: Refresh is the one flat button`). The redis connection's ⋯ and a key tab's
  ⋯ both end with Pub/Sub… (`a redis connection reaches Pub/Sub from either ⋯`).
- **Not detected:** an ACL that refuses one name beside names it allows. That channel stays
  silent.
- **Not built:**
  - sharded pub/sub (SSUBSCRIBE);
  - a keyspace-notification preset;
  - a message history. Redis keeps none, so the log is only what this page heard while it was
    in front.
