# §panel — The admin panel

Chapter 0014 of the [swiss specification](../../SPEC.md).

One page application, embedded in the binary, served on the same loopback port as everything
else. It is held to the product's four properties like the binary is (§product): pixels are
as accountable as bytes. No framework, no bundler, no runtime npm dependency, no web font
download.

| Where | What |
|---|---|
| `crates/swiss-panel/panel/src/` | The TypeScript source: shell (`main.ts`, `page-registry.ts`, `page-core.ts`, `plugin-palette.ts`, `last-page.ts`, `immersive.ts`, `pane-scroll.ts`), views (`views/*.ts`, one per page), the UI library (`ui/`), state slices, i18n (`i18n.ts`, `locales/`) |
| `crates/swiss-panel/panel/test/` | Vitest (happy-dom): behaviour tests and the gates (§panel.ui, §panel.i18n, §panel.lint) |
| `crates/swiss-panel/src/admin_assets/` | What is served: `index.html` (the shell and the SVG sprite), `ui.html` (the gallery), `styles/{base,ui,views}.css`, `js/` (the committed emit, §panel.toolchain), `js/vendor/` (§panel.vendor), `logo.svg` |
| `crates/swiss-panel/src/admin.rs` | The embedded asset server |

**Serving** (`admin.rs`). `rust-embed` over `admin_assets/`: a debug build reads every file from
disk per request (edit, emit, reload — no gateway restart). A release build embeds the tree
gzip-compressed: swiss-panel's `build.rs` gzips each file under its own name into `$OUT_DIR`
(no mtime and no name in the header, so one tree always gives the same bytes), which keeps
~7 MB of panel to ~2 MB of binary. A body is the embedded bytes borrowed from the binary,
never a per-request copy: `Content-Encoding: gzip` (with `Vary: Accept-Encoding`) to a client
that takes it — every browser — and inflated for that one response to a client that does not.
The shell (`shell()`) is served `no-store`. An asset (`asset()`) is served `no-cache` with a
strong ETag over its SHA-256 and the encoding sent, so every load revalidates — a new build
shows at once, as under `no-store` — and an unchanged module is a bodiless 304. `mime_of` is a
whitelist: a file whose type is not on it is refused, never guessed. `asset` guards the path (a
traversal is a 404). The **panel version stamp** is a SHA-1 over every file's path and SHA-256
(rust-embed's, compile-time in release), handed to the panel by `/api/info`; a panel that sees
the stamp change reloads itself, so a rebuilt gateway is never driven by a stale tab. The build
script tracks `admin_assets/` (`rerun-if-changed` on the directory), so after `npm run build`
the next release build re-embeds what changed, an added or removed file included.

**The rendering contract.** The page body is rebuilt only by an explicit load or a user action,
never by a poll and never while something in it has focus. A poll patches in place — the
sidebar rows, the detail header, a status cell — so a caret, a scroll position, an open menu
or a half-typed field survives every tick.

**Polling.** One `setInterval(poll, 6000)`; a tick is skipped while the tab is hidden, and
returning to it polls at once (`visibilitychange`). A poll is the memory reading
(`/api/memory`, §host.memory), `/api/info` (the version stamp) and the active page's own
`poll()`. A page whose server state reaches it on the bus (§bus.topics) reads nothing on the
tick: the Remote pages export no network poll (§remote.panel). The memory chip is a reading,
not a reload button: clicking it refreshes the reading only. The one explicit view refresh is
the `r` key (`refreshNow`), which keeps Data's manual reload alive.

**Pages.** A view module exports a `PageModule`:

```ts
interface PageModule {
  mount?: (ctx: { signal: AbortSignal }) => void | Promise<void>;
  unmount?: () => void | Promise<void>;
  poll?: () => void | Promise<void>;
  refresh?: () => void | Promise<void>;
  canLeave?: () => boolean;          // false = navigation, a language flip, a reload are held
  countText?: () => string;          // the context bar's count readout
  hasPendingChanges?: () => boolean; // the beforeunload guard
}
```

`mount` gets an `AbortSignal` that fires on leave; listeners and fetches hang off it. A late
module load that lost the race to a newer navigation is discarded (a sequence ticket). The
`beforeunload` guard asks the browser to confirm while `hasPendingChanges()` is true (staged
Data edits, an unsaved form).

**The API helper.** `api(path, opts)` adds the JSON content type; a 401 from `/api/*` is the
session gate (§host.session) and sends the tab to `/`, which answers with the sign-in page.
`apiJson<T>()` is the one fetch → parse → report path for a mutation: a failure toasts the
API's own `{error}` (or "HTTP N"), a network failure toasts "is the gateway running", and
both return `null` — every caller is `if (!j) return;`, and none of them may fail silently.
API types are copied from the Rust serde structs field by field (§panel.toolchain).

**Theme.** `swiss_theme` holds the preference — `light`, `dark`, or absent for auto (the OS's
`prefers-color-scheme`, followed live). The result is `<html data-theme>`. Explicit choices follow across open pages through the
PANEL.PREFS relay (§panel.i18n), including the desktop window. The button shows what
a click switches to (a moon on light, a sun on dark) and the first click pins the choice for
this browser. Blocked storage keeps the choice for the page load.

**Keyboard.** The shell listens at `document`. Escape closes the outermost layer first: sheet,
then menu, then the MCP run history popover, then Focus mode. Outside a text field: `r`
refreshes the view; on the sidebar page `/` focuses the filter, ↑/↓ walk the rows actually on
screen (a folded group is stepped over) and Alt+↑/↓ move the selected MCP through the list
(the drag-free reorder). A layer that handles a key stops its propagation. A sheet is modal:
its keys stay in it, Escape aside (`ui/sheet.ts initSheet`).

**Browser storage.** Panel-only preferences; nothing a store needs lives here, and every read
and write tolerates blocked storage.

| Key | Holds |
|---|---|
| `swiss_theme`, `swiss_lang` | Theme and language preference (absent = auto / English) |
| `swiss.lastPage` | `{pluginId: pageId}` — the last page per plugin (§panel.nav) |
| `swiss.rail.pinned` | The rail's pinned plugin list |
| `swiss.tokenId` | Which token the copied connect commands embed |
| `swiss.groups.<scope>.collapsed`, `swiss.groups.<scope>.last` | Fold state per group (carried across a rename) and the last group used (§panel.groups) |
| `swiss.dbGrid.<conn>_<schema.table>`, `swiss.dbFavorites`, `swiss.dbRowLimit` | Data column widths, favourites, the console's row limit (§data; the console history is the server's, §data.history) |
| `swiss.terminal.fontSize`, `.bell`, `.copyOnSelect`, `.hint` | Terminal preferences (§terminal.panel) |


**Parts.** The sections live in three files beside this one, in reading order:

1. [The shell and the library](01-shell.md) — navigation, the design language, code colour, the UI library
2. [Pages](02-pages.md) — per-page look, the grouped list, System, Plugins and Secrets
3. [Languages and the build](03-build.md) — i18n, the toolchain, the lint ratchet, vendored libraries
