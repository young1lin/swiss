# The event bus: design brief, draft SPEC chapter and delivery plan

This is the implementation brief the owner asked for before any code, not a second canonical
specification. The current-behaviour specification remains [SPEC](../SPEC.md). The draft
chapter in §4 and the amendments in §6 land **with the commit that makes each paragraph true**,
never ahead of it.

Status: accepted by the owner for implementation, 2026-11-04, with the choices of §3 and §5 as
written (module numbers are chapter numbers; the Rust generator lives outside the workspace;
the preferences follow across every open page; the reconnecting line). No code, dependency or
deploy accompanies this brief itself.

## 1. What the owner decided (2026-11-04)

1. **One envelope, all protobuf.** Each WebSocket binary message is one protobuf `Frame`;
   bodies are protobuf messages too. No JSON on the bus.
2. **Generated code, not a hand-written codec.** Other polling moves onto the bus later (the
   Remote page's run history, whether a run was canceled, cancelling a running one); everything
   that has to be live goes through this module, so adding a message must be cheap and safe.
3. **One step all the way.** swiss is unreleased, so nothing is kept for compatibility: writes
   go over the bus from the first delivery, and the HTTP routes they replace are deleted.
4. **One person edits.** Two windows open on the same thing are one person's two views, never
   two editors. The design does not merge concurrent edits; the reason is written into the code
   comments. If merging is ever needed, it is a **three-way merge**.
5. **Phase 1:** the bus core; PING and subscribe; DATA.CONSOLES; the new-build notice (BYE and
   WELCOME); the PANEL.PREFS relay (language and theme across open pages).

And one boundary: **the terminal's WebSocket is unrelated** and stays as it is (§terminal.api).
The bus is a new standalone module with its own crate and its own endpoint.

## 2. What the bus is, and is not

It is one WebSocket per page (browser tab or the desktop window) to `GET /api/bus`, carrying:

- **requests** a page sends and the server answers, writes included;
- **events** the server pushes to the pages subscribed to a topic, for state the server owns
  (a console record, later a run's state);
- **relays** a page sends to the other pages through the server, for state a page owns (its
  language and theme).

It is not a message queue. Nothing is persisted and nothing is replayed: an event carries the
whole new state of what it names, with its revision, so applying it twice or late is harmless,
and a page that reconnects subscribes again and reads the current state. It is not a second
terminal transport, and it is not a collaboration engine (§4, §bus.editing).

## 3. Choices made inside the decisions, with reasons

| Question | Choice | Why |
|---|---|---|
| Module numbers | **A module's number is its SPEC chapter number**; 0 is the bus itself | The chapter table already is a registry whose numbers are never reused (SPEC §about). No second table to keep in step. All fit one varint byte. This refines the 1/2/3 numbering proposed in conversation; PING's bytes are unchanged (module 0 is omitted on the wire) |
| `seq`, `reply_to` | `uint32`, per sender per connection, from 1 | A connection never sends 2^32 frames; `uint32` stays a JS `number` (protobuf-es maps `uint64` to `bigint`) |
| Times and revisions | `uint64`/`int64` with `[jstype = JS_NUMBER]` | Milliseconds and revisions stay below 2^53; the option only changes the TS type, not the wire; prost ignores it |
| Rust codegen | `prost-build` + `protox` (a pure-Rust protobuf compiler) in a generator **outside the workspace**; output committed; a digest test gates freshness | §arch.deps keeps prost `derive`-only with no protoc and no build script. prost-build drags petgraph, prettyplease and friends; outside the workspace they never touch the shipping graph or every `cargo test --workspace` |
| TS codegen | `@bufbuild/buf` + `@bufbuild/protoc-gen-es` 2.16.0 as panel devDependencies, `target=js+dts` | Buf's own generator for its own runtime. `target=ts` emits `export enum`, which is not erasable syntax (ts-blank-space and `erasableSyntaxOnly` reject it); `js+dts` emits plain JS plus declarations. Verified 2026-11-04 in a scratch build: generated, round-tripped, and the bytes match §4 |
| TS runtime | `@bufbuild/protobuf` 2.16.0 vendored (§panel.vendor), its imports rewritten by the generator (`rewrite_imports`) to the vendor path | No bundler; native ES modules. The generated code needs about 40 runtime modules, 299 KiB before gzip (measured, scratch build) |
| Auth | The admin session cookie, plus a **mandatory** `Origin` equal to the request's `Host` | Same-origin WebSocket handshakes carry the cookie; browsers always send `Origin` on them, so requiring it closes cross-site WebSocket hijacking. No ticket: a ticket buys nothing the cookie does not already prove for a same-origin page |
| Liveness | Server: WebSocket protocol pings every 30 s. Page: an app-level PING after 20 s of silence | A browser cannot send protocol pings, but answers them even in a throttled background tab; the page's own PING detects a dead link from its side (laptop wake, a killed gateway) |

## 4. Draft chapter 0019 — §bus

The text below becomes `docs/spec/0019-bus.md`, with its row in the chapter table and its block
in the anchor map, in the first implementation commit.

---

> # §bus — The event bus
>
> Chapter 0019 of the swiss specification.
>
> One WebSocket per panel page carries requests, events and relays between the gateway and every
> open page, so a change made in one window shows in every other window without a reload, a poll
> or a focus. Frames are protobuf, the codecs are generated for Rust and TypeScript from one set
> of `.proto` files, and each module owns its own messages. The terminal's socket is a different
> thing (§terminal.api) and is not on the bus.
>
> | Piece | Location |
> |---|---|
> | Protocol sources | `crates/swiss-bus/proto/swiss/bus/v1/*.proto`: `bus.proto` (the envelope and the bus's own messages), one file per module (`data.proto`, `panel.proto`, …) |
> | Rust | `crates/swiss-bus/`: `src/pb/` (generated, committed), `hub.rs` (links, topics, handlers), `link.rs` (one connection's pump), `route.rs` (the upgrade) |
> | Rust generator | `crates/swiss-bus/protogen/`: a standalone crate excluded from the workspace |
> | Panel | `panel/src/bus/`: `pb/` (generated `*_pb.js` + `*_pb.d.ts`, committed), `link.ts`, `bus.ts` |
> | Runtime | `src/admin_assets/js/vendor/protobuf/2.16.0/` (§panel.vendor) |
>
> ## §bus.wire — Frames
>
> Every WebSocket message is binary and holds exactly one `Frame`. A text message, or bytes that
> do not decode as a `Frame`, end the link with BYE `PROTOCOL`.
>
> ```proto
> message Frame {
>   uint32 module = 1;   // the owning module: its SPEC chapter number; 0 is the bus itself
>   uint32 sub = 2;      // the module's sub-module, from 1
>   uint32 id = 3;       // the message within the sub-module, from 1
>   Kind kind = 4;
>   uint32 seq = 5;      // the sender's own counter on this connection, from 1
>   uint32 reply_to = 6; // RESPONSE only: the seq of the REQUEST it answers
>   string key = 7;      // the topic key within (module, sub); "" when the sub has none
>   bytes body = 8;      // the message itself
>   Fault fault = 9;     // RESPONSE only: set means refused, and body is empty
>   string origin = 10;  // EVENT and RELAY: the page whose request caused it; "" = the server
> }
> enum Kind { KIND_UNSPECIFIED = 0; KIND_REQUEST = 1; KIND_RESPONSE = 2; KIND_EVENT = 3; KIND_RELAY = 4; }
> message Fault { string code = 1; map<string, string> params = 2; string message = 3; }
> ```
>
> - **A page sends only REQUEST and RELAY**, and its first frame is HELLO. Any other first
>   frame, a second HELLO, an EVENT or a RESPONSE from a page, a `sub` or `id` of 0, or a
>   RESPONSE whose `reply_to` names no open request (on the page's side: dropped and logged)
>   ends the link with BYE `PROTOCOL`.
> - **The router reads the envelope only.** `(module, sub, id, kind)` names the body's message
>   type (§bus.registry); `body` is decoded by the module that owns it, so the hub never links a
>   module's types.
> - **REQUEST** (page → server) is answered by exactly one **RESPONSE** with the same
>   `(module, sub, id)` and `reply_to` = the request's `seq`. Responses may come out of order:
>   the hub runs each request as its own task.
> - **EVENT** (server → page) goes to every link subscribed to its topic, the causing page's
>   own link included; `origin` lets that page skip what its response already applied.
> - **RELAY** (page → server → pages) is forwarded unread to every other link subscribed to its
>   topic, `origin` set by the hub. Only a sub on the relay list (§bus.registry) is relayed; a
>   relay elsewhere is answered by a RESPONSE carrying `bus.notRelayable`. Relays never carry
>   server-owned state, so a page can never forge an event.
> - **Faults** use the served-code namespace (§panel.i18n): a module's own codes
>   (`console.changed`) and the bus's: `bus.unavailable` (the owning plugin is not serving),
>   `bus.unknown` (no such message), `bus.badBody` (the body did not decode), `bus.notRelayable`,
>   `bus.limit` (a limit below was reached); and three a page raises locally, `bus.offline` (no
>   link), `bus.timeout` (no response in 15 s) and `bus.tooLarge` (a frame past the limit). Each ships its `served.*` keys in both locales.
>
> **Limits.**
>
> | Limit | Value | Past it |
> |---|---|---|
> | Frame size, either direction | 4 MiB (a console's text is at most 2,000,000 bytes) | inbound: the link closes; outbound: the page refuses locally with `bus.tooLarge` |
> | Links per gateway | 32 | BYE `LIMIT` to the newest |
> | Topics per link | 512 | the SUBSCRIBE is refused, `bus.limit` |
> | Outbound queue per link | 256 frames or 8 MiB | BYE `SLOW`; the page reconnects and resyncs |
> | HELLO after the upgrade | 10 s | the link closes |
>
> **Example — PING**, `(0, 1, 2)` REQUEST, seq 7, sent 2026-11-04 09:30:00 UTC, 17 bytes
> (module 0 is the proto3 default and is not written):
>
> ```
> 10 01  18 02  20 01  28 07  42 07 | 08 c0 ab 93 af 9a 34
> sub=1  id=2   REQ    seq=7  body  | Ping.sent_ms = 1793784600000
> ```
>
> Its PONG, seq 9, `reply_to` 7, `server_ms` 3 ms later, 26 bytes:
> `10 01 18 02 20 02 28 09 30 07 42 0e 08 c0 ab 93 af 9a 34 10 c3 ab 93 af 9a 34`.
> These and the other golden frames live in `crates/swiss-bus/testdata/golden.json`, which the
> Rust and the panel suites both decode and re-encode byte for byte.
>
> ## §bus.registry — Modules, subs and messages
>
> **A module's number is its SPEC chapter number**, 0 is the bus itself, and numbers are never
> reused (SPEC §about). 1–3 have no runtime; 9 (terminal) keeps its own socket. Within a module,
> subs and ids start at 1 (0 is invalid) and are never reused: a retired message leaves a
> `reserved` line in its `.proto`. Each `.proto` declares its numbers as enums, so both sides use
> generated names, never literals.
>
> | Module | Sub | Id | Kind | Body → reply | Key |
> |---|---|---|---|---|---|
> | 0 BUS | 1 LINK | 1 HELLO | REQUEST | `Hello` → `Welcome` | — |
> | | | 2 PING | REQUEST | `Ping` → `Pong` | — |
> | | | 3 BYE | EVENT | `Bye`, then the server closes | — |
> | | | 4 BUILD | EVENT | `Build`: the stamp changed under a live link (debug builds) | — |
> | | 2 TOPIC | 1 SUBSCRIBE | REQUEST | `Topics` → empty | — |
> | | | 2 UNSUBSCRIBE | REQUEST | `Topics` → empty | — |
> | 6 DATA | 1 CONSOLES | 1 LIST | REQUEST | `ConsolesPage` → `ConsoleList`, paged by `next` | — |
> | | | 2 PUT | REQUEST | `ConsolePut` → `ConsoleSaved` | — |
> | | | 3 REMOVE | REQUEST | `ConsoleRemove` → `ConsoleRemoved` | — |
> | | | 4 CLEAR_CLOSED | REQUEST | `ConsolesOf` → `ConsolesCleared` | — |
> | | | 5 CHANGED | EVENT | `ConsoleSaved` | connection |
> | | | 6 REMOVED | EVENT | `ConsoleGone` | connection |
> | | | 7 CLEARED | EVENT | `ConsolesCleared` | connection |
> | 14 PANEL | 1 PREFS | 1 CHANGED | RELAY | `Prefs` (only the fields set changed) | — |
>
> Relay list: PANEL.PREFS. Planned rows (§7 of the brief) take their numbers when they land.
>
> ## §bus.link — Connecting, staying connected, leaving
>
> - **Handshake.** The page's first frame is HELLO `{protocol: 1, page_id, build}`; `page_id` is
>   random per page load and becomes the `origin` of what the page causes. WELCOME answers
>   `{protocol, conn_id, build, heartbeat_ms: 20000, max_frame, server_ms}`. Another protocol
>   number gets BYE `PROTOCOL`: the page reloads, which brings the matching build.
> - **The new-build notice.** The first WELCOME's `build` (the panel stamp `/api/info` serves as `panelVersion`, §host.routes) is the
>   page's own. A later WELCOME with another stamp means the gateway now serves a newer panel:
>   the page reloads itself when nothing unsaved would be lost, else says so (the existing
>   `maybeReloadPanel`). A release stamp is fixed for the process, so a new build is always a
>   restart, a reconnect and a WELCOME; a debug build reads its assets from disk, re-stamps every
>   6 s and announces a change as BUILD. This replaces the stamp check in the 6 s `/api/info` poll.
> - **Heartbeat.** The page sends PING after `heartbeat_ms` without a frame from the server; no
>   PONG within 10 s ends the link. The server sends protocol pings every 30 s; any inbound
>   frame or pong keeps the link, and 75 s of silence ends it with BYE `IDLE`. On `online` and
>   on the page becoming visible, the page pings at once.
> - **Reconnect.** After the link drops the page reconnects at 0.5 s, doubling to 30 s, ±20%
>   jitter, reset by a WELCOME; `online` or becoming visible tries at once. A connect that fails
>   is followed by one plain `GET /api/info`, whose 401 takes the page to the lock page as every
>   `api()` call does (§host.session).
> - **BYE.** `SHUTDOWN` (the gateway is stopping: reconnect with backoff), `SLOW` (this link's
>   queue overflowed: reconnect at once and resync), `PROTOCOL` (reload), `IDLE`, `LIMIT`.
>   Requests still open when a link ends are refused locally with `bus.offline`.
> - **The link's state shows only when it matters.** Connected, nothing is drawn. Down for more
>   than 3 s, one quiet line in the chrome says the panel is reconnecting; it goes when WELCOME
>   arrives.
>
> ## §bus.topics — Subscribing, events and resync
>
> - A topic is `(module, sub, key)`; `key` "" in a SUBSCRIBE means every key of the sub. Pages
>   subscribe to what they show (DATA.CONSOLES for each connection whose consoles are loaded) and
>   the client counts references, so two views of one topic subscribe once.
> - **Subscribe, then read.** A page subscribes first and reads the state second; an event that
>   arrives before the read's answer is applied by revision like any other, so nothing falls in
>   the gap between them.
> - **Events carry state, not deltas.** A CHANGED event is the whole record with its revision. A
>   page applies an event whose revision is newer than what it holds and ignores the rest, so a
>   duplicate, an echo or a late event is harmless.
> - **Order.** Events on one topic leave in commit order: the owner publishes while it still
>   holds the write that produced the revision. One link delivers its frames in order.
> - **Resync.** After every WELCOME the page re-subscribes everything it holds and re-reads each
>   topic's state, reconciling exactly as an event would. There is no replay buffer: the bus
>   never has to know what a page missed.
> - **Backpressure.** Publishing never waits: a frame that does not fit a link's queue ends that
>   link with BYE `SLOW`, and its resync puts it right.
>
> ## §bus.requests — Requests and handlers
>
> - A module serves a sub by registering a handler with the hub when its plugin starts; the
>   registration is a guard, so stopping the plugin unregisters it and later requests answer
>   `bus.unavailable` (the bus's form of the plugin boundary's 503, §host.routes).
> - A handler receives the decoded body, the key and the requesting page's `page_id`, and
>   publishes the events its write causes with that `origin`. Writes that do not come from a
>   page (the Data agent saving a console) publish with `origin` "".
> - **Writes travel on the bus** like reads. A write that may be retried names the revision it
>   was based on (compare-and-swap), so a retry after a lost response cannot apply twice: it
>   meets a revision that moved and is refused, and the resync that follows the reconnect has
>   already shown the page its own write.
>
> ## §bus.editing — One person edits
>
> The bus keeps every open window current; it does not make simultaneous editing a feature.
> swiss is edited by one person, and two windows on the same console (the desktop GUI beside a
> browser tab, or two tabs) are one person's two views. One person does not type into two
> windows at once, so the store does not merge concurrent edits: every write names the revision
> it read, a stale write is refused with a served code, a view with no draft follows the newer
> record, and a view holding a draft keeps it until the person chooses between the server's copy
> and the draft. Nothing is lost silently and nothing is merged silently.
>
> If merging is ever needed, it is a **three-way merge**: base = the record at the revision the
> draft started from, ours = the draft, theirs = the current record. A clean merge is saved at
> theirs' revision; a conflicting hunk goes to the person. Never last-writer-wins, never a
> character-level OT or CRDT. (The tab keeps its base revision today; a merge would also keep
> the base text.)
>
> ## §bus.codegen — The generated code
>
> - **One source.** `crates/swiss-bus/proto/` is the only place a message is defined. Changing a
>   message means editing the `.proto` and regenerating both sides in the same commit.
> - **Rust.** `cargo run --manifest-path crates/swiss-bus/protogen/Cargo.toml` compiles the
>   sources with protox and writes `src/pb/swiss.bus.v1.rs` with prost-build. The file's header
>   records a SHA-256 over the sources; a swiss-bus unit test recomputes it, and a mismatch fails
>   with the command to run. The shipping crate needs only `prost` with `derive`.
> - **TypeScript.** `npm run proto` runs buf with protoc-gen-es (`target=js+dts`,
>   `import_extension=js`, `rewrite_imports` to the vendored runtime) into `panel/src/bus/pb/`.
>   `build.mjs` copies `*_pb.js` into the served tree as written; `tsc` reads the `.d.ts`.
>   `npm run check` regenerates into a scratch directory and fails on any difference.
> - **Golden frames.** `testdata/golden.json` holds named frames as hex beside their meaning.
>   Both suites decode each one, compare the fields, and re-encode it to the same bytes, so the
>   two generators can never drift apart on the wire.
> - **Lint.** buf lint runs with `ENUM_ZERO_VALUE_SUFFIX` excepted: `MODULE_BUS = 0` and
>   `SAVE_MODE_AUTO = 0` are meaningful zeros on purpose.
>
> ## §bus.security — Who may connect
>
> `GET /api/bus` is a host-owned route in the main tree, behind the loopback guard and the admin
> session gate like every `/api/*` (§host.boundary, §host.session). On top of the cookie, the
> upgrade **must** carry an `Origin` naming the request's own `Host`, port included; absent or
> different is a 403, because a WebSocket handshake is not covered by `SameSite` or CORS. The CLI
> key (`X-Swiss-Key`) also opens it, for the integration harness. Every page of the session sees
> every topic: there is one user.

---

## 5. Phase 1 in detail

### 5.1 The core

- New crate `swiss-bus`: depends on `swiss-core`, `axum` (`ws`), `tokio`, `prost` (`derive`),
  `bytes`. `swiss-host` depends on it and carries `Arc<Bus>` in `RuntimeServices`, the way it
  carries the connection catalog; subsystem crates that publish name `swiss-bus` directly (a
  foundation crate, like `swiss-core`, so "no subsystem depends on another" still holds).
- The root composition creates the hub, mounts `/api/bus` in the main tree, and on graceful
  shutdown sends BYE `SHUTDOWN` to every link.
- Panel `bus/link.ts` (the socket, handshake, heartbeat, backoff, resync) and `bus/bus.ts`
  (the API the views use):

```ts
// The API the views use (sketch).
export const pageId: string;
export function busRequest<Q, R>(route: Route, key: string, req: Q, reply: GenMessage<R>): Promise<R>; // rejects with BusFault
export function busOn<T>(topic: Topic, id: number, body: GenMessage<T>, fn: (msg: T, origin: string) => void): () => void;
export function busRelay<T>(route: Route, key: string, msg: T): void;
export function busOnResync(fn: () => void): () => void;
```

- `servedError` learns faults: `servedFault(fault)` uses the same code table.

### 5.2 DATA.CONSOLES

Server (`swiss-plugins/src/db_consoles.rs`):

- The store gets the bus handle; `put`, `remove` and `clear_closed` publish CHANGED, REMOVED and
  CLEARED **inside the write lock**, so event order is revision order. The Data agent's saves
  (`llm_db.rs`) go through the same store, so they publish too, with `origin` "".
- **LIST is paged.** One connection's consoles can hold far more than a frame (each text may
  be 2,000,000 bytes), so `ConsolesPage {conn, after}` answers `ConsoleList {consoles, next}`:
  records in id order after `after`, cut before the response would pass 2 MiB (never fewer
  than one record), `next` the last id sent or "" at the end. The page reads until `next` is
  ""; an event arriving between pages is applied by revision like any other.
- The handler for (6, 1) serves LIST, PUT, REMOVE and CLEAR_CLOSED with the same lease check
  and the same refusal codes as today (`console.exists`, `removed`, `changed`,
  `changedDelete`, `tooLarge`).
- **Deleted:** the `/api/llm/db/consoles` GET/POST/DELETE routes and their handlers.
- A console the agent writes reaches the page twice, by the agent stream's own frame and by
  the CHANGED event; applying by revision makes the second a no-op.
- The comment on `ConsoleStore` states §bus.editing in code: why compare-and-swap and refuse,
  why no merge, and that a merge, if ever, is three-way.

Panel:

- `data-console-store.ts` calls `busRequest` instead of `api()`; the debounce, the serialized
  writes per console and the pending/failed states stay. A fault maps to the same
  `PutResult` failure with its `code`.
- `data-tabs.ts` subscribes to (6, 1, conn) when a connection's consoles load. The reconcile in
  `dbConsolesRefresh` becomes one function per record, `applyServerConsole(conn, record)`, used
  by events, by the first read and by every resync:
  - a clean tab takes a newer record (text, title, revision, caret kept);
  - a clean tab whose record closed elsewhere closes and is filed in Recently closed;
  - a console opened elsewhere joins the strip without taking the front;
  - a tab with a draft is never rewritten (its save meets the newer revision and the conflict
    bar asks, as now);
  - a pending tab whose text equals the server's record adopts that revision and turns clean
    (a write whose response was lost to a reconnect);
  - REMOVED drops the record from the strip if clean and from Recently closed; CLEARED drops
    the closed ones.
- **Deleted:** the focus and visibility catch-up (`dbConsolesCatchUp` and its listeners). The
  epoch guard, the close retry and the conflict bar stay.
- The comment on the sync module states §bus.editing in code, as on the store.

### 5.3 The new-build notice

- WELCOME's `build` replaces the stamp check in `loadInfo` (the 6 s poll still loads info for the
  token list until that has its own phase).
- A debug build's hub re-stamps every 6 s and publishes BUILD to every link.

### 5.4 PANEL.PREFS

- A language, theme or code-scheme choice made by the person is applied, stored, and relayed as
  `Prefs` with the field that changed.
- A received relay is stored and applied the same way, without relaying again: the language
  through `setLang` (which repaints the chrome and the page), the theme through `applyTheme`.
- Relays reach only open pages. A page opened later reads its own storage; a page that was open
  when the choice was made has already stored it, the desktop window included.

## 6. Amendments to other chapters (with the code)

| Section | Amendment |
|---|---|
| SPEC chapter table and map | Row 0019 §bus (crate `swiss-bus`), its sections in the map |
| §arch.crates | `swiss-core ← swiss-bus ← swiss-host`; the `swiss-bus` row; subsystem crates may name `swiss-bus` |
| §arch.deps | prost stays `derive`-only in the shipping graph; the generators (protox, prost-build; buf, protoc-gen-es) are dev tools outside the workspace and in the panel's devDependencies |
| §host.routes | `GET /api/bus` in the host-owned table |
| §host.session | The bus upgrade joins "Protected"; its mandatory `Origin` |
| §panel.vendor | `@bufbuild/protobuf` 2.16.0 (Apache-2.0 and BSD-3-Clause): the import closure of `dist/esm`, byte-identical, pinned by SHA-256. Its upstream `.d.ts` files are copied as they are instead of a hand-written mirror, because the generated declarations reference the runtime's real types |
| §panel.toolchain | Generated `*_pb.js` files are copied, not erased; `proto:check` joins `npm run check` |
| §panel.i18n | The language preference is stored per browser and **follows across open pages** (the relay); the `served.bus*` codes; the theme sentence the same way |
| §data console ("Another window on the same connection") | Rewritten: events instead of the focus catch-up; the pending-adopts-equal-text rule; §bus.editing cited |
| §testing | The codegen freshness gates and the golden frames |
| §decisions | ADR-039 (below) |

**ADR-039 — The event bus: one WebSocket, protobuf frames, generated codecs.** Accepted.
Spec: §bus. Pages kept other windows' changes out of sight until a reload or a focus, and
every live view polled on its own timer. One socket per page now carries
requests, events and relays; module numbers are chapter numbers; the codecs are generated
from one `.proto` set; writes go over the bus from the first delivery because nothing is
released yet. Single-user editing is the product's shape, so concurrent edits are refused by
revision, not merged; a merge, if ever, is three-way. Rejected:

- **SSE for events plus HTTP for writes.** Two transports, and no page-to-page path for relays.
- **JSON frames.** No schema, no generated types, and every new message hand-checked twice.
- **A hand-written codec.** A grammar written here (§arch.deps); the owner chose generation.
- **protoc, or prost-build in a build script.** A system binary, or the generator in every
  build's graph; protox outside the workspace needs neither.
- **ts-proto, protobuf.js.** ts-proto emits `enum` (not erasable) and needs protoc or buf
  anyway; protobuf.js's static output is a CommonJS-era runtime.
- **OT or CRDT (Yjs, Automerge).** They solve two people typing at once, which this product
  does not have.
- **The terminal's socket.** A per-session byte pump with its own tickets; unrelated.

## 7. Later phases (planned, not numbered yet)

What each replaces, so the registry has room for it. Each takes its numbers when it lands.

| Topic | Replaces |
|---|---|
| HOST.RUNS: a run's state (queued, running, finished, **canceled**), its live output while a row is open, CANCEL as a request | the Remote page's live-output timer and its runs poll; `POST /api/runs/{id}/cancel` |
| REMOTE.HISTORY: a history row written or updated | the Remote page's `/api/remote/runs` poll |
| HOST.MEMORY: the memory reading | `loadMemory` in the 6 s poll |
| HOST.PLUGINS: a plugin's state | page polls of `/api/plugins` |
| MCP.SERVERS: a server's status | the MCP page's poll |
| DATA.ACTIVITY, DATA.ANALYSIS, DATA.REDIS (pub/sub and stream follow) | `data-activity`, `data-analysis` (1 s), `data-pubsub`, `data-stream` timers |
| PROCESS.APPS, PROCESS.LOGS | the Process page's status and log timers |
| JOBS.RUNS | the Jobs page's poll |

## 8. Acceptance tests

Rust (`swiss-bus`, `swiss-plugins`):

- every golden frame decodes to its fields and re-encodes to its bytes;
- the codegen digest matches the sources;
- HELLO → WELCOME; another protocol → BYE `PROTOCOL`; no HELLO in 10 s → closed;
- PING → PONG with the same `sent_ms` and `reply_to`;
- topic matching: exact key and "" for every key; unsubscribe stops delivery; refcount-free on
  the server (a page subscribes once per topic);
- RELAY reaches the other subscribed links, not the sender, with `origin` set; a relay off the
  list → `bus.notRelayable`;
- a request to an unregistered sub → `bus.unavailable`; dropping the registration guard
  unregisters; an undecodable body → `bus.badBody`;
- a full outbound queue → BYE `SLOW` and the link closed, other links unaffected;
- the upgrade without the session → 401; with a foreign or missing `Origin` → 403;
- LIST pages: three consoles of 1.5 MB each come back in more than one page, every page under
  the frame limit, none lost or repeated; an empty connection is one empty page;
- a page sending an EVENT, a RESPONSE, a frame before HELLO or a second HELLO → BYE `PROTOCOL`;
- consoles over the bus: LIST, PUT (create, update, stale → `console.changed`), REMOVE (stale →
  `console.changedDelete`), CLEAR_CLOSED; each write publishes its event with the requester's
  `origin`; an agent save publishes with `origin` ""; events leave in revision order.

Panel (vitest, a fake socket):

- the same golden frames, from the same file;
- handshake, out-of-order responses by `reply_to`, `bus.timeout`, `bus.offline` on a drop;
- backoff 0.5 s → 30 s with jitter (fake timers), reset by WELCOME, at once on `online`;
- PONG overdue → reconnect; BYE `SLOW` → reconnect at once; `PROTOCOL` → reload path;
- resync: every topic re-subscribed and re-read after WELCOME;
- a later WELCOME with another build → `maybeReloadPanel`;
- the reconnecting line appears after 3 s down and goes on WELCOME;
- consoles: the four "another window" cases driven by events instead of focus; an own-origin
  echo changes nothing; a pending tab whose text equals the record turns clean; REMOVED and
  CLEARED; a fault shows the conflict bar in the reader's language;
- prefs: a flip relays once; a received relay applies without relaying back;
- `proto:check` fails on a stale generated file.

Live, after a deploy: two pages (the headless walk) on 19998 or a scratch home, never the
owner's own consoles: a language flip in one flips the other; a console saved in one shows in
the other without a focus; stopping the gateway shows the reconnecting line, starting it again
reconnects and (with a new build) reloads.

## 9. Delivery order

1. `swiss-bus`: protos, generator, generated code, hub, link, route, golden and unit tests;
   mounted at `/api/bus`; chapter 0019, ADR-039, the map, §arch, §host amendments.
2. Panel: vendored runtime, `npm run proto`, generated code, `build.mjs` and lint changes,
   `bus/` client with its tests, the golden test; the build notice and the reconnecting line.
3. DATA.CONSOLES: server handler and events, the panel store and tabs on the bus, the HTTP
   routes and the focus catch-up deleted, the §data amendment, the comments of §bus.editing.
4. PANEL.PREFS: the relay and the §panel.i18n amendment.

Each step ends with `npm run check` (capped workers) and `cargo test -j 2 --workspace` run
serially, clippy with `-D warnings`, and `npm run build` with the emit committed.
