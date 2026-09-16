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

// No setup file on purpose: the old Node-repo setup pinned MCP_GATEWAY_MASTER_KEY/HOME for the
// server suite — panel tests need neither. Anything a DOM-stub test needs, it stubs itself.
export default defineConfig({
  test: { environment: "node", include: ["test/**/*.test.ts"] },
});
