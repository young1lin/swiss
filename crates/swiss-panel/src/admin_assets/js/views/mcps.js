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

import { KINDS, state } from "../util.js";
import { loadList, mcpChipText } from "../polling.js";
import { loadCalls, loadMeta, loadPage, pageState } from "../detail.js";
import { renderPane } from "../pane.js";
import { histClose } from "../run-history.js";
export async function mount() { await loadList(); renderPane(); }
export async function poll() { await loadList(); if (state.detail && state.detail.tab === "logs") await loadCalls(state.detail.name); }
export async function refresh() {
  await poll();
  var d = state.detail;
  if (!d) return;
  await loadMeta(d.name);
  if (KINDS.indexOf(d.tab) >= 0) { d[d.tab] = pageState(); await loadPage(d.name, d.tab); }
}
export function countText() { return mcpChipText(); }
export function unmount() { histClose(); }
