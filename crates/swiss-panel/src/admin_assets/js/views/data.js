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

import { $ } from "../util.js";
import { dbConnLabel, dbOkToDrop, dbPending, loadDbView } from "../data-view.js";
import { dbConn, unmountDbView } from "../db-state.js";
export function mount() { return loadDbView(); }
export function refresh() { return loadDbView(); }
export function hasPendingChanges() { return dbPending() > 0; }
export function canLeave() { return !dbPending() || dbOkToDrop(); }
export function unmount() {
  unmountDbView();
  // Take the workspace framing back off (data-view.js's renderDbView adds it) so the next
  // page — whatever it is — starts from the pane's ordinary padding.
  const pane = $("pane");
  if (pane) pane.classList.remove("db-host");
}
/* The context bar's count chip. It used to return "Data", which the location label one slot
   left already says — the bar read "Data … Data". It names the connection being browsed instead,
   in the dropdown's own words (dbConnLabel, one builder for both), and renders nothing when
   no connection is selected rather than repeating the page name. */
export function countText() {
  const d = dbConn();
  if (!d.conn) return "";
  const c = d.conns.find((x) => { return x.name === d.conn; });
  return c ? dbConnLabel(c) : "";
}
