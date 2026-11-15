# 01 — Split the locale tables by area

Implementation brief, not SPEC. The SPEC and AGENTS.md sentences named in §6 change in the same
commit that makes them true. Shared rules: [00-handoff.md](00-handoff.md).

## 1. Why

`crates/swiss-panel/panel/src/locales/en.ts` is 4357 lines and `zh.ts` 4254 — past the
4000-line line the owner splits files at. Every panel change edits both, so they are the
merge-conflict magnet for every other brief in this round. This brief goes first and is a
**pure move**: no key renamed, no value changed, no caller edited.

## 2. Facts the design rests on (verified 2026-11-12)

- The tables are flat `Record<string, string>` objects, `export default`. 4246 keys in `en`.
- `i18n.ts:39` imports `./locales/en.js` statically (always loaded); `i18n.ts:256` and
  `ui-gallery.ts:90` import `./locales/zh.js` lazily. Tests import both paths directly, and
  `test/i18n-load-fail.test.ts` does `vi.mock("../src/locales/zh.js", …)`.
- 64 comment lines sit **inside** the `en` table, between keys.
- `test/i18n-complete.test.ts:97` and `test/ui-class-ratchet.test.ts:69` skip a directory named
  `locales` while walking `src/`, so part files under `locales/en/` stay out of those walks.
  `test/i18n-ratchet.test.ts` walks every `.ts` file — confirm the part files add no ratchet
  hits (they are plain object literals, exactly like today's tables).
- `build.mjs` walks `src/` recursively and the Rust side embeds `src/admin_assets` as a folder
  (rust-embed), so new files are emitted and served with no list to update.
- `scripts/migrate-i18n-keys.mjs` is a finished one-off migration that **rewrites `en.ts` and
  `zh.ts` wholesale** (lines 233-237). Run after the split, it would destroy the barrels.
- Key counts by top-level prefix (the text before the first dot): memory 817, llm 543,
  gallery 380, dataMongo 254, process 145, jobs 120, logs 96, runHistory 93, analysis 92,
  remoteRuns 87, then a long tail (ui 86, dataPubsub 86, dataGrid 86, dataView 80, …).

## 3. Layout

```
src/locales/en.ts            barrel: imports every part, spreads them, `export default` (same type as today)
src/locales/en/<part>.ts     `const part: Record<string, string> = { … }; export default part;`
src/locales/zh.ts            barrel, same shape
src/locales/zh/<part>.ts     the same part names as en/
```

The import paths `./locales/en.js` and `./locales/zh.js` do not change, so `i18n.ts`,
`ui-gallery.ts`, every test and the `vi.mock` keep working untouched. `zh` stays lazy: the barrel
is the one dynamic import, and its static imports load the parts as one group.

## 4. Part assignment

By top-level prefix. A prefix lives in exactly one part.

| Part | Prefixes |
| --- | --- |
| `memory` | `memory` |
| `llm` | `llm` |
| `gallery` | `gallery` |
| `data-mongo` | `dataMongo` |
| `data` | every other prefix matching `/^data([A-Z]|$)/`, plus `analysis` |
| `ops` | `process`, `jobs`, `logs`, `runHistory`, `remoteRuns`, `remote`, `tunnels`, `tunnelSheets`, `terminal`, `terminalSettings`, `traffic` |
| `shell` | every prefix not listed above (ui, fields, detail, pane, plugins, system, secrets, tokens, pageRegistry, polling, wire, desktop, served, …) |

If a part comes out above ~1000 lines, split it further along the same rule and say so in the
report; do not invent a different rule.

Inside a part, keys keep their original relative order (not sorted), and each comment line
moves with the key that follows it, so a reviewer can diff the moved text.

Do the move with a throwaway script (scratchpad, not committed) that reads the pre-split
tables, assigns keys by the table above, and writes the part files plus barrels. Before
writing, dump `JSON.stringify` of each pre-split table (import the emitted
`src/admin_assets/js/locales/*.js`); after `npm run build`, dump the barrels again and require
deep equality. Report both key counts.

## 5. Tests

New `test/i18n-parts.test.ts`, which owns the assignment table above as its single source:

1. `en/` and `zh/` contain the same part file names.
2. Within each locale, no key appears in two parts (a spread silently overwrites duplicates).
3. Every key's prefix belongs to the part it is in, per the table (`shell` takes the rest).
4. Per part, the `zh` key set equals the `en` key set except the existing `.one` exemption
   (mirror `i18n-complete.test.ts`'s `exempt`). This is stricter than today's global parity: a
   key added to `en/ops.ts` and `zh/shell.ts` fails.
5. Each barrel's key count equals the sum of its parts' key counts.

All existing i18n tests stay green unchanged.

`scripts/migrate-i18n-keys.mjs`: add a guard at the top that exits 1 with "historical
migration: the locale tables are split into locales/en/ and locales/zh/" when
`src/locales/en` is a directory. Do not delete the script (it records how keys were made).

## 6. Text that changes in the same commit

- AGENTS.md, "The panel" / "Making changes": "English lives in `panel/src/locales/en.ts` and
  every other locale (currently `zh.ts`) carries the same keys" → English lives in
  `panel/src/locales/en/*.ts` (one file per area, `en.ts` is the barrel), `zh/` carries the
  same files and keys.
- SPEC §panel.i18n (chapter 0014): the same correction, plus one sentence on the part rule
  and the test that enforces it.
- `i18n.ts` header comments near lines 18 and 149 that name `locales/en.ts`.

## 7. Delivery

One commit: `refactor(panel): the locale tables split by area; en.ts and zh.ts are barrels`,
with the emitted `admin_assets/js/locales/**` files. No CHANGELOG entry (nothing user-visible).

Gate: `npm run check` alone in `crates/swiss-panel/panel` (nothing else running). No cargo run
is needed beyond what the panel gate implies, but the embedded assets changed, so do one
`cargo build -j 2 --profile quick` to prove the folder embed picks the new files up, and say so.

Live (19998): open the panel in English and in Chinese and visit Memory, LLM, Data, Process,
Terminal and Settings; no raw `module.key` text appears, and the network list shows no 404 for
any `locales/` file. One screenshot per language.
