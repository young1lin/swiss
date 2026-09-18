/*
 * Copyright 2026 The swiss authors
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
 *   R5  no-restricted-properties: innerHTML (allowlist: static skeletons)
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
  rules: {
    /* R0: docs/37 D10 - the any budget is zero; the regex guard in panel-no-any.test.ts
     * retires in favor of this. */
    "@typescript-eslint/no-explicit-any": "error",
    /* R0 (warn until R3): the panel has a large share of un-awaited apiJson() calls;
     * the warnings inventory is the R3 worklist. */
    "@typescript-eslint/no-floating-promises": "warn",
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
);
