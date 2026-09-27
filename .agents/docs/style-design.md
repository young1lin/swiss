# Overall Style Design (the swiss unified style spec)

> One-liner: **a pocket multitool on a loopback port** — one exe, ten workspace crates (nine shipping; swiss-it is the dev-only integration harness), one panel, where pixels are as accountable as bytes for "ruthlessly small, plugin-shaped, hot-pluggable, three ways to bring a tool in"; the loopback is the security boundary and a one-time sign-in link opens the UI (docs/48), credentials never touch disk, and in the monochrome hairline admin interface only status gets color.

## Design Principles (four properties + loopback security)

The four properties come from the top of AGENTS.md ("A change that trades any of them away for convenience is the wrong change"); loopback security is a fifth, cross-cutting red line. Each principle has checkable style consequences:

1. **Ruthlessly small (small is the product)** — memory is the product itself, not a final optimization pass. Consequences: the panel has no bundler, no framework, no npm runtime dependencies (node is a dev-only dependency for the check gate, docs/36) — 3 CSS files loaded base → ui → views + native ES modules (docs/18 §0, docs/46 U3); lists are always paginated/filtered/collapsed server-side, full bodies and full results expand lazily (traffic.js:78-110); the subprocess-tree snapshot is cached 20s server-side; on the Rust side, current_thread, opt-level z, and every added dependency must justify itself (AGENTS.md Rust-specific rules).
2. **Plugin-shaped** — every capability is a plugin: the six contributions descriptor + config + actions + routes + pages + lifecycle (docs/09 §4); the host keeps only cross-cutting mechanism — zero business logic, zero match arms; panel navigation is rendered entirely from `/api/plugins` data, and the shell knows no specific plugin (page-core.js:12-30); a new tool reaches the panel by "contributing a descriptor, an action, a page" — never by editing the host (AGENTS.md).
3. **Hot-pluggable** — enabled / running / visible are three different states; disabling really releases tasks, connections, child processes, routes. Consequences: rows on the panel's Plugins page show the compiled/desired/actual state machine together with lastError (views/plugins.js:1-13); a disabled plugin's page renders a structured unavailable empty state instead of vanishing (page-registry.js:34-37); Jobs hides its tab when it cannot detect the subsystem (jobs.js:28-41).
4. **Three ways to bring a tool in** — a stdio child process / an HTTP endpoint / compiled into the binary. Consequences: proc MCPs start lazily and are reaped when idle ("do not make anything start eagerly for simplicity"); http/rest deliberately have no ping — a metered third-party endpoint would rather stay unknown than burn real requests; the first two are priced per process/socket and the third is nearly free, so the tools worth keeping eventually get compiled in.
5. **Loopback security is a boundary, not a default** — bind 127.0.0.1, non-loopback Host/Origin is always 403 (app.rs:230-252); the config stores only `${ENV_VAR}` references and `secret://` references, expansion happens only at adapter build time, and the panel masks them back out on echo; on top of it `/api/*` needs the admin session — the CLI key or a cookie from a one-time `swiss open` link (docs/48); to go remote, forward the port — never loosen the bind.

## Panel Visual Design System (CSS variable inventory, light/dark theme mechanism, typographic scale)

All tokens live in two blocks at the head of `styles/base.css`: `:root` (light) and `:root[data-theme="dark"]` (dark); a literal outside that block fails gate G4 (docs/46). The panel's file header writes this system's three enforcement rules: **mono only for values you will copy** (endpoint paths, tool names, JSON — never for identity; a 26px monospace title is the biggest flaw of "imitating Apple rather than following Apple"); **saturation only for status** (dots, accent, error red; type tags are always monochrome, docs/18 V6); **text needs a measure** (cards never stretch to the full window width; logs/traffic use the wide tier). The shape language is borrowed from Apple System Settings: sidebar + detail pane, grouped inset lists, one primary action per view with the rest in an overflow menu, a strict type scale, four weights and the 4pt grid.

Since docs/46 the shapes are drawn by one in-tree library: `crates/swiss-panel/panel/src/ui/*.ts` (exported by `ui/index.ts`) with its classes in `styles/ui.css`, shown in every state by the gallery at `/admin/ui.html`. CSS is three layers: `base.css` (tokens, reset, the shell), `ui.css` (the library's classes), `views.css` (workspace skeletons and page-local layout only). The swiss-ui-design skill §0 and §17 map every word to its function.

### CSS Variable Inventory (base.css:26-108; light values / dark values)

| Group | Variable | Light | Dark | Use |
| --- | --- | --- | --- | --- |
| Fonts | `--sans` | "Inter", "Segoe UI Variable Text", "Segoe UI", -apple-system, system-ui, then the CJK faces (PingFang SC, Hiragino Sans GB, Microsoft YaHei), sans-serif | the same one | Inter is used where installed but never shipped; the design is judged in Segoe UI Variable and SF (docs/46 U6); CJK after system-ui (docs/38 §5.2) |
| Fonts | `--mono` | "SF Mono", "JetBrains Mono", "Cascadia Mono", Consolas, ui-monospace, monospace | same as above | Only for copyable values |
| Type scale | `--f-title` 22 / `--f-head` 15 / `--f-body` 13 / `--f-label` 12 / `--f-caption` 11 (px) | — | — | Five steps, nothing in between; pane titles `--w-title` + -0.02em; captions all uppercase (letter-spacing .04-.06em) |
| Weights | `--w-body` 400 / `--w-name` 450 / `--w-emph` 500 / `--w-title` 600 | — | — | Four and only four (docs/46 U5): running text / identity in a list / a selected tab, a button, a caption, a band / titles. Any other `font-weight` fails gate G3 |
| Component sizes | `--ic-s` 12 / `--ic-m` 14 / `--dot` 6 / `--row-h` 42 (px) | — | — | The few sizes a component needs off the 4pt scale: a disclosure chevron, a glyph in a button or menu row, the status dot, a two-line list row |
| Grid | `--s1..--s8` = 4/8/12/16/20/24/32 (px) | — | — | The 4pt spacing grid (base.css:41) |
| Measure | `--measure` 920px, `--measure-wide` 1180px | — | — | Content is never unbounded; `.pane > *` hugs the left under a width cap, `.wide` relaxes it (ui.css, `pane()` / `paneBody()`; docs/18 V4) |
| Radius | `--r-card` 8 / `--r-row` 6 / `--r-btn` 6 / `--r-pill` 980 (px) | — | — | Three tiers card/row/button + the fully round pill (base.css:47-50) |
| Surfaces | `--bg` | #fafafa | #0f1012 | The canvas; the dark one is a warm black that "reads as paper, not as an IDE theme" (base.css:83-86) |
| Surfaces | `--sidebar` | #f4f4f5 | #141518 | The sidebar sits one step lower/higher than the canvas |
| Surfaces | `--bar` / `--card` / `--field` | #ffffff ×3 | #141518 / #191a1e / #0f1012 | Brightness is hierarchy — in the dark theme a card is lifted only by hairline + a 1px top highlight |
| Text | `--text` / `--text-2` / `--text-3` | #111114 / #6b7280 / #9ca3af | #e4e4e7 / #9a9ca3 / #66686f | The three-step text ladder; dark `--text` came down from #ededef, which glared at the lighter weights (docs/46 U5) |
| Lines | `--sep` / `--sep-soft` | rgba(0,0,0,.08) / .05 | rgba(255,255,255,.08) / .05 | hairline separators; no shadow-based layering anywhere on the page |
| Interaction | `--hover` | rgba(0,0,0,.04) | rgba(255,255,255,.05) | Hover background |
| Status | `--accent` / `--accent-text` | #2563eb / #ffffff | #3b82f6 / — | The only brand color; one primary per view |
| Status | `--green` / `--red` / `--amber` | #16a34a / #dc2626 / #d97706 | #4ade80 / #f87171 / #fbbf24 | Pick the deeper, less fluorescent one of each pair — "a column of nine saturated green dots is too bright for row labels" (base.css:59-61) |
| Shadows | `--shadow-card` | `0 0 0 1px var(--sep)` | plus `inset 0 1px 0 rgba(255,255,255,.04)` | The hairline ring is a card's entire edge — a shadow would let two near-identical surfaces masquerade as different layers; the grayscale must do honest work (base.css:55-58,78,104) |
| Shadows | `--shadow-btn` | none | none | Flat buttons |
| Shadows | `--shadow-sheet` / `--shadow-pop` | see base.css:80-81 | deepened | Only floating layers (sheet/menu) may cast a shadow |
| Syntax | `--syn-key` / `--syn-str` / `--syn-num` / `--syn-lit` / `--syn-punct` | GitHub-light muted hues | GitHub-dark muted hues | Only inside a code block (the JSON view, the SQL highlighter): the one place hue marks something other than state (skill rule 2) |
| Terminal | `--term-bg` / `--term-bar` / `--term-edge` / `--term-fg` / `--term-dim` / `--term-cursor` / `--term-sel` | a dark stage | one step below `--bg` | Derived from the panel's own colours (docs/46 U12); the terminal stays a dark stage in the light theme too |
| Scrollbars | — | 10px, transparent track, no arrows, rounded thumb | the same | docs/46 U8; Firefox gets `scrollbar-width: thin` |

### Light/Dark Theme Mechanism

- **pre-paint theme decision**: the head inline script at `index.html:10-22` reads `localStorage.swiss_theme` (default auto) before any first paint, resolves auto via `matchMedia`, and writes the result as `<html data-theme="light|dark">` — what lands in the DOM is always a concrete value, because "the white flash is exactly the thing being dodged". The dark block therefore needs to be written only once (base.css:27-30).
- **three-state toggle**: the theme button cycles auto → light → dark, with the icon swapping sprite per current theme (moon/sun, index.html:61-64); in the auto state it listens for OS changes and re-resolves in real time (main.js:70-118).
- **`color-scheme`** is declared with the theme (base.css:31,88), so scrollbars/form controls adapt natively; the only exception is `<select>`'s system popup layer — on Windows it is always white, so the panel takes it over entirely with custom dropdowns (dropdown.js:3-14).

### Typography and Numbers

- Global `font-variant-numeric: tabular-nums` (docs/18 V1; the base.css body block); count columns (request counts, byte counts, ms) are right-aligned, so polling patches never jitter in width.
- The weight ladder is the four `--w-*` tokens (titles `--w-title`, captions and bands `--w-emph`, a row's or a tool's name `--w-name`, running text `--w-body`); secondary `--text-2`, explanatory `--text-3`; disabled `opacity .4`.

## Layout Skeleton and Navigation

The shell is a rail beside two rows, with every dimension pinned:

| Region | Size | Contents | Source |
| --- | --- | --- | --- |
| rail | 56px wide | The brand mark; one 42px seat per pinned plugin — its 18px glyph over its name (one caption size for the whole rail, fitted from 10px down to a 9px floor by `fitRailLabels`); `⋯` opens the plugin palette. No edge of its own (docs/46 U7) | base.css `.rail*`; page-registry.ts |
| context bar | **40px, always present** | The plugin's title (its rail glyph + label); one underline tab per page when it has several (docs/39 S2); the count and memory readouts, theme, language and Focus at right | base.css `.ctxbar`; page-registry.ts |
| shell | the remainder | the MCP sidebar (resource layout only) + `#pane`, the one scroller | index.html; pane-scroll.ts |
| floating | — | `#sheet` (modal, backdrop click closes), `#toast`, the popup menus, the back-to-top button (shown past one screen of scroll, docs/46 U18) | ui/sheet.ts, ui/menu.ts, ui/to-top.ts |

- **Navigation = data**: both levels come entirely from `/api/plugins`; group order = the smallest page.order in the group, and grouping is a pure function (docs/13 D1/D2). For the current live table see .agents/docs/panel.md §2.
- **Page-body templates** (skill §6): ① content — `paneBody({ wide })` + a pinned `paneHead` (Tunnels, Jobs, Token, Settings); ② split / resource — the source list beside a detail whose `resHead` pins as name row + tabs (MCP › Servers); ③ workspace — `pane({ full })`, full-bleed, no scroll of its own (Data, Terminal).
- **Left-hugging measure**: `.pane > * { max-width: var(--measure); margin-inline: 0 }` (ui.css) — content hugs the left under a width cap, not centered, not full-bleed (docs/18 V4).
- **Pinned heads** (docs/46 U9): the shell measures the pinned layers into `--pin-title-h` / `--pane-head-h` (pane-scroll.ts); the hairline under them appears only once content is under it; an event list's day heading sticks at `--pane-head-h`.

## Component Pattern Inventory

| Component | Shape and rules | Source |
| --- | --- | --- |
| Buttons | `btn(label, { kind, icon, hidden })`: hairline default; `primary` solid accent, **at most one per view**; `ghost`; `danger` text colour only. `iconBtn` (with `aria-label`), `moreBtn` (the `⋯`) | ui/button.ts |
| One primary + ⋯ (V5) | One primary action per row/header, everything else behind ⋯; destructive items last in the menu or the row's trailing glyph; "a whole column of solid Starts is a whole column shouting" | docs/18 V5; ui/row.ts |
| Segmented control | `seg(items, current)`: `role=tablist`, counts `.seg-n`; L3 only; places no margin of its own (the page's flow does) | ui/seg.ts |
| Switch | `sw(on, label)`: a 38×22 capsule, `role=switch` + `aria-checked` | ui/switch.ts |
| Status dots | `dot(state, words)`: 6px; up green / down+error red / **idle hollow ring** / starting+stopping amber; the title is the state in words (dotTitle) | ui/status.ts; docs/18 V6 |
| Tags | `tag(text, { mono, tone })`: monochrome, a 5% wash; `tone: "bad"` is the one red (a failure in a list, never a red row); sidebar type glyphs carry no box | ui/status.ts; docs/39 S6 |
| Page head | `paneHead` (content page, pins) and `resHead` (resource: name row + tabs pin, the words scroll away) | ui/page.ts; docs/46 §3.2 |
| Sections and cards | `section({ cap, note, tools })` — sections `--s6` apart; `card(...)` — the hairline-ringed surface (`.group`, historical name); two cards in one section `--s3` apart | ui/page.ts |
| List row | `row({ lead, name, sub, err, cols, toggle, primary, more, detail })`: the ONE row; separators inset to the text column (U7); `detail` opens the item's record in a native `<details>` with the row's controls outside it | ui/row.ts |
| Label / value row | `kvRow(label, value, { mono })` — config and system facts; mono only for a value you would copy | ui/row.ts |
| Source-list row | `sideRow({ name, lead, tail, selected })`: 30px, `role=option`; the patch pass paints dot, tag and title | ui/row.ts; menu.ts patchSidebar |
| Group band | `groupNode(...)`: a `--sep-soft` band — 28px in the sidebar, 36px across its card at page density — chevron + name + count, `+` always, `⋯` on hover; the WHOLE head drags, `+`/`⋯` cancel the drag at dragstart | ui/group.ts; groups.ts wireHeadDrag; docs/35 |
| Event list | `timeline(items, { open, body })`: time column, sticky day headings, title, one line of arguments, who only when it varies, ×N, failure as a red tag, duration right (amber ≥ 1s); no box, no row lines, the open row is the card; `timelineMeta` is the open row's meta line. MCP Logs is the reference | ui/timeline.ts; docs/46 U10 |
| Code block | `jsonCodeNode(value, all, { oneLine })`: one formatted block, `--syn-*` tokens, decoded-string markers, Show all past 200 lines, short values on one line; `valueBlock({ label, notes, tools, text })` is the labelled block around it (one visible Copy, the rest behind ⋯) | ui/json-view.ts; docs/33 C3 as revised |
| Forms | `form`, `field({ label, control, required, meta, hint })`, `checkField`, `pair` (two columns wherever it sits), `formActions`, `hint`; fields and pairs `--s3` apart in a plain container, the grid gap inside `.form` / `.sheet-body` | ui/form.ts |
| Inline form | `inlineForm(...)`: create in place (Tokens, Secrets) — one row of fields + Group select + the one primary, `--s5` before the list | ui/page.ts; docs/35 §3 |
| Filter, pager, notes | `filterInput` (a section's search box), `pager({ status, prev, next })` (a live status), `note(body, { busy, err })`, `failNote({ text, why, action })` | ui/page.ts |
| Sheet | `sheet({ title, body, foot })` + `showSheet` (unhidden before painted), `openFieldSheet` (one field, inline error, stays open on refusal); `min(560px, 92vw)`, modal keys | ui/sheet.ts |
| Menus | `popupMenu(anchor, items)` floating, `anchoredMenu(host, items)` hung in a head's actions; headings, picks, glyphs, the held-edits dot, danger last; arrows walk, Escape closes | ui/menu.ts |
| Custom dropdowns | The native `<select>` stays the source of truth; `styleSelect` / `initSelects` draw the trigger and the list | ui/select.ts |
| Empty states | `emptyNode({ icon, title, hint, action })`: every page's empty state, and a switched-off plugin's page | ui/page.ts; docs/18 V7 |
| Back to top | `toTop(scroller)`: mounted once by the shell on `#pane`; past one screen of scroll; relabelled with the language | ui/to-top.ts; pane-scroll.ts |
| toast | Auto-dismisses in 3.4s, err gets a red edge; once per error, never repeated every 6 seconds | util.ts |
| Readouts (chip) | The context-bar readouts are one voice (docs/39 S5): count at `--f-label`, the mono memory value at `--f-caption`, both `--text-3`; the memory chip doubles as the refresh control | base.css; polling.ts |
| Not yet on the library | Tunnels and Jobs rows (`.tun-row`, polling.ts), Traffic / Remote runs / a job's runs (`.call*`), the client grid, Data's grid and Terminal's stage keep their own markup until their docs/46 phase (P3–P8) | views.css |

## Interaction Patterns

- **Polling contract (the whole panel's constitution)**: one beat every 6s, skipped outright when the page is invisible; polling **may only patch** — sidebar rows, detail headers, inline dots/copy/buttons/counts — never rebuild a structure that holds focus; rebuilds are triggered only by explicit loads or user actions; a structure-signature (sig) comparison decides the skip; rebuilds are deferred while a row or group drag is in progress (main.js:1-13,123-136;menu.js:91-105).
- **One unified fetch channel**: `api()` only adds Content-Type; `apiJson()` uniformly parses + error toast + returns null; callers all do `if (!j) return;`; a network failure must also toast ("is the gateway running?") — a silent null once made Commit do nothing after the confirmation (util.js:147-168).
- **Tiered confirmation for destructive operations**: confirm() copy names the consequence ("Clients using it stop working immediately"); table-level TRUNCATE/DROP requires **typing the table name verbatim**; group deletion explains that "rows are only moved, not deleted"; the terminal's save-config names that it will close N sessions; deleting a group is refused for the last one ("At least one group must remain") and its confirm names the first remaining group as where the rows go (sidebar.js:243-252).
- **Visible races via revision**: Plugins enable/disable and the Terminal/Jobs plugin config saves all carry a revision, and 409 puts the race right in your face; **200 but lastError = enable succeeded, START failed** gets one more toast beyond the row display (views/plugins.js:140-164).
- **Copying is the credential exit**: connection commands (claude/codex/.mcp.json) are assembled in the browser, and a secret appears only in the one-time box after creation/rotation ("shown only once"); clipboard API + legacyCopy fallback + toast confirmation (connect.js:59-128).
- **Edits live in buffers**: Data grid edits are all buffered client-side (amber bar LOCAL ONLY + SQL preview + single-transaction Commit + zero-query Discard); leave guards `canLeave/hasPendingChanges` + beforeunload; while typing/dragging/holding a sheet open, even the panel's self-reload yields (maybeReloadPanel, main.js:26-42).
- **Keyboard**: `/` search, `r` refresh, ↑↓ walk rows, Alt+↑↓ reorder, Esc closes layer by layer (sheet→menu→popover); Data console/Run forms Ctrl+Enter; terminal Ctrl+=/-/0 and Ctrl+wheel zoom, Ctrl+C/V with Windows Terminal semantics, right-click copy/paste (main.js:152-181;terminal-core.js).
- **Lazy expansion**: a Logs row paints its body only when opened, and opening a clipped reply fetches the whole of it by seq; Traffic still shows a preview first and fetches on expansion; 404 = already scrolled out of the ring buffer, and the copy says so plainly.
- **Drag reorder**: rows drag to reorder/regroup with upper/lower half-plane insertion lines; groups reorder by dragging their whole header (docs/35) — the + and ⋯ cancel the drag at dragstart so they keep every click — dropping on the upper/lower half of another WHOLE group picks before/after, and Move up/Move down in the ⋯ menu are the click-precise counterpart, present only where the move exists (groups.js wireHeadDrag/wireGroupDrop). Persisted through the `/api/groups/{scope}` family (docs/20 §3 — `PUT /api/order`, `PUT /api/tunnels/order` and `PUT /api/groups` are retired): `PUT /api/groups/{scope}` replaces the whole list ("here is the new list"), `POST /api/groups/{scope}/rename`, `PUT /api/groups/{scope}/members/{id}`, `PUT /api/groups/{scope}/order`; polling stays silent during a row or group drag (menu.js:91-105).

## Copy and Naming Style

- **UI copy is bilingual — English and 中文 through `tr()`/`trn()` with symbolic keys (docs/38; i18n.js + locales/en.js + locales/zh.js), sentence case, verb-first**: Start / Stop / Restart / Save & Restart / Delete / Revoke / Rotate / Move up / Move down / Run now / Test connection / Import .mcp.json / Commit (1 transaction) / Discard / Store / Open session (the English copies; a visible string that skips `tr()` fails `npm run check`). Primary buttons use verb-object or the bare verb — never OK.
- **A small set of status words**: up / down / error / idle / starting / stopping / reconnecting / connected / active / disabled / failed / unavailable / off; the dot's title carries the full story ("idle — the process is not running; the first request starts it", menu.js:75-86).
- **Count phrasing**: "N MCPs · N up · N down", "N rules, N active", "N plugins · N on · N failed", "n of m interactions", "N tokens", "N secrets", "N live" — always tabular-nums, with the middle dot · as the separator.
- **Time and size**: `whenLabel` shows a 24h clock time for today and adds the date for anything earlier; `ago()` just now / Ns / Nm; sizes keep one decimal in MB, k chars for characters, B/KB; the terminal session grace window's copy is written as a story ("A dropped socket does not end a session").
- **mono only for copyable values**: the mount path (a `<code>` in the resource head's state line), tool names, command lines, `secret://name` references, revision, SQL; titles, descriptions and the panel's own words are always sans.
- **Naming conventions**: plugin ids kebab-case (mcp/tunnels/data/jobs/terminal/process); Rust snake_case; JSON camelCase; config keys kebab-case; page ids short and plural (mcps/tokens/tunnels/jobs/secrets); hash route = page id.
- **Error copy states consequences, not technical detail**: "the Data view console is read-only — edit rows in the grid instead"; "the terminal plugin is not running"; "rolled out of the buffer".

## API Shapes and Error Response Conventions (/api/* envelope)

- **camelCase, absent means absent**: fields never get explicit null placeholders; the panel JS is the spec, and response shapes must match the Node build field for field (AGENTS.md; adminapi.rs module header).
- **The error envelope is uniformly `{"error": "…"}`**: every admin API non-2xx returns it; the panel's `apiJson` toasts `j.error` directly; HTTP status semantics are fixed: 400 bad parameters, 404 no such thing / no such route on old gateways, 405 does not exist (wrong-method is uniformly 404, aligned with Node), 409 revision race, 503 plugin disabled.
- **No auth layer**: `/api/*` is gated by the route-layer loopback guard; only the MCP endpoints (`POST /:name`, `DELETE /:name`) go through the bearer token — the token is an AI-client credential, not a panel login (app.rs:207,303-325;adminapi.rs:3-9).
- **Structured 503**: a disabled plugin's `/api/jobs`, `/api/terminal/*`, `/api/plugins/:id/config` return 503 + error copy; the panel degrades as "capability absent" (hide tab / empty state / no toast).
- **The revision field**: inventory, plugin config, and secrets write operations all carry `revision`; it is passed back in the POST body or query, and a mismatch is a 409 — optimistic concurrency made visible.
- **List responses carry everything a patch needs**: sidebar/row rendering uses a single GET (/api/mcps, /api/tunnels, /api/jobs, /api/plugins); polling fires no second request.

## Backend and Architectural Consistency (the plugin's six contributions, error discipline, memory discipline)

### The Plugin's Six Contributions (docs/09 §4; crates/swiss-host/src/host/descriptor.rs)

| Contribution | Shape |
| --- | --- |
| Descriptor | `id/kind/label/version/configSchemaVersion/configSchema/pages[]/routes[]/restartOnConfigChange/requires[]` — static, enumerable, self-describing |
| Config | One config row per plugin (`plugins.<id>`), schema-driven, revision concurrency control; the panel uses the schema description directly as help text |
| Services/Actions | Capabilities register into the host's Action/Run services; Jobs' action references are reused across plugins (the process command actions) |
| Routes | Declarative prefixes (`/api/tunnels`…) mounted by the host; enable/disable mounts/unmounts them, leaving no corpse behind |
| Pages/Slots | `PageDescriptor{id, pluginId, label, order, path, entry, sidebar}`; extension slots of the `nav.tools` kind also work (docs/09 §6) |
| Lifecycle | The state machine compiled→desired→actual; disable really releases tasks/connections/child processes/routes; restartOnConfigChange is declarative |

- **Composition-table philosophy**: `src/builtin.rs` is the one composition point — "add a plugin = one factory + one register line"; the reverse of registration order is start order (capability providers before consumers, Process starts first; Tunnels before the MCPs riding its tunnels); the six subsystem crates never depend on each other; when something cross-subsystem is needed, add the contract to the host, not a crate edge (docs/02).
- **Error discipline**: `.unwrap()` is forbidden on any path that touches config/network/database/filesystem; one crashing MCP must not take down the other seven; the loopback guard rejects non-local requests with only a warn log, not a panic; even a hand-written SHA-1 carries FIPS vector tests ("nothing else checks it", admin.rs:131-175).
- **Zero DOM on forwarding paths**: the `proc/http/rest` adapters pass payloads through as `&RawValue`, parsing only the envelope fields they route on — materializing a 2 MB body into a DOM is the single most expensive thing this process can do (AGENTS.md).
- **current_thread + a single exe**: a current_thread Tokio runtime; every thread of a work-stealing pool is a stack + allocator caches; opt-level z + fat LTO + codegen-units 1 — the slowness is deliberate.
- **Comment and test culture**: code comments are all English; comments record the "why" and the bugs whose tuition was already paid ("Port the reason, not just the code"); behavior changes ship with tests, and porting a module means porting its vitest too; the panel tree has a byte-level guard test (skipped on dev machines = "guards nothing while still reporting ok", admin.rs:263-271).
- **One source of truth (ADR-016, superseded by ADR-024)**: the panel is authored in `crates/swiss-panel/panel/src/*.ts`, emitted line-for-line into `crates/swiss-panel/src/admin_assets/js/` (committed), gated by `npm run check` (`crates/swiss-panel/panel/test/`); when docs and code conflict, code wins, but a discovered conflict must be fixed in the docs.

## Unified Style Checklist (new plugin/page onboarding) + Known Exceptions/Inconsistencies

### Checklist

0. Draw with the library (`panel/src/ui/`, the gallery at `/admin/ui.html`); a shape it lacks goes into the library first — function, `ui.css` classes, a gallery section, a test (skill §0) — and `npm run check` runs the seven gates.
1. The descriptor's six contributions are complete; pages come as `page(id, …, order, sidebar)` with an order that avoids the existing tiers (10/20/30…/70/1000/1001); the entry lands in `/admin/js/views/` or `/admin/plugins/`.
2. The page module implements `mount/poll/refresh/countText` (leave-blocking edits add `canLeave/hasPendingChanges`); polling only patches, skips on structure signature, and never rebuilds a focus-holding structure.
3. Empty states go through `emptyNode()` (ui/page.ts); disabled/503 has structured degradation and does not manufacture a toast every 6 seconds.
4. At most one `primary` per view; red actions go into the ⋯ menu; icons use sprites, not Unicode glyphs; type tags are monochrome mono.
5. Destructive operations confirm with the consequence named; table-level operations confirm verbatim; write operations carry a revision.
6. fetch goes only through `api/apiJson`; error copy states consequences; mono only for copyable values; counts use tabular-nums.
7. Backend: no `.unwrap()` (config/network/db/fs), no `serde_json::Value` materializing forwarding paths, dependencies defended with `default-features = false`, behavior changes ship with tests, comments in English.
8. Changing behavior an older build also had: the reasons live in the module comments and in docs/ — read them before changing a shape; edit the TypeScript in `panel/src`, never the committed emit.
9. Docs: the numbered docs/ series and the `.agents/docs/<id>.md` audit docs update in sync; status lines stay honest (see below).
10. A reorderable group drags by its whole header, whose buttons cancel the drag at dragstart (docs/35); rows offer the ⋯-menu Move up/down exactly where the move exists, show the drop as before/after accent edges, persist as a whole-list PUT, and poll rebuilds wait while a drag is in flight (the MCP sidebar is the reference).

### Known Exceptions/Inconsistencies (found in this audit; code wins)

1. **The docs/17 and docs/18 status lines still say "status: to-do"**, yet V1–V7 have all landed in the asset tree with `(docs/18 Vx)` comments (hairline tokens, the Inter stack + tabular-nums, the always-present 36px subbar, SVG sprites, the left-hugging measure, one primary + ⋯, monochrome tags + the idle hollow dot, the emptyHtml template) — a case of "implementation first, status line in arrears"; the next time docs are touched they should be flipped to implemented.
2. **The docs/13 body snapshot is stale**: it says mcp has two pages / seven top-level tiles; the code has three mcp pages (including Token 30) + the Gateway group's two-page composite page (builtin.rs:109-115;page-registry.js:13-20).
3. **Leftovers of V2's "no Unicode symbol icons"**: ⚙ (terminal settings), the PK key marker (data-grid), ✕/↩ (buffered-row buttons), ↑↓ (grid sorting and the Data list's direction toggle, data-view.js:261), ↺ (history-button copy), and Table ▾ are still glyphs; sprites do not cover everything.
4. **V5's "red goes into the menu" is not total**: Tokens' Revoke and Secrets' Delete are still inline red buttons (views/tokens.js;views/secrets.js).
5. **Two rename interactions remain**: group names — sidebar and tunnels alike — go through the one-field group sheet, which stays open on a server refusal so the error is readable next to what was typed (add-sheet.js:52-82), while MCP/table names still use the browser prompt (detail.js:39-41;data-edit.js:24); no unified rename component.
6. **Two generations of localStorage keys coexist**: `swiss.*` (the Node era) and `swiss_theme`/`swiss.terminal.fontSize`.
7. **Comments lagging**: the `util.js:36` state.view comment lists only five views; the `state.groups` comment still says "`default` is implicit and comes first" (util.js:13) and the base.css sidebar-groups comment still pins `default` as "always first and always present" (base.css:334-337), contradicting the ordinary-group model (util.js:6-9); the `pg_browser.rs` module header also has a "mid-port leftover" style comment (see data.md).
8. **Sidebar mechanism coupling**: `page.sidebar` is granted per page, yet the sidebar content always renders the MCP list; only mcps uses it today, and docs/13 §7 explicitly declines to fix it.
9. **The panel-asset boundary (ADR-024)**: panel sources are `panel/src/*.ts`; the committed emit under `admin_assets/js` stays plain ES modules with no bundler — the exe build needs no node.
10. **The grouped lists diverge at the edges**: only the MCP sidebar got grip handles and ⋯-menu moves — tunnel group headers are drop-into only (views.css:320-322) — and the two delete confirms disagree on where rows go: the sidebar names the first remaining group (sidebar.js:243-252), the tunnels still name the literal `default` (tunnels.js:92-97).
11. **docs/46 is mid-migration**: MCP › Servers (P2) is on the library; Traffic and Token (P3), Tunnels (P4), Settings (P5), Jobs and Remote (P6), Data (P7) and Terminal (P8) still draw their own shapes, so G5's frozen rows and views.css's `.call*` / `.tun-*` rules are still non-zero. Each page deletes its share when it moves (U16).

## Style and Design Observations

swiss's style is not a visual skin but **the isomorphic projection of one set of principles onto three layers — Rust, HTTP, and CSS**: "ruthlessly small" lands in dependency justifications and RawValue pass-through, and equally in server-side paging and lazy expansion; "plugin-shaped" lands in the six contributions and builtin.rs's composition table, and equally in a navigation shell rendered from /api/plugins data that knows no specific plugin; "hot-pluggable" lands in disable-really-releases, and equally in structured unavailable empty states and hidden tabs; "loopback is the boundary" lands in the 403 guard and ${ENV_VAR}/secret:// references, and equally in the API layering of "no panel login; the token belongs only to AI clients". The visual system itself is extremely restrained: a five-step type scale, the 4pt grid, hairline layering, saturation only for status, mono only for copyable values — every tightening corresponds to a lesson written into a comment (base.css:6-24). The biggest debt lies not in code but in doc status lines (docs/17/18 to-do vs implemented) and the small V2/V5 leftovers; both classes are named by the Checklist and the exceptions list.
