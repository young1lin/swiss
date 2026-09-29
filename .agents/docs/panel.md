# Panel map (crates/swiss-panel)

> REBUILT 2026-09-13. A scripting accident flattened this file to a single line and .agents/
> carries no git history, so there is nothing to restore from. What is below: the shell
> constitution and the cross-cutting sections come back verbatim from the session's reads;
> the asset table is regenerated from the tree (counts are real); the lost per-view notes
> are replaced by stubs that point at the surviving per-plugin files (data.md, terminal.md,
> mcp.md, tunnels.md, jobs.md, host.md) and the authoritative docs/. Re-verify a detail
> against the code before relying on it.

> swiss-panel carries no Rust business logic: the Rust side is just `crates/swiss-panel/src/admin.rs` (263 lines — rust-embed embedding, asset serving, the version stamp, SHA-1). The panel is **authored in TypeScript** in `crates/swiss-panel/panel/src/*.ts` since ADR-024 (docs/36): ts-blank-space erases the types line-for-line and the emit is COMMITTED under `src/admin_assets/js/` — no bundler, no minify, `cargo build` needs no node. That tree carries 1 index.html + 2 CSS + 68 own modules (52 root + 14 views + 2 locale tables) + vendored xterm.js/cronstrue/shlex. The panel's code is the spec for the admin API: every `/api/*` response shape must match what the panel reads field for field, and a shape change ships on both sides in one commit. Gate: `npm run check` in `crates/swiss-panel/panel/` — typecheck ×2 + eslint + emit-freshness + the vitest suite in `panel/test/`.

## Asset inventory (counts re-verified against the tree)

Since ADR-024 every `js/*.js` row below is the COMMITTED EMIT of a TypeScript source with the
same path under `crates/swiss-panel/panel/src/` (`js/foo.js` ← `panel/src/foo.ts`, line counts
track the source; edit the source, run `npm run build` there, commit both). CSS, `index.html`
and `js/vendor/**` have no TypeScript twin and are still edited in place.

| File (emit ← panel/src twin) | Lines | Role |
| --- | --- | --- |
| `index.html` | 160 | The only HTML shell: the theme pre-paint script, the SVG icon sprite, the rail/context-bar/shell skeleton, the shell-owned app zone, `#sheet`/`#toast` mounts, the module entry |
| `logo.svg` | 12 | favicon and brand mark |
| `js/main.js` | 228 | Entry: boot, the three-state theme, the language boot (`loadLocale` + the 文/A button wiring), 6 s polling, keyboard, panel-version self-reload |
| `js/h.js` | 169 | The node builder of docs/37 R5: `h(tag, props, ...kids)` (props are DOM properties, `data:`/`aria:` bags go through setAttribute), `fill(host, ...kids)`, `frag(...)` |
| `js/util.js` | 216 | The `api()`/`apiJson()` fetch wrappers, `esc/iconNode/emptyNode/typeTagNode/dotTitle/whenLabel/toast`, localStorage collapse persistence (the mutable `state` bag left with R4's slices) |
| `js/i18n.js` | 259 | The second language (docs/38): `tr()`/`trn()`/`tk()` over symbolic keys, the lazy zh dictionary load, the 文/A flip (no reload; refuses while edits are pending), `paintChrome`, `wireLabel()` for nav labels the gateway serves as text |
| `js/locales/en.js` | 1543 | The English dictionary — the fallback floor and the key authority (static import) |
| `js/locales/zh.js` | 1499 | The Chinese dictionary over the same keys, fetched only when the preference is zh |
| `js/ui-state.js` | 78 | R4 state slice: the shell/UI-owned fields |
| `js/mcp-state.js` | 106 | R4 state slice: MCP rows/detail/groups |
| `js/db-state.js` | 171 | R4 state slice: the Data page's `dbView()` record (mounted/unmounted lifecycle) |
| `js/job-state.js` | 100 | R4 state slice: jobs definitions and runs |
| `js/tunnel-state.js` | 91 | R4 state slice: tunnels connections/rules |
| `js/page-registry.js` | 473 | Navigation rendering and page lifecycle (`initPages/navigatePage/pollPage/refreshPage`), the legacy page table, synthetic pages |
| `js/page-core.js` | 95 | Page-bar paint primitives shared by page-registry (and its tests) |
| `js/last-page.js` | 72 | Per-plugin last page (docs/39 S4): a rail seat or palette row reopens where the user left that plugin (`localStorage swiss.lastPage`; deep links name their page outright) |
| `js/pane.js` | 324 | The detail pane's chrome and ONE delegated dispatch per event type on #pane (docs/37 R5): head, seg bar, empty state, the ... menu |
| `js/menu.js` | 212 | `patchSidebar` (structure signature + in-place patch), `popupMenu`, row dragging `wireDrag`, `tooltipOf` |
| `js/plugin-palette.js` | 210 | The "All plugins" overlay: search, pin, jump |
| `js/polling.js` | 332 | Data loaders: `loadList/loadMemory/loadTunnels/loadJobs`, `refreshMemoryNow` (the chip's memory-only click) and `refreshNow` (the r key), the row builders `jobRowNode/ruleRowNode/connRowNode`, chip copy |
| `js/immersive.js` | 99 | Shell-owned Focus/full-page control: folds the rail, keeps an in-flow bar for ordinary pages, docks the app zone into an opt-in workspace slot, restores it after pane replacement, and dispatches resize; never requests document fullscreen |
| `js/dropdown.js` | 234 | Panel-wide custom dropdown (a MutationObserver auto-takes-over every `<select>`) |
| `js/fields.js` | 356 | The form-field schema for the 6 MCP types (`TYPE_FIELDS/TYPE_LABELS`) plus rendering/reading |
| `js/add-sheet.js` | 173 | Add MCP sheet, group-name sheet, `.mcp.json` import |
| `js/connect.js` | 160 | Connection-command generation (claude/codex/.mcp.json), token secret resolution, `copyText` clipboard + fallback |
| `js/detail.js` | 743 | The detail pane: header, actions, the tabs of the selected MCP |
| `js/run.js` | 170 | Run tab: the argument form generated from the tool's inputSchema (`argFieldsNode/readRunArgs`) |
| `js/run-history.js` | 893 | Run history table + the history popover |
| `js/sidebar.js` | 224 | The #mcps sidebar rows (the MCP list — the page's main content) |
| `js/groups.js` | 505 | The one groups component every list renders through (docs/20) |
| `js/group-logic.js` | 101 | Pure grouping rules behind groups.js, unit-tested |
| `js/jobs.js` | 977 | The Jobs page logic |
| `js/jobs-v2.js` | 205 | Pure functions: `triggerSummary/historyMeta/v2ToForm/formToV2/defTemplate`, env-line parsing |
| `js/logs.js` | 529 | The logs viewer |
| `js/traffic.js` | 303 | The Traffic page rendering |
| `js/tunnels.js` | 437 | Tunnels page logic |
| `js/tunnel-sheets.js` | 408 | The tunnel add/edit sheets |
| `js/terminal-core.js` | 327 | Pure terminal logic: fit math, paste rules, resize frames (unit-tested) |
| `js/term-overlay.js` | 55 | The terminal overlay flashes (cols×rows, latency) |
| `js/data-view.js` | 1261 | Data page controller (docs/21/22) |
| `js/data-browsers.js` | 854 | Connection/table browsing |
| `js/data-grid.js` | 1405 | The grid renderer |
| `js/data-cell.js` | 246 | Cell editing |
| `js/data-sql.js` | 696 | The SQL console |
| `js/data-ddl.js` | 516 | DDL (create/alter) sheets |
| `js/data-csv.js` | 528 | CSV import/export |
| `js/data-edit.js` | 255 | Buffered edits |
| `js/data-filters.js` | 297 | Column filters |
| `js/data-form.js` | 326 | Form view |
| `js/data-structure.js` | 341 | Structure inspection |
| `js/data-suggest.js` | 183 | AI suggest wiring |
| `js/data-tabs.js` | 749 | The Data page's object tab strip (docs/42 T2): several tables/consoles at once, a small cap, LRU eviction that never touches a tab holding work |
| `js/data-tree.js` | 124 | The Data sidebar's tree rules (docs/43 M2), pure and DOM-free: SQL Tables/Views/Routines sections, the Postgres catalog nest, the redis keyspace ":" fold |
| `js/data-stream.js` | 547 | Redis stream follow (docs/45): the Follow tick and rate readout, the gap bar; `dbStreamJumpLatest` reopens the latest window on the shared `dbFollowReq` guard token, so a jump voids ticks still in flight |
| `js/data-activity.js` | 194 | Activity feed |
| `js/data-value.js` | 155 | Value sheet |
| `js/views/mcps.js` | 39 | Thin page entry: mount/refresh/unmount |
| `js/views/data.js` | 43 | Thin page entry (+ hasPendingChanges/canLeave guard) |
| `js/views/jobs.js` | 42 | Thin page entry |
| `js/views/traffic.js` | 23 | Thin page entry |
| `js/views/tunnels.js` | 27 | Thin page entry |
| `js/views/tunnel-forwards.js` | 27 | Thin page entry (the forwards seg) |
| `js/views/plugins.js` | 267 | The Plugins page |
| `js/views/secrets.js` | 338 | The Secrets page (docs/19) |
| `js/views/tokens.js` | 369 | The Tokens page |
| `js/views/remote.js` | 365 | The Remote page: targets table and sheets (docs/34) |
| `js/views/remote-runs.js` | 404 | The Remote Runs page: every run with its output |
| `js/views/system.js` | 101 | The System page (host diagnostics) |
| `js/views/terminal.js` | 1218 | THE terminal page: DOM/WebSocket wiring, xterm mount, fit, sessions, and the optional shell-control dock |
| `js/views/terminal-settings.js` | 105 | The terminal settings sheet |
| `styles/base.css` | 568 | Shell, rail/context bar/sidebar, tokens, buttons, chips, Focus and docked-full-page rules |
| `styles/views.css` | 1175 | Per-view styles (data grid, Terminal toolbar/dock, jobs, ...) |
| `js/vendor/*` | — | Vendored xterm.js (+ addons), cronstrue and shlex; pinned, never npm |
| `panel/src/types/*.d.ts` | — | Declaration-only (no emit): `api.d.ts` (1019) the wire shapes, `state.d.ts` (509), `dom.d.ts` (249), `runs.d.ts` (166), `terminal-view.d.ts` (110), `vendor.d.ts` (114) |

## Per-view notes

The detailed per-view notes survive in the sibling files and are NOT duplicated here:
- **Data** → `.agents/docs/data.md` (and docs/21, docs/22)
- **Terminal** → `.agents/docs/terminal.md` (and docs/14, docs/15, docs/23)
- **MCP pages** → `.agents/docs/mcp.md` (and docs/13)
- **Tunnels** → `.agents/docs/tunnels.md`
- **Jobs** → `.agents/docs/jobs.md`

### 5.8 Plugins (#plugins, js/views/plugins.js, docs/09)

- A row = dot (stateLabel is the dot's title; the status word has ceded its place to the dot, docs/18 V4/V6) + label (disabled adds · off) + grey line (id mono, the pages list or no page, the requires badge "needs connection-catalog (no provider)", lastError); version in the row title; the **switch** at the row end (enable/disable).
- Enable/disable exposes the revision race: `POST /api/plugins/:id/enable|disable {revision}`; **200 but a row with lastError = enable succeeded, START failed** — toast once in addition to the row display, otherwise "pressing Enable looks like nothing happened" (plugins.js:140-164).
- Footer "N plugins · N on · N failed" + mono revision; polling re-pulls the inventory + a structure-signature patch.

### 5.9 Secrets (#secrets, js/views/secrets.js, docs/19)

- The vault is host-owned (any plugin may depend on it, so it goes into no plugin; grouped with Plugins, docs/19 D6); **names only** — a row's value is the `secret://name` reference to be copied, not the credential (write-only: a forgotten value can only be re-stored, never echoed back).
- Form: name (lowercase-kebab regex validation, secrets.js:87) + value (password box) + the Group select + Store (primary) + New group; since docs/20 G6 the list renders through `js/groups.js` with dragging OFF — name order IS the order (the family's order route is a 400) — and a group label is a folder name, never a credential; rows = Copy ref / the red Delete (the confirm names that "things referencing it start failing"); writes carry a rev (PUT `/api/secrets/:name {value, rev}`, DELETE `?rev=`), and a race loses with 409.
- Old-gateway 404 → apiJson toasts once + an empty page (the same degradation as every host page); exports `rowsHtml/storeSecret/removeSecret/__setVaultForTest` for DOM-less tests (secrets.js:119-123).

## 6. Cross-Cutting Mechanics

- **Rendering contract** (main.js:1-13, the whole panel's constitution): 6 s polling **may only patch** (sidebar rows, detail header, row dots/copy/buttons); structural rebuilds are triggered only by explicit loads/user actions, never while focus is held; all editable content lives in sheets (separate subtrees).
- **Polling** (main.js:123-136): `setInterval(poll, 6000)`, skipped outright when `visibilityState !== "visible"`; each round runs `loadMemory(true)` (with the child-process tree; server-side 20 s cache + dedup + simply not run when there are no proc MCPs) + `loadInfo()` (the version stamp) + `pollPage()` (the current page). The memory chip is a reading, not a reload button: its click lands in `refreshMemoryNow` — exactly one `/api/memory?tree=1`, the active view is never rebuilt under the pointer (the toolbar refresh button is long gone; the click reloading the view was a regression, fixed 2026-09-13, Node 1cf23dc). The ONE explicit view refresh left is the `r` key: `refreshNow` = memory + `refreshPage()`, which stays Data's only manual reload — its poll deliberately leaves paged-in lists alone (polling.js:33-62).
- **Theme**: three states auto/light/dark (localStorage `swiss_theme`); the pre-paint script in head sets `data-theme` before the first frame (otherwise "a flash of white screen is exactly the thing being dodged", index.html:10-22); the button icon swaps sprite with the current theme (moon/sun); in auto it follows OS changes (main.js:70-118).
- **i18n (docs/38)**: every visible string goes through `tr()`/`trn()` (i18n.js) under symbolic `<module>.<semanticId>` keys; English copy lives in `locales/en.js` (the static fallback floor), Chinese in `locales/zh.js` (fetched only when the preference is zh). The preference is `localStorage swiss_lang` (no navigator.language probing); the 文/A button flips without a reload — remounting the current page, repainting the chrome, and refusing while a page holds pending changes. Nav labels the gateway SERVES as English text go through `wireLabel()` instead. Two machine gates live in `npm run check`: dictionary completeness across every locale (`i18n-complete.test.ts`) and the bare-literal zero gate (`i18n-ratchet.test.ts`).
- **Keyboard** (main.js:152-181): `/` focuses the sidebar search, `r` refreshes (memory AND the current view — the chip's click is memory-only, see the Polling entry above), ↑/↓ walks the visible rows (skipping collapsed groups), Alt+↑/↓ moves within the group, Esc closes layer by layer (sheet → menu → the history popover, then leaves fullscreen); no hijacking inside input boxes. The Data console and the Run form additionally get Ctrl+Enter.
- **Focus / full-page mode** (js/immersive.js): the shell owns one app zone containing Appearance and Focus/exit. On ordinary pages, Focus folds the Plugin Rail and keeps a minimal 40px Context Bar in normal flow, so shell controls cannot cover page actions. A workspace may opt in with `[data-shell-focus-slot]`: Terminal declares that slot in its toolbar, receives the same app-zone node while focused, and collapses the Context Bar to zero for one-row page fullscreen. The node is moved, never cloned, and a shallow `#pane` observer re-docks it after page-root replacement without watching xterm output. The browser's document fullscreen is NEVER requested. Every mode change dispatches **window resize** so Terminal refits rows/cols and wide views re-measure. The same pointer control or Esc (last in the Esc chain) exits and restores the app zone to the Context Bar. No localStorage key — refresh returns to the normal shell.
- **Custom dropdown** (dropdown.js): the native `<select>` stays hidden as the source of truth (value/options/change-event semantics unchanged, zero changes at call sites), with a trigger button + floating layer rendering the looks (a MutationObserver takes over automatically; views need not register); the reason: the Windows system popup list is always white — the panel's only control that breaks the theme (dropdown.js:3-14).
- **Sheet/menu/toast**: `#sheet` (add-sheet.js open/close; a backdrop click and Cancel close it, Enter submits the group name), `popupMenu` (menu.js:9-30; anchored to the button's left edge, flips up when it would not fit, danger red, sep divider), `#toast` (util.js:137-144; 3.4 s, err red).

## 7. Local State (localStorage)

| Key | Purpose | Source |
| --- | --- | --- |
| `swiss_theme` | Theme preference (default auto) | util.js:4;index.html:17 |
| `swiss_lang` | UI language (absent = English) | i18n.js:36 |
| `swiss.collapsed` | Sidebar group collapse | util.js:1 |
| `swiss.tun_collapsed` | Tunnel group collapse (independent of the sidebar) | util.js:2 |
| `swiss.tokenId` | Which token the copied commands embed | util.js:3 |
| `swiss.dbSqlHistory` | SQL history (≤50) | data-view.js:18-19 |
| `swiss.terminal.fontSize` | Terminal font size | views/terminal.js:44 |
| `swiss.lastPage` | Per-plugin last visited page | last-page.js:31 |

## 8. Key Source Files

| File | Role |
| --- | --- |
| crates/swiss-panel/src/admin.rs | Embedding (rust-embed), asset serving (mime whitelist + path guard), the version stamp (hand-written SHA-1) |
| crates/swiss-panel/panel/ | The panel's TypeScript sources (`panel/src/*.ts`), the `npm run check` gate (typecheck ×2 + eslint + emit-freshness + vitest in `panel/test/`) and the emit tooling; dev-only node dependency, never an exe build step |
| src/app.rs:230-279,482 | The `/` and `/admin/{*path}` routes (no-store), loopback-guard mounting |
| src/adminapi.rs:1-9,382-394 | The /api no-gate declaration, `/api/info`'s tokenEnv/panelVersion/build |
| src/builtin.rs:36-46,109-115 | Page-descriptor construction and the built-in plugin page table (the authoritative source of order) |
| src/plugins/terminal.rs:42-132 | The Terminal plugin descriptor (page order 70, routes /api/terminal) |
| crates/swiss-host/src/host/descriptor.rs | The PageDescriptor/PluginDescriptor contract |

## 9. Audit leftovers (kept from before the rebuild)

- **Stale comments around the group model**: the `state.view` comment (util.js:36) still lists only five views (no tokens/terminal/plugins/secrets); the `state.groups` comment still says "`default` is implicit and comes first" (util.js:13) and the base.css sidebar-groups comment still pins `default` as "always first and always present" (base.css:334-337) — both contradict the ordinary-group model the code now implements (util.js:6-9).
- **Sidebar-mechanism legacy** (docs/13 §7 explicitly says it will not be fixed): `page.sidebar` is granted per page, but the sidebar's content is sidebar.js hard-coding the MCP list; of the existing pages only mcps opens a sidebar — the mechanism/content coupling is a known wart.
- **The line references above survived the rebuild but the code has moved since some were written** — treat them as hints, not truth; re-locate before editing.
