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

import { defineConfig } from "vitest/config";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

// No setup file on purpose: the old Node-repo setup pinned MCP_GATEWAY_MASTER_KEY/HOME for the
// server suite — panel tests need neither. Anything a DOM-stub test needs, it stubs itself.

/* The sources under panel/src import the vendored runtime ("./vendor/cronstrue/…" from
   jobs.ts, "../vendor/xterm/…" from views/terminal.ts) by the path the SERVED tree needs:
   emission writes them back beside js/vendor, where the browser resolves them. From the
   source tree that path does not exist, so this alias points vite at the real vendor files.
   tsc needs no alias: panel/src/vendor/<same-path>/index.d.ts mirrors the shapes (docs/36
   D7) and the specifier resolves there for type-checking. */
const vendorDir = resolve(fileURLToPath(new URL("../src/admin_assets/js/vendor", import.meta.url)));

export default defineConfig({
  resolve: {
    alias: [{ find: /(^|\/)\.\.?\/vendor\//, replacement: vendorDir + "/" }],
  },
  test: { environment: "node", include: ["test/**/*.test.ts"] },
});
