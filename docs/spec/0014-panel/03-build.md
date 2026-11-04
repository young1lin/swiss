# Languages and the build

Part 03 of [§panel](README.md), chapter 0014 of the [swiss specification](../../SPEC.md).

## §panel.i18n — Languages

English and Simplified Chinese. The scope is the **panel's own copy and the gateway's own
refusals**. A string the server relays is data, shown as sent inside a translated frame
(`{name}` placeholders): an engine error, a provider's message, a row. Identifiers (names,
keys, SQL, paths) are never translated.

**Gateway refusals carry a code.** A refusal the gateway itself writes and the panel shows
(a stale console revision, a limit) answers `{error, code, params?}` (`admin_error_code` in
swiss-host `reply.rs`):
- `code` is a stable dotted id (`console.changed`) that never changes once shipped;
- `error` is the English sentence the CLI, the logs and an older panel read;
- `params` are the values the sentence names.

`servedError(body, fallback)` (i18n.ts) turns the code into a `served.*` key through its
`SERVED_CODES` table and fills it from `params`. A code the build does not know yet shows
`error` as sent, and a body without one shows the caller's fallback. A new refusal adds its
code on the server and its key in both locales in the same change.

The codes so far are the console store's: `console.exists`, `console.removed`,
`console.changed`, `console.changedDelete` and `console.tooLarge` (SPEC §data.agent Tabs).
Two English sentences predate codes and are mapped by their text: `wireLabel` for a served
nav label, and `moduleDbMessage` for a module-database refusal.

- **Keys are symbolic**, `<module>.<semanticId>` (`addSheet.addMcp`), in `locales/en.ts` and
  `locales/zh.ts` (TS sources, about 1,800 entries each). English is a static import and the
  floor; Chinese is a lazy import. The switch to symbolic keys was one codemod
  (`panel/scripts/migrate-i18n-keys.mjs`, kept as the record; it only runs on the pre-migration
  tree).
- `tr(key, vars)` looks up the active dictionary, then English, then answers the key itself.
  `trn(n, oneKey, otherKey, vars)` picks through `Intl.PluralRules` of the installed locale
  (Chinese has only `other`). `tk(key)` is an identity marker for module-level tables: module
  top level never calls `tr`, because the language is not loaded yet. `wireLabel(served)` maps
  a gateway-served English label (a plugin or page name) to a `wire.*` key.
- `locale()` answers `en` or `zh-CN` and is what every `toLocale*` / `Intl` call receives.
- **The preference** is `swiss_lang`; absent is English. `navigator.language` is not
  consulted. `<html lang>` follows it. A top-level `await loadLocale()` runs before the first
  paint.
- **The flip** (文/A in the context bar) is `setLang` → repaint the chrome → re-navigate the
  current page, without a reload. A page whose `canLeave()` refuses blocks it, and then the
  preference is not written. Blocked storage keeps the choice in memory; a failed Chinese load
  falls back to English. A view that keeps nodes across repaints redraws them in the new words.
  Before 2026-11-04 the Data agent's column kept the old language's labels (System prompt, its
  work line, Sent with) after the rest of the page flipped. Now it reads its conversation again
  and redraws it, leaving a reply that is still streaming until it ends (`a language flip
  redraws the restored column…`).
- **No context mechanism**: a string that needs different words in two places gets two keys.
- **Chinese style**: full-width punctuation after Han characters.

**The gates** (vitest, inside `npm run check`):

| Test | Fails when |
|---|---|
| `i18n-complete` | a `tr`/`tk` key, or a `trn` other-key, found by walking the AST has no entry — or an entry has no user |
| `i18n-ratchet` | a bare English literal sits in a visible position: an `h()` child (except in `code`, `kbd`, `pre`), a `title` / `placeholder` / `aria-label` prop, the first argument of `toast` / `confirm` / `prompt` / `say`, an `emptyNode` title / hint / action. Zero allowed. It is an AST test rather than an eslint rule because `no-restricted-syntax` is held by the `innerHTML` rule. Known gap: it does not scan function return values |
| `i18n-zh-style` | a Chinese value has a half-width `,` after a Han character |
| `i18n-toggle`, `i18n-shell-zh`, `i18n-load-fail`, `i18n` | The flip, the Chinese shell, the English fallback, the helpers |

## §panel.toolchain — TypeScript, the build and the emit

The panel is written in TypeScript and served as the JavaScript it erases to (ADR-024).

- **Home**: `crates/swiss-panel/panel/` — `package.json`, `build.mjs`, two tsconfigs,
  `eslint.config.js`, `src/`, `test/`, `scripts/`. Node ≥ 24.
- **The emit is type erasure, line for line.** `build.mjs` runs `ts-blank-space` over each
  `src/**/*.ts` into `crates/swiss-panel/src/admin_assets/js/`: types become spaces, so a line
  and column in the browser are the line and column in the source. No bundle, no minify, no
  source map, no rewriting of imports. It writes only changed files, skips `.d.ts`, never
  deletes and never touches `js/vendor/`.
- **The emit is committed** and marked `linguist-generated`, so the binary builds with no Node
  on the machine. It is never edited by hand. `panel-emit.test` re-emits in memory and fails
  on a stale file or an orphan.
- **tsconfig**: `strict`, `verbatimModuleSyntax`, `erasableSyntaxOnly` (nothing that needs a
  transform: no enums, no namespaces, no parameter properties), `isolatedModules`, target
  es2022, module esnext, resolution bundler, lib es2022 + dom + dom.iterable. Two projects: the
  source (no Node types) and the tests.
- **Imports** use the emitted `./x.js` specifier; types come in through `import type`.
- **A behaviour change is named and tested.** A commit that changes how the panel behaves (not
  only how it is written) says so in its message and carries the vitest case that pins it;
  deleting a test to pass a gate is never allowed.
- **Types.** The `any` budget is zero. API types are copied from the Rust serde structs field
  by field (`types/api.d.ts`), index signatures included; a field the panel reads that Rust
  does not send is a bug. The `.d.ts` files export their types explicitly. No augmentation of
  built-in types. Event handlers use `e.currentTarget`, never `this`; an error is shown through
  `errText(e)`.
- **DOM building.** `h()` / `frag()` / `fill()` build nodes; nothing writes `innerHTML`
  (§panel.lint), and there are no inline `on…` attributes. The one HTML string the panel reads
  is a vendored library's own output (markdown-it's, mermaid's sandbox iframe): `ui/markdown.ts`
  parses it with `DOMParser` into an inert document and moves the nodes over. A view wires one delegated listener
  and dispatches on `data-act`. **A delegated handler resolves its record, key and buffer from
  live state at event time**, never from what the render captured: a menu built at click time
  must still act on the right object after a tab switch or a reload under it.
- **State** is six slices — `db-state`, `job-state`, `mcp-state`, `traffic`, `tunnel-state`,
  `ui-state` — each owning its data and its setters; `util.ts` exports no `state`.
- **Scripts.** `build` (`node build.mjs`), `build:check` (fail if the committed emit is stale),
  `dev` (`--watch`), `typecheck` (both projects), `lint` (`eslint .`), `test` (`vitest run`),
  and **`check` = typecheck && lint && build:check && test** — the panel's one gate.
- **Dev tools**: `typescript` ~6.0, `ts-blank-space` ~0.9, `eslint` 10 with
  `typescript-eslint` 8, `vitest` 5 on `happy-dom`, `vite` 8 (vitest's), `@types/node` 24.
  Dev-only: nothing from npm reaches the browser.
- **The dev loop**: `npm run dev` beside a debug gateway — save the `.ts`, the emit updates,
  reload the page (debug builds read assets from disk).
- **Gates.** `scripts/deploy.ps1` runs `npm run check` after clippy, so a deploy can never
  serve a panel whose committed emit predates its sources; CI has a panel job (Ubuntu, Node 24, `npm ci`, `npm run check`). Vitest runs
  capped (`--maxWorkers=4`).

## §panel.lint — The lint ratchet

`eslint.config.js`, type-aware over both tsconfigs. No prettier: formatting is by hand, like
the Rust tree. No whole-file disable; a single-line disable carries its reason.

| Scope | Rule | Level |
|---|---|---|
| src | `no-explicit-any` | error |
| src | `no-floating-promises` | error |
| src | `no-var`, `prefer-const` | error |
| src | `consistent-type-imports` (separate type imports) | error |
| src | `no-unused-vars` (`^_` arguments ignored, caught errors not checked) | error |
| src | `no-non-null-assertion` | error, plus a per-file warn allowlist frozen by `test/non-null-ratchet.test.ts` |
| src | `no-restricted-imports`: `state` from `util.js` | error |
| src | `no-restricted-syntax`: any `.innerHTML =` or `+=` | error, no allowlist |
| src | `no-unnecessary-condition` | warn — a burn-down (496 sites on 2026-09-20) that flips to error at zero |
| `src/types/**/*.d.ts` | `no-restricted-syntax`: an interface named after a built-in (`Function`, `EventTarget`, `RegExp`, `Window`, `Element`, `Array`, `String`, `Number`, `Boolean`, `Object`, `Promise`) — narrow at the call site instead | error |
| test | `no-explicit-any`, `no-floating-promises` | warn |

The non-null allowlist and every other frozen count follow the ratchet convention
(§panel.ui): only down, in the same commit.

## §panel.vendor — Vendored browser libraries

A browser library the panel needs is vendored, never an npm runtime dependency and never a
CDN:

| Package | Version | Used by |
|---|---|---|
| `@xterm/xterm` + addons fit 0.10.0, search 0.16.0, unicode11 0.8.0, web-links 0.11.0, webgl 0.18.0 | 5.5.0 | Terminal (§terminal.panel) |
| `cronstrue` | 2.52.0 | Jobs: the spoken schedule, loaded on first use |
| `shlex` | 3.0.0 | Data: splitting a Redis console line (§data.redis-console) |
| `cytoscape` | 3.34.3 | Memory: the project graph and a note's neighbourhood (§memory.panel), loaded on first use |
| `markdown-it` | 15.0.2 | Every Markdown the panel draws (`ui/markdown.ts`): notes, hubs, briefs, previews |
| `@mermaid-js/tiny` (mermaid's tiny build) | 12.1.0 | A ```` ```mermaid ```` fence in that Markdown, drawn as its diagram; loaded on first use |

- Files live in `src/admin_assets/js/vendor/<pkg>/<version>/` beside the package's `LICENSE`,
  byte-identical to the npm dist files — no patches. xterm adds `load-classic.js` and one
  `index.js` shim per package; the cytoscape and mermaid shims reuse that loader. An upstream
  that ships a self-contained ES module (shlex, markdown-it's `dist/browser/*.esm.min.mjs`) is
  re-exported as it is; the server types `.mjs` as JavaScript. The xterm addon-search, cytoscape,
  markdown-it and mermaid bundles are pinned by SHA-256 in the panel tests.
- A vendored library is MIT, BSD, ISC, Apache-2.0 or MPL-2.0, as `deny.toml` holds the crates, and
  so is everything it bundles. That is why mermaid is its tiny build: the full `mermaid.min.js`
  bundles the Eclipse ELK layout engine (EPL-2.0). Tiny lacks mindmap and architecture diagrams
  and KaTeX; such a fence stays as code, the reason in its title. `THIRD_PARTY_NOTICES.md` lists
  each vendored library and what it bundles.
- The types are a hand-written mirror in `panel/src/vendor/<pkg>/<version>/index.d.ts` that
  declares only the exports the panel uses.
- An upgrade is one commit: the new directory, the import change, the old directory deleted.
