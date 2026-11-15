# 02 — Settings → System: an About section (version-hash, update check, diagnostics)

Implementation brief, not SPEC. The SPEC amendments in §7 land in the commit that makes each
paragraph true. Shared rules: [00-handoff.md](00-handoff.md). Depends on 01 (locale split) being
merged, because it adds keys.

## 1. Goal

The owner asked for "the current version-{commit-hash} in Settings". Today the build stamp
reaches `/health`, `/api/info`, `swiss status` and `swiss --version`, but no page shows it. This
brief adds an **About** section at the top of Settings → System with three rows:

1. **Version** — `swiss 0.2.2` with `a5a12a9c · built 2026-11-12 13:58` under it, a warn tag
   `dirty` when the hash ends in `-dirty`, and a copy glyph that copies the exact
   `swiss --version` line.
2. **Updates** — a button "Check for updates" that asks GitHub once, on click, and shows the
   outcome in the row. Brief 04/U3 later turns a positive outcome into "Update to vX"; build the
   row so that it can (one function that renders the row from a state value).
3. **Diagnostics** — a button "Copy diagnostics" that puts a short, name-free summary on the
   clipboard for a bug report.

## 2. Facts (verified 2026-11-12)

- `build.rs` stamps `SWISS_GIT_HASH` (short hash, `-dirty` suffix) and `SWISS_BUILD_TIME`
  (RFC 3339 UTC). `src/app.rs:56` `build_info()` returns `{hash, time}`; `/api/info`
  (`src/adminapi.rs:693`) serves `{tokenEnv, panelVersion, build}`.
- `src/cli.rs` `version_line()` is `swiss {CARGO_PKG_VERSION} ({hash}, {time})`. Its shape is a
  public contract (`swiss status` and `scripts/deploy.ps1` parse it). **Do not change it.**
- `src/update_check.rs` `check()` does one GitHub GET with a 10 s timeout and returns
  `UpdateInfo {current, latest: Option<Release{tag,url}>, newer}`; a 404 means "no releases".
- The machine-writing settings use AppContext seams set once at boot and **unset in every test
  composition**: `autostart` / `user_path` (`src/app.rs:129-134`, set at `src/server.rs:334`).
  An unset seam answers 503 (`machine_setting_unmounted`), and the panel's `readStatus()` turns a
  non-OK answer into `null`, which hides the row.
- Panel: `views/system.ts` `systemBodyNode()` builds the page; helpers are `section`, `card`
  (`ui/page.ts`), `row`, `kvRow` (`ui/row.ts`), `tag` (`ui/status.ts`), `copyBtn` / `copyText`
  (`ui/copy.ts`), `toast` (`util.ts`). `main.ts:96` `loadInfo()` reads `/api/info` on every
  WELCOME. `types/api.d.ts:26` types `build` as `Record<string, unknown>`.
- Refusals use `admin_error_code(status, code, message)` (`crates/swiss-host/src/reply.rs:40`);
  the panel maps `code` through `SERVED_CODES` (`i18n.ts:150`) to a `served.*` key.

## 3. Server

### 3.1 Build stamp

`build.rs` additionally stamps `SWISS_TARGET` from cargo's `TARGET` env var
(`println!("cargo:rustc-env=SWISS_TARGET={}", std::env::var("TARGET")...)`). `build_info()`
becomes `{hash, time, version, target}` where `version` is `CARGO_PKG_VERSION`. Additive only:
every existing reader of `hash` and `time` is unchanged. `version_line()` is unchanged.

### 3.2 `GET /api/update/check`

A new AppContext seam, same pattern as `autostart`:

```rust
pub update_check: std::sync::OnceLock<Arc<dyn crate::update_check::UpdateCheck>>,
```

`UpdateCheck` is an `async_trait` (or a boxed-future trait — follow whatever `swiss-host`
already uses for async seams) with one method returning `Result<UpdateInfo, String>`. The boot
sequence sets it to a `GithubUpdateCheck` that calls today's `check()`. The CLI keeps calling
`check()` directly.

- Seam unset (every test composition): **503**, the existing `machine_setting_unmounted`
  wording ("update check is not available in this composition"). The panel hides the Updates row.
- Success: **200** `{current: "0.2.2", currentVersion: "0.2.2 (a5a12a9c)", latest: {tag, url} | null, newer: bool}`.
- Failure: **502** `admin_error_code(BAD_GATEWAY, "update.unreachable", <the honest string>)`.

The route is called **only** on the button's click — never on mount, never on WELCOME, never on
a timer. The HTTP client is built per call and dropped (that is what `check()` already does), so
the resident gateway holds nothing between checks.

## 4. Panel

### 4.1 The section

`aboutSection()` in `views/system.ts` (or a new `views/system-about.ts` if `system.ts` would pass
~350 lines), placed first in `systemBodyNode()`, before This machine. It reads the gateway info
the page already has from `loadInfo()` (expose a getter if none exists; do not add a second
`/api/info` read on mount).

- **Version row**: name `swiss 0.2.2`, sub `<hash> · built <local date-time>` (format the RFC
  3339 time with the page's existing date helper and the user's locale). Tag `dirty` (warn tone)
  when the hash ends in `-dirty`. A `copyBtn` copying `swiss 0.2.2 (<hash>, <time>)` — rebuild
  exactly the `version_line()` shape from `build.version`, `build.hash`, `build.time`.
- **Updates row**: a state machine with six states, rendered by one `updateRowNode(state)`:
  `idle` (button "Check for updates"), `checking` (spinner, button disabled), `current`
  ("You have the newest release"), `newer` ("v0.2.3 is available" + a link "Release notes" to
  `latest.url`, opened in a new tab with `rel="noopener"`), `none` ("No releases published yet"),
  `error` (the `served.updateUnreachable` text plus a Retry button). From any end state the
  button checks again. The state is module-scoped and reset in `unmount()`.
  Before the first click the panel does **not** know whether the seam exists. Render the row
  from the start, and if the first click answers 503, remove the row for the rest of the page's
  life. A test pins that.
- **Diagnostics row**: button "Copy diagnostics". Calls `copyText()` with the text in §4.2 and
  shows a `toast` ("Diagnostics copied" / "Copy failed"). It reads only what the page already
  has or can read with GETs that are already public to the panel.

### 4.2 Diagnostics text

Counts and states, **never names, paths, hostnames, tokens, env var names, secret names, URLs,
or connection strings**. Exactly these lines, in English (it goes into an issue, not the UI):

```
swiss 0.2.2 (a5a12a9c, 2026-11-12T05:58:00Z)
target: x86_64-pc-windows-msvc
panel: <panelVersion>
ui: <lang> · <navigator.userAgent>
plugins: <n> on, <m> off
mcps: <total> (<running> running, <failed> failed)
memory: <rss MB> MB resident
```

Use whichever existing GET routes the panel already uses for the plugin list, the MCP list and
`/api/memory`; if one fails, write `<line>: unavailable` instead of failing the whole copy. If the
userAgent line feels like too much, keep it anyway — it is what a WebView2 vs browser bug needs.

### 4.3 Types and strings

- `types/api.d.ts`: `ApiInfoResponse.build` becomes
  `{hash: string; time: string; version?: string; target?: string}` (optional, so an older
  gateway behind a newer panel still renders: missing → show what is there).
- New `UpdateCheckResponse` type.
- Keys under `system.about*` in the shell part (`en/shell.ts`, `zh/shell.ts` after brief 01),
  and `served.updateUnreachable` mapped from `update.unreachable` in `SERVED_CODES`.

## 5. Tests

Rust (targeted, `-j 2`, `--test-threads=1`):

- `build_info` carries `version` and `target`; `version_line()` output is byte-identical in shape
  to before (an existing test may already pin it; extend it, don't loosen it).
- `/api/update/check` with the seam unset answers 503 and makes **zero** outbound calls.
- With a fake seam: newer → 200 `newer:true`; no releases → `latest:null`; error →
  502 `code:"update.unreachable"`.

Vitest (`--maxWorkers=2`, the touched files only, then `npm run check` once at the end):

- Mounting the System page makes **no** `/api/update/check` request (spy on the fetch stub).
- Each update outcome renders its state; 503 removes the row.
- The version copy text equals the `version_line()` shape; `-dirty` shows the tag.
- Diagnostics text: contains the lines above; given a fixture with named plugins, MCPs and a
  connection, the copied text contains none of those names (assert on each fixture name).
- `i18n-complete` and the parts test stay green with the new keys.

## 6. Live proof (19998)

Build `target-test`, start the 19998 instance with `scripts/test-instance.ps1`, open Settings →
System in English and Chinese. Screenshot the About section. Click Check for updates once and
screenshot the outcome (with a real network this reads the real GitHub latest; that is allowed —
it is a read). Paste the copied diagnostics into the report so the reviewer can check it for
names.

## 7. SPEC and docs in the same commit

- SPEC §panel.settings (`docs/spec/0014-panel/02-pages.md`, the System page): an About section
  first, its three rows, "checked only on click", the diagnostics content rule.
- SPEC §host.cli / `update`: the panel's check shares the CLI's source; the route and its codes.
- SPEC wherever `/api/info`'s `build` object is described (§host.daemon or §host.routes — grep
  `panelVersion`): the two new fields.
- README Quick start's "Settings → System shows …" sentence gains "the version and build".
- CHANGELOG `[Unreleased]`: one Added entry.

## 8. Delivery

Two commits are fine: `feat(host): build info carries version and target; /api/update/check`
and `feat(panel): Settings shows the version, checks for updates, copies diagnostics` (with the
emit). Or one; not more.
