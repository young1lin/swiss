/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

/* The writing-style gate that replaced docs/36 D9 (docs/37 M10): D9 held the emitted bytes
 * to "identical to the pre-migration JS", which forced the type system to bend around
 * untouched 2015-era code. From docs/37 on, the STYLE itself is what the machine checks,
 * and this ratchet is where each stage's rules land:

 *   R0  no-explicit-any (error)              the any budget, retires panel-no-any's regex
 *       no-floating-promises (warn)          until R3 turns it into an error
 *   R1  no-restricted-syntax: no built-in augmentations in types/** (Function, EventTarget,
 *       RegExp, Window, Element, Array, String)
 *   R2  no-var, prefer-const; no-non-null-assertion -> error with a per-file allowlist
 *   R3  consistent-type-imports, no-unnecessary-condition; floating-promises -> error
 *   R4  no-restricted-imports: no importing the mutable state out of util.js
 *   R5  no-restricted-syntax: no innerHTML WRITES (landed with zero survivors - no allowlist)
 *
 * Only rules the tree already passes are on; each stage flips its row in one commit so the
 * ratchet only ever tightens. No prettier: this house hand-aligns its comments and the
 * formatter would destroy that (docs/37 M10).
 */

import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    files: ["src/**/*.ts", "test/**/*.ts"],
    languageOptions: {
    parser: tseslint.parser,
    parserOptions: {
      /* Type-aware linting across both projects: src/ is covered by tsconfig.json (no node
       * types - browser code), test/ only by tsconfig.test.json. The projectService option
       * cannot express this: it auto-discovers plain tsconfig.json files only and leaves the
       * suite "not found by the project service", so the classic project list it is.
       * build.mjs and this config are in neither and outside the files globs on purpose. */
      project: ["./tsconfig.json", "./tsconfig.test.json"],
      tsconfigRootDir: import.meta.dirname,
    },
  },
  plugins: { "@typescript-eslint": tseslint.plugin },
  },
  {
    /* The src rule set proper (docs/37 R0-R2). The suite gets its own block below: R2's
     * mechanical sweep scoped itself to the emitted tree, and the suite's idioms are a
     * separate budget. */
    files: ["src/**/*.ts"],
    rules: {
      /* R0: docs/37 D10 - the any budget is zero; the regex guard in panel-no-any.test.ts
       * retires in favor of this. */
      "@typescript-eslint/no-explicit-any": "error",
      /* R3: every fire-and-forget now carries a visible void (the handler meant it),
       * and every awaited path awaits. The warn inventory closed with M8/M9. */
      "@typescript-eslint/no-floating-promises": "error",
      /* R2: block scope and const-first everywhere; the var/function-expression era ended
       * with docs/37. The four survivors are exempt by position, not by rule: the fetch
       * wrapper needs this+arguments, and three self-removing document listeners keep
       * their names (no-var cannot see them - they are function expressions). */
      "no-var": "error",
      "prefer-const": "error",
      /* R3 (warn until the inventory is gone): dead branches surfaced with M9's
       * reconciliation - 550 sites, each a micro-audit (redundant guard vs a type that
       * over-promises a wire field). The original plan flipped this to error with R4's
       * state split; R4 landed and 496 sites survive (2026-09-20 count) - the split
       * touched the state bag, not the wire-field over-promises. The warning inventory
       * IS the burn-down list; the rule flips to error the day it reaches zero. */
      "@typescript-eslint/no-unnecessary-condition": "warn",
      /* R3: type-only imports carry their own keyword - the 92 import type lines M8
       * wrote are the seed; this keeps every later type import honest. */
      "@typescript-eslint/consistent-type-imports": ["error", { prefer: "type-imports", fixStyle: "separate-type-imports" }],
      /* R2: the assertion budget. 1,193 sites came into the migration; 326 survive (121 of
       * them are a function's single boundary assertion on the Data pane state - the honest
       * shape until R4 splits PanelState). The files below still carry survivors, so they
       * warn; the exact per-file counts are frozen by test/non-null-ratchet.test.ts, which
       * fails on any growth. Clean files get the full error. */
      "@typescript-eslint/no-non-null-assertion": "error",
      /* R4, not a row on the docs/37 §9 table - added because the state slices kept leaving
       * dead `state` imports behind and nothing caught them. This config lists its rules one
       * by one instead of spreading a preset, so no-unused-vars was simply absent; switching
       * it on found 21 sites that predate R4 entirely. The ratchet only tightens, so it goes
       * in as an error with the sites cleared, not as a warning inventory.
       * caughtErrors is off: the deliberate `catch (e) { /* comment *\/ }` swallows are the
       * house idiom for "this failure is the expected path", and the comment IS the handling. */
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_", caughtErrors: "none" }],
      /* R4's row, landed with the last slice (docs/37 M11). util.js no longer exports a
       * `state` — the bag it held is seven domain modules now — so this cannot fire today.
       * That is the point: it is what stops the 35-field bag growing back one convenient
       * field at a time. util.js keeps its helpers ($, el, api, esc, toast…); what it may
       * never hand out again is shared mutable state. */
      "no-restricted-imports": ["error", {
        paths: [{
          name: "./util.js",
          importNames: ["state"],
          message: "state lives in its domain slice (mcp-state / ui-state / traffic / tunnel-state / job-state / db-state), not in util.js — docs/37 R4.",
        }, {
          name: "../util.js",
          importNames: ["state"],
          message: "state lives in its domain slice (mcp-state / ui-state / traffic / tunnel-state / job-state / db-state), not in util.js — docs/37 R4.",
        }],
      }],
      /* R5 (docs/37 section 9): no innerHTML WRITES anywhere in src - the string builders
       * retired with the last of them, so the machine gate needs no allowlist. The selector
       * matches ANY `.innerHTML =` / `.innerHTML +=` (no-restricted-properties would only
       * see variables named element); reads stay legal. */
      "no-restricted-syntax": ["error", {
        selector: "AssignmentExpression[left.type='MemberExpression'][left.property.name='innerHTML'], UpdateExpression[argument.type='MemberExpression'][argument.property.name='innerHTML']",
        message: "innerHTML writes are retired (docs/37 R5) - build with h()/frag() and mount with fill(), or iconNode()/emptyNode() for the shared shapes.",
      }],
    },
  },
  {
    /* The suite is the acceptance spec, not shipped code: R2's mechanical sweep (var,
     * arrows, assertions) scoped itself to src/ - the emitted tree - and the suite keeps
     * its own idioms (var-free is coming with R3's consistent-type-imports pass). The
     * any budget stays visible as warnings, same as before R2 split the scopes. */
    files: ["test/**/*.ts"],
    rules: {
      "@typescript-eslint/no-explicit-any": "warn",
      "@typescript-eslint/no-floating-promises": "warn",
    },
  },
  {
    /* R2 assertion-budget carriers (docs/37 M5): the rule warns here instead of erroring
     * so the tree stays green while the counts burn down; test/non-null-ratchet.test.ts
     * freezes each file's exact count and fails on growth, so "warn" here is not a free
     * pass. A file that reaches zero loses its line here and its protection upgrades to
     * the error above. Files absent from this list get no survivors at all. */
    files: [
      "src/add-sheet.ts",
      "src/menu.ts",
      "src/sidebar.ts",
      "src/views/plugins.ts",
      "src/views/secrets.ts",
      "src/views/tokens.ts",
      "src/detail.ts",
      "src/main.ts",
      "src/page-core.ts",
      "src/page-registry.ts",
      "src/pane.ts",
      "src/run-history.ts",
      "src/tunnel-sheets.ts",
      "src/views/remote.ts",
      "src/views/terminal-settings.ts",
      "src/views/terminal.ts",
      "src/data-activity.ts",
      "src/data-browsers.ts",
      "src/data-ddl.ts",
      "src/data-form.ts",
      "src/data-structure.ts",
      "src/data-suggest.ts",
      "src/dropdown.ts",
      "src/groups.ts",
      "src/jobs.ts",
      "src/tunnels.ts",
      "src/data-cell.ts",
      "src/data-edit.ts",
      "src/data-grid.ts",
      "src/data-sql.ts",
      "src/data-view.ts",
      "src/data-csv.ts",
    ],
    rules: {
      "@typescript-eslint/no-non-null-assertion": "warn",
    },
  },
  {
    /* The suite mocks heavily and its anys are a separate budget from D10 (which scoped
     * src/): these warn so the inventory is visible while R1-R4 kill the shared shapes'
     * excuses, then this override flips to error. */
    files: ["test/**/*.ts"],
    rules: {
      "@typescript-eslint/no-explicit-any": "warn",
    },
  },
  {
    /* R1 (docs/37 M3): no augmenting built-in interfaces in the shape library. These five
     * names are the ones docs/36 D9 once forced in (Function for toast._t/_warned and
     * paintTabs.last, EventTarget for closest/tagName, RegExp for test(string | null),
     * Window for webkitAudioContext) - each bent the whole program's types around one
     * call site. Narrow at the call site (targetEl, casts) or keep module state instead. */
    files: ["src/types/**/*.d.ts"],
    rules: {
      /* One exact-match selector per name: esquery's regex literals choke on the parens
       * an alternation needs, and exact equality says precisely "this interface IS the
       * built-in" anyway. */
      "no-restricted-syntax": ["error",
        ...["Function", "EventTarget", "RegExp", "Window", "Element", "Array", "String", "Number", "Boolean", "Object", "Promise"].map((name) => ({
          selector: "TSInterfaceDeclaration[id.name='" + name + "']",
          message: "Global built-in augmentation (docs/37 M3): narrow at the call site or keep the state module-scoped.",
        })),
      ],
    },
  },
);
