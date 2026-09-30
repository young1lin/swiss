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

import { isMcpKind } from "../util.js";
import { loadList, mcpChipText } from "../polling.js";
import { loadCalls, loadMeta, loadPage, pageState } from "../detail.js";
import { renderPane } from "../pane.js";
import { histClose } from "../run-history.js";
import { mcpDetail } from "../mcp-state.js";
export async function mount() { await loadList(); renderPane(); }
export async function poll() {
  await loadList();
  const d = mcpDetail();
  // SPEC §mcp.calls: only page 0 is live — an offset page is a reading position a poll must not
  // drift, and a switch in flight owns the tab until it commits.
  if (d && d.tab === "logs" && d.callsPage === 0 && d.callsPendingPage == null) await loadCalls(d.name, true);
}
export async function refresh() {
  await poll();
  const d = mcpDetail();
  if (!d) return;
  await loadMeta(d.name);
  if (isMcpKind(d.tab)) { d[d.tab] = pageState(); await loadPage(d.name, d.tab); }
}
export function countText() { return mcpChipText(); }
export function unmount() { histClose(); }
