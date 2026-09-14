# Overall Style Design (the swiss unified style spec)

> One-liner: **a Swiss Army knife on a loopback port** — one exe, eight crates, one panel, where pixels are as accountable as bytes for "ruthlessly small, plugin-shaped, hot-pluggable, three ways to bring a tool in"; the loopback is the security boundary, so the UI has no login, credentials never touch disk, and in the monochrome hairline admin interface only status gets color.

## Design Principles (four properties + loopback security)

The four properties come from the top of AGENTS.md ("A change that trades any of them away for convenience is the wrong change"); loopback security is a fifth, cross-cutting red line. Each principle has checkable style consequences:

1. **Ruthlessly small (small is the product)** — memory is the product itself, not a final optimization pass. Consequences: the panel has no bundler, no framework, no npm dependencies — 2 CSS files + native ES modules (docs/18 §0); lists are always paginated/filtered/collapsed server-side, full bodies and full results expand lazily (traffic.js:78-110); the subprocess-tree snapshot is cached 20s server-side; on the Rust side, current_thread, opt-level z, and every added dependency must justify itself (AGENTS.md Rust-specific rules).
2. **Plugin-shaped** — every capability is a plugin: the six contributions descriptor + config + actions + routes + pages + lifecycle (docs/09 §4); the host keeps only cross-cutting mechanism — zero business logic, zero match arms; panel navigation is rendered entirely from `/api/plugins` data, and the shell knows no specific plugin (page-core.js:12-30); a new tool reaches the panel by "contributing a descriptor, an action, a page" — never by editing the host (AGENTS.md).
3. **Hot-pluggable** — enabled / running / visible are three different states; disabling really releases tasks, connections, child processes, routes. Consequences: rows on the panel's Plugins page show the compiled/desired/actual state machine together with lastError (views/plugins.js:1-13); a disabled plugin's page renders a structured unavailable empty state instead of vanishing (page-registry.js:34-37); Jobs hides its tab when it cannot detect the subsystem (jobs.js:28-41).
4. **Three ways to bring a tool in** — a stdio child process / an HTTP endpoint / compiled into the binary. Consequences: proc MCPs start lazily and are reaped when idle ("do not make anything start eagerly for simplicity"); http/rest deliberately have no ping — a metered third-party endpoint would rather stay unknown than burn real requests; the first two are priced per process/socket and the third is nearly free, so the tools worth keeping eventually get compiled in.
5. **Loopback security is a boundary, not a default** — bind 127.0.0.1, non-loopback Host/Origin is always 403 (app.rs:230-252); the config stores only `${ENV_VAR}` references and `secret://` references, expansion happens only at adapter build time, and the panel masks them back out on echo; the panel's `/api/*` therefore needs no login — "on this machine there is exactly one operator" (adminapi.rs:3-9); to go remote, forward the port — never loosen the bind.

## Panel Visual Design System (CSS variable inventory, light/dark theme mechanism, typographic scale)

All tokens live in two blocks of `styles/base.css`: `:root` (base.css:26-82, light) and `:root[data-theme="dark"]` (base.css:87-108, dark). The panel's file header writes this system's three enforcement rules (base.css:6-20): **mono only for values you will copy** (endpoint paths, tool names, JSON — never for identity; a 26px monospace title is the biggest flaw of "imitating Apple rather than following Apple"); **saturation only for status** (dots, accent, error red; type tags are always monochrome, docs/18 V6); **text needs a measure** (cards never stretch to the full window width; logs/traffic use the wide tier). The shape language is borrowed from Apple System Settings: sidebar + detail pane, grouped inset lists, one primary action per view with the rest in an overflow menu, a strict type scale + the 4pt grid (base.css:2-4).

### CSS Variable Inventory (base.css:26-108; light values / dark values)

| Group | Variable | Light | Dark | Use |
| --- | --- | --- | --- | --- |
| Fonts | `--sans` | "Inter", "Segoe UI Variable Text", "Segoe UI", -apple-system, system-ui, sans-serif | the same one | Windows-first: use Inter when present, otherwise Segoe UI Variable Text with optical sizing (docs/18 V1, base.css:22-24,52) |
| Fonts | `--mono` | "SF Mono", "JetBrains Mono", "Cascadia Mono", Consolas, ui-monospace, monospace | same as above | Only for copyable values |
| Type scale | `--f-title` 22 / `--f-head` 15 / `--f-body` 13 / `--f-label` 12 / `--f-caption` 11 (px) | — | — | Five steps, nothing in between (base.css:33-38); pane titles 600 + -0.02em (views.css:15); captions all uppercase (letter-spacing .04-.06em) |
| Grid | `--s1..--s8` = 4/8/12/16/20/24/32 (px) | — | — | The 4pt spacing grid (base.css:41) |
| Measure | `--measure` 920px, `--measure-wide` 1180px | — | — | Content is never unbounded; `.pane > *` hugs the left under a width cap, `.wide` relaxes it (views.css:10-11;docs/18 V4) |
| Radius | `--r-card` 8 / `--r-row` 6 / `--r-btn` 6 / `--r-pill` 980 (px) | — | — | Three tiers card/row/button + the fully round pill (base.css:47-50) |
| Surfaces | `--bg` | #fafafa | #0f1012 | The canvas; the dark one is a warm black that "reads as paper, not as an IDE theme" (base.css:83-86) |
| Surfaces | `--sidebar` | #f4f4f5 | #141518 | The sidebar sits one step lower/higher than the canvas |
| Surfaces | `--bar` / `--card` / `--field` | #ffffff ×3 | #141518 / #191a1e / #0f1012 | Brightness is hierarchy — in the dark theme a card is lifted only by hairline + a 1px top highlight |
| Text | `--text` / `--text-2` / `--text-3` | #111114 / #6b7280 / #9ca3af | #ededef / #9a9ca3 / #66686f | The three-step text ladder |
| Lines | `--sep` / `--sep-soft` | rgba(0,0,0,.08) / .05 | rgba(255,255,255,.08) / .05 | hairline separators; no shadow-based layering anywhere on the page |
| Interaction | `--hover` | rgba(0,0,0,.04) | rgba(255,255,255,.05) | Hover background |
| Status | `--accent` / `--accent-text` | #2563eb / #ffffff | #3b82f6 / — | The only brand color; one primary per view |
| Status | `--green` / `--red` / `--amber` | #16a34a / #dc2626 / #d97706 | #4ade80 / #f87171 / #fbbf24 | Pick the deeper, less fluorescent one of each pair — "a column of nine saturated green dots is too bright for row labels" (base.css:59-61) |
| Shadows | `--shadow-card` | `0 0 0 1px var(--sep)` | plus `inset 0 1px 0 rgba(255,255,255,.04)` | The hairline ring is a card's entire edge — a shadow would let two near-identical surfaces masquerade as different layers; the grayscale must do honest work (base.css:55-58,78,104) |
| Shadows | `--shadow-btn` | none | none | Flat buttons |
| Shadows | `--shadow-sheet` / `--shadow-pop` | see base.css:80-81 | deepened | Only floating layers (sheet/menu) may cast a shadow |
| Terminal | `--t-*` (ttyd palette) | — | — | term-page carries its own black-background foreground set and does not join the light/dark theme (views.css:521-527) |

### Light/Dark Theme Mechanism

- **pre-paint theme decision**: the head inline script at `index.html:10-22` reads `localStorage.swiss_theme` (default auto) before any first paint, resolves auto via `matchMedia`, and writes the result as `<html data-theme="light|dark">` — what lands in the DOM is always a concrete value, because "the white flash is exactly the thing being dodged". The dark block therefore needs to be written only once (base.css:27-30).
- **three-state toggle**: the theme button cycles auto → light → dark, with the icon swapping sprite per current theme (moon/sun, index.html:61-64); in the auto state it listens for OS changes and re-resolves in real time (main.js:70-118).
- **`color-scheme`** is declared with the theme (base.css:31,88), so scrollbars/form controls adapt natively; the only exception is `<select>`'s system popup layer — on Windows it is always white, so the panel takes it over entirely with custom dropdowns (dropdown.js:3-14).

### Typography and Numbers

- Global `font-variant-numeric: tabular-nums` (docs/18 V1; the base.css body block); count columns (request counts, byte counts, ms) are right-aligned, so polling patches never jitter in width.
- The line-height and weight ladder: titles 600, group captions 500-600 uppercase, body default, secondary `--text-2`, explanatory `--text-3`; disabled `opacity .4` (base.css:150).

## Layout Skeleton and Navigation

A three-level vertical structure + one workspace, with every dimension pinned:

| Region | Height | Contents | Source |
| --- | --- | --- | --- |
| toolbar (top bar) | 48px | The swiss brand, `#viewSeg` level-one navigation (group tabs, horizontally scrollable with no scrollbar), a flex gap, the memory chip (doubling as the refresh control), the theme icon-btn | index.html:52-63;base.css:238-262 |
| subbar (page bar) | **36px always present** | Groups with ≥2 pages render `#subSeg` page tabs; single-page groups show `#subName`; `#countChip` counts on the right | index.html:65-75;base.css:278-285;docs/18 V3 (revising docs/13 D5's "hide the page bar for single-page groups") |
| shell | the remainder | `#side` (only the Servers page mounts the MCP sidebar) + `#pane` | index.html:77-88 |
| sheet/toast | floating layer | `#sheet` modal (backdrop click closes), `#toast` 3.4s | index.html:90-93 |

- **Navigation = data**: both levels come entirely from `/api/plugins`; group order = the smallest page.order in the group, and grouping is a pure function (docs/13 D1/D2;page-core.js:12-30). For the current live table (MCP group 3 pages / Tunnels / Data / Jobs / Terminal + the Gateway group's 2-page composite) see .agents/docs/panel.md §2.
- **Four page-skeleton shapes**: ① sidebar+detail (Servers: sidebar.js + pane.js); ② grouped card lists (Tunnels/Jobs: `.group` cards + `.cap` uppercase small captions, views.css:59-61); ③ exclusive full-width data surfaces (Data `db-root`, Terminal `term-page` — both explicitly exempt from measure, views.css:358,521); ④ single-card form pages (Token/Secrets/Plugins).
- **Left-hugging measure**: `.pane > * { max-width: var(--measure); margin-inline: 0 }` (views.css:10) — content hugs the left under a width cap, not centered, not full-bleed (docs/18 V4).

## Component Pattern Inventory

| Component | Shape and rules | Source |
| --- | --- | --- |
| Buttons `.btn` | Default hairline (card background + 1px ring), active pressed inward; `.primary` solid accent, **at most one per view**; `.ghost` no background; `.danger` changes only the text color; `.icon` small icon button | base.css:133-150,175 |
| One primary + ⋯ (V5) | One primary action per row/header (Start/Stop/Test/Run now/Open session), everything else goes into the ⋯ overflow menu; the red Delete lives only in the menu; "a whole column of solid Starts is a whole column shouting" | docs/18 V5;pane.js:157-177;polling.js:231-238 |
| seg segmented control | `role=tablist`, aria-selected gets the white background, counts `.seg-n` in the secondary color; `.seg:empty { display:none }` | views.css:38-54 |
| switch `.sw` | A 38×22 capsule, `role=switch` + aria-checked, colors follow accent | base.css:157-172 |
| Status dots `.dot` | 6px circles: up green / down+error red / **idle hollow ring** (`box-shadow: inset 0 0 0 1.5px var(--text-3)`) / starting+stopping amber pulse (`prefers-reduced-motion` turns the animation off); the dot's title is the status story (dotTitle), and status words have ceded their place to the dot | base.css:385-394;menu.js;docs/18 V6 |
| Type tags `.tag` / `side-type` | Monochrome mono 11px (`--sep-soft` background) — they only say "what this is" and carry no hue (mysql and redis have both been red; zero information) | views.css:99;base.css:328;docs/18 V6 |
| Group cards `.group` + `.cap` | White card + hairline ring + r-card; group titles are small uppercase captions | views.css:59-61 |
| Rows `.tun-row` | min-height 42px, sep-soft between rows, hover background, draggable cursor, drag insertion lines drop-before/after inlaid with accent | views.css:296-326 |
| Drag handles `.grp-grip` | The ONLY draggable thing on a group header — buttons living inside a draggable element turn a hand that moves a pixel into a cancelled drag and a silently dead button; whisper-quiet (opacity 0) until the header is hovered, like ⋯, but the grab/grabbing cursor is what teaches "this reorders" | base.css:341-349;sidebar.js:113-137 |
| Client grid `.cli-head/.cli-row` | A five-column grid, caption-uppercase headers, row hover, selected blends accent at 9% | views.css:342-350 |
| Data grid `.db-grid` | Collapsed table, sticky headers, three-state sorting, cells truncate with ellipsis at 340px | views.css:426-448 |
| sheet | `min(560px, 92vw)`, r 10px, three sections head/body/foot, `role=dialog aria-modal`; all editing lives in sheets (a separate subtree the poll cannot hit) | views.css:240;add-sheet.js |
| Floating menus `.menu` | Anchored to the trigger button's left edge, flips up when it cannot fit downward, `.danger` red, `.pick` blue-check single-select, `hr` grouping (the MCP overflow menu is a macOS-style three-section grouping) | views.css:204-229;menu.js:9-30 |
| Empty states `emptyHtml()` | One unified template: sprite icon + h2 title + a 44ch width-capped p + optional ghost action button; every page's empty state must go through this function; the terminal page is the only registered exemption | util.js:80-94;views.css:28-35;docs/18 V7 |
| toast | Auto-dismisses in 3.4s, err gets a red edge; once per error, never repeated every 6 seconds | util.js:138-145;base.css:273-279 |
| Custom dropdowns | The native select is hidden but kept as the source of truth (value/event semantics unchanged), rendered as trigger button + floating layer; a MutationObserver takes over globally and automatically | dropdown.js |
| chip | The top-bar memory chip (title hover details + click = refreshNow; the top-bar refresh button was deleted), the page-bar count chip, the Jobs footer chip | base.css:263-268;polling.js:33-55 |
| Terminal stage `term-page` | Full-width black surface, its own ttyd palette and --t-* foreground set, tab strip + status footer; does not join the light/dark theme | views.css:505-637 |
| Schedule builder `.sched` | Control groups for the five modes interval/daily/weekly/monthly/cron + one-sentence cronstrue feedback | views.css:640-675;jobs.js:179-285 |

## Interaction Patterns

- **Polling contract (the whole panel's constitution)**: one beat every 6s, skipped outright when the page is invisible; polling **may only patch** — sidebar rows, detail headers, inline dots/copy/buttons/counts — never rebuild a structure that holds focus; rebuilds are triggered only by explicit loads or user actions; a structure-signature (sig) comparison decides the skip; rebuilds are deferred while a row or group drag is in progress (main.js:1-13,123-136;menu.js:91-105).
- **One unified fetch channel**: `api()` only adds Content-Type; `apiJson()` uniformly parses + error toast + returns null; callers all do `if (!j) return;`; a network failure must also toast ("is the gateway running?") — a silent null once made Commit do nothing after the confirmation (util.js:147-168).
- **Tiered confirmation for destructive operations**: confirm() copy names the consequence ("Clients using it stop working immediately"); table-level TRUNCATE/DROP requires **typing the table name verbatim**; group deletion explains that "rows are only moved, not deleted"; the terminal's save-config names that it will close N sessions; deleting a group is refused for the last one ("At least one group must remain") and its confirm names the first remaining group as where the rows go (sidebar.js:243-252).
- **Visible races via revision**: Plugins enable/disable and the Terminal/Jobs plugin config saves all carry a revision, and 409 puts the race right in your face; **200 but lastError = enable succeeded, START failed** gets one more toast beyond the row display (views/plugins.js:140-164).
- **Copying is the credential exit**: connection commands (claude/codex/.mcp.json) are assembled in the browser, and a secret appears only in the one-time box after creation/rotation ("shown only once"); clipboard API + legacyCopy fallback + toast confirmation (connect.js:59-128).
- **Edits live in buffers**: Data grid edits are all buffered client-side (amber bar LOCAL ONLY + SQL preview + single-transaction Commit + zero-query Discard); leave guards `canLeave/hasPendingChanges` + beforeunload; while typing/dragging/holding a sheet open, even the panel's self-reload yields (maybeReloadPanel, main.js:26-42).
- **Keyboard**: `/` search, `r` refresh, ↑↓ walk rows, Alt+↑↓ reorder, Esc closes layer by layer (sheet→menu→popover); Data console/Run forms Ctrl+Enter; terminal Ctrl+=/-/0 and Ctrl+wheel zoom, Ctrl+C/V with Windows Terminal semantics, right-click copy/paste (main.js:152-181;terminal-core.js).
- **Lazy expansion**: traffic parameters/results and Logs full results all show a preview first, and only expansion fetches the full payload by seq; 404 = already scrolled out of the ring buffer, and the copy says so plainly.
- **Drag reorder**: rows drag to reorder/regroup with upper/lower half-plane insertion lines; groups reorder by a dedicated grip handle — never a draggable header, so the + and ⋯ keep every click — dropping on another header's half picks before/after, and Move up/Move down in the ⋯ menu are the click-precise counterpart, present only where the move exists (sidebar.js:113-137,269-331). Persisted as whole-list PUTs — `PUT /api/order`, `PUT /api/tunnels/order`, and `PUT /api/groups` for create/reorder/delete alike ("here is the new list"); polling stays silent during a row or group drag (menu.js:91-105).

## Copy and Naming Style

- **UI all English, sentence case, verb-first**: Start / Stop / Restart / Save & Restart / Delete / Revoke / Rotate / Move up / Move down / Run now / Test connection / Import .mcp.json / Commit (1 transaction) / Discard / Store / Open session. Primary buttons use verb-object or the bare verb — never OK.
- **A small set of status words**: up / down / error / idle / starting / stopping / reconnecting / connected / active / disabled / failed / unavailable / off; the dot's title carries the full story ("idle — the process is not running; the first request starts it", menu.js:75-86).
- **Count phrasing**: "N MCPs · N up · N down", "N rules, N active", "N plugins · N on · N failed", "n of m interactions", "N tokens", "N secrets", "N live" — always tabular-nums, with the middle dot · as the separator.
- **Time and size**: `whenLabel` shows a 24h clock time for today and adds the date for anything earlier; `ago()` just now / Ns / Nm; sizes keep one decimal in MB, k chars for characters, B/KB; the terminal session grace window's copy is written as a story ("A dropped socket does not end a session").
- **mono only for copyable values**: mount paths `.sub-path`, tool names, command lines, `secret://name` references, revision, SQL; titles and identity text are always sans.
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
- **One source of truth (ADR-016)**: the panel is edited directly in `crates/swiss-panel/src/admin_assets/` with its vitest suite (`crates/swiss-panel/panel-tests/`); when docs and code conflict, code wins, but a discovered conflict must be fixed in the docs.

## Unified Style Checklist (new plugin/page onboarding) + Known Exceptions/Inconsistencies

### Checklist

1. The descriptor's six contributions are complete; pages come as `page(id, …, order, sidebar)` with an order that avoids the existing tiers (10/20/30…/70/1000/1001); the entry lands in `/admin/js/views/` or `/admin/plugins/`.
2. The page module implements `mount/poll/refresh/countText` (leave-blocking edits add `canLeave/hasPendingChanges`); polling only patches, skips on structure signature, and never rebuilds a focus-holding structure.
3. Empty states go through `emptyHtml()`; disabled/503 has structured degradation and does not manufacture a toast every 6 seconds.
4. At most one `primary` per view; red actions go into the ⋯ menu; icons use sprites, not Unicode glyphs; type tags are monochrome mono.
5. Destructive operations confirm with the consequence named; table-level operations confirm verbatim; write operations carry a revision.
6. fetch goes only through `api/apiJson`; error copy states consequences; mono only for copyable values; counts use tabular-nums.
7. Backend: no `.unwrap()` (config/network/db/fs), no `serde_json::Value` materializing forwarding paths, dependencies defended with `default-features = false`, behavior changes ship with tests, comments in English.
8. Changing behavior the Node build also has: read the original Node module first (the comments hold the reasons), change Node + vitest first, then copy the whole tree; never edit `admin_assets` directly.
9. Docs: the numbered docs/ series and the `.agents/docs/<id>.md` audit docs update in sync; status lines stay honest (see below).
10. A reorderable list drags by a dedicated handle (never a header that carries buttons), offers the ⋯-menu Move up/down exactly where the move exists, shows the drop as before/after accent edges, persists as a whole-list PUT, and defers poll rebuilds while a drag is in flight (the MCP sidebar is the reference, sidebar.js:113-137,269-331).

### Known Exceptions/Inconsistencies (found in this audit; code wins)

1. **The docs/17 and docs/18 status lines still say "status: to-do"**, yet V1–V7 have all landed in the asset tree with `(docs/18 Vx)` comments (hairline tokens, the Inter stack + tabular-nums, the always-present 36px subbar, SVG sprites, the left-hugging measure, one primary + ⋯, monochrome tags + the idle hollow dot, the emptyHtml template) — a case of "implementation first, status line in arrears"; the next time docs are touched they should be flipped to implemented.
2. **The docs/13 body snapshot is stale**: it says mcp has two pages / seven top-level tiles; the code has three mcp pages (including Token 30) + the Gateway group's two-page composite page (builtin.rs:109-115;page-registry.js:13-20).
3. **Leftovers of V2's "no Unicode symbol icons"**: ⚙ (terminal settings), the PK key marker (data-grid), ✕/↩ (buffered-row buttons), ↑↓ (grid sorting and the Data list's direction toggle, data-view.js:261), ↺ (history-button copy), and Table ▾ are still glyphs; sprites do not cover everything.
4. **V5's "red goes into the menu" is not total**: Tokens' Revoke and Secrets' Delete are still inline red buttons (views/tokens.js;views/secrets.js).
5. **Two rename interactions remain**: group names — sidebar and tunnels alike — go through the one-field group sheet, which stays open on a server refusal so the error is readable next to what was typed (add-sheet.js:52-82), while MCP/table names still use the browser prompt (detail.js:39-41;data-edit.js:24); no unified rename component.
6. **Two generations of localStorage keys coexist**: `mcp_gateway_*` (the Node era) and `swiss_theme`/`swiss.terminal.fontSize`.
7. **Comments lagging**: the `util.js:36` state.view comment lists only five views; the `state.groups` comment still says "`default` is implicit and comes first" (util.js:13) and the base.css sidebar-groups comment still pins `default` as "always first and always present" (base.css:334-337), contradicting the ordinary-group model (util.js:6-9); the `pg_browser.rs` module header also has a "mid-port leftover" style comment (see data.md).
8. **Sidebar mechanism coupling**: `page.sidebar` is granted per page, yet the sidebar content always renders the MCP list; only mcps uses it today, and docs/13 §7 explicitly declines to fix it.
9. **The panel-asset boundary (ADR-016)**: `admin_assets` is edited directly in this repo — still plain ES modules, no build step.
10. **The grouped lists diverge at the edges**: only the MCP sidebar got grip handles and ⋯-menu moves — tunnel group headers are drop-into only (views.css:320-322) — and the two delete confirms disagree on where rows go: the sidebar names the first remaining group (sidebar.js:243-252), the tunnels still name the literal `default` (tunnels.js:92-97).

## Style and Design Observations

swiss's style is not a visual skin but **the isomorphic projection of one set of principles onto three layers — Rust, HTTP, and CSS**: "ruthlessly small" lands in dependency justifications and RawValue pass-through, and equally in server-side paging and lazy expansion; "plugin-shaped" lands in the six contributions and builtin.rs's composition table, and equally in a navigation shell rendered from /api/plugins data that knows no specific plugin; "hot-pluggable" lands in disable-really-releases, and equally in structured unavailable empty states and hidden tabs; "loopback is the boundary" lands in the 403 guard and ${ENV_VAR}/secret:// references, and equally in the API layering of "no panel login; the token belongs only to AI clients". The visual system itself is extremely restrained: a five-step type scale, the 4pt grid, hairline layering, saturation only for status, mono only for copyable values — every tightening corresponds to a lesson written into a comment (base.css:6-24). The biggest debt lies not in code but in doc status lines (docs/17/18 to-do vs implemented) and the small V2/V5 leftovers; both classes are named by the Checklist and the exceptions list, and the repair path (detour through the Node repo for a whole-tree copy) is itself one of this repo's most conscientiously enforced style rules.
