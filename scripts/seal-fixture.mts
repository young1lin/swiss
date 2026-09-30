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

/**
 * Regenerates the Node-sealed envelope fixture used by the Rust envelope tests.
 *
 * Runs the REFERENCE implementation's own `seal()` (../local-mcp-gateway/src/secure/envelope.ts)
 * under a fixed test key, so the Rust test proves it opens bytes the Node build produced — the
 * blocking Phase 0 check in SPEC §formats.sealed. CI never needs the Node repo afterwards: the output is
 * committed under tests/fixtures/.
 *
 *   cd ../local-mcp-gateway && npx tsx ../local-mcp-gateway-rust/scripts/seal-fixture.mts
 */
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { seal } from "../../local-mcp-gateway/src/secure/envelope.ts";

// Must stay in sync with FIXTURE_KEY in src/secure/envelope.rs. A test key, never a real one.
const TEST_MASTER_KEY = "3f7a9c2e58b1d406af83c2e97b1f6d0a4c8e2f5b7a9d1c06e3b8f4a2d5c7e91f";

// A realistic sealed-payload shape: pretty-printed JSON, exactly how statefile.ts writes it.
const payload = JSON.stringify(
  { port: 19999, host: "127.0.0.1", tokenEnv: "MCP_GATEWAY_TOKEN", mcps: [] },
  null,
  2,
);

const envelope = seal(Buffer.from(TEST_MASTER_KEY, "hex"), "env", payload);

const here = dirname(fileURLToPath(import.meta.url));
const outDir = resolve(here, "../tests/fixtures");
mkdirSync(outDir, { recursive: true });
// Written the way statefile.ts writes every state file: pretty-printed envelope.
writeFileSync(resolve(outDir, "sealed-node.json"), JSON.stringify(envelope, null, 2));
writeFileSync(resolve(outDir, "sealed-node.payload.txt"), payload);
console.log(`sealed ${payload.length} bytes of payload into tests/fixtures/sealed-node.json`);
