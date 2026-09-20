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

/* ================================================================================================
   The last page visited, per plugin (docs/39 S4).

   A plugin's seat and its palette row reopen where the user left that plugin, not always on
   its first page: "I clicked away from A while it was on A2 — clicking A's seat should land
   on A2, not A1". Like the theme and the rail pins, this is a preference about this screen,
   not gateway state, so it lives in localStorage. Deep links (#traffic) and the hashchange
   path ignore it — they name their page outright. Leaf module on purpose: it imports
   nothing, so the shell, the palette and the suite all reach it without a DOM.
   ================================================================================================ */

/** localStorage key: a JSON Record<pluginId, pageId>. Keyed by the GROUP id, not
 *  page.pluginId — the descriptor field is optional on legacy manifests, while the group
 *  id always exists (page-core derives one when the field is absent). */
const LAST_PAGE_KEY = "swiss.lastPage";

/** The stored map, or {} when nothing usable is there — absent, garbage, or a storage that
 *  throws (private mode, blocked). Never raises: this memory is a convenience, never a
 *  dependency of the navigation itself. */
function loadLastPages(): Record<string, string> {
  try {
    const raw = localStorage.getItem(LAST_PAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const out: Record<string, string> = {};
    Object.keys(parsed as Record<string, unknown>).forEach((key) => {
      const value = (parsed as Record<string, unknown>)[key];
      if (typeof value === "string") out[key] = value;
    });
    return out;
  } catch (e) { return {}; /* unreadable storage is the empty memory */ }
}

/** Record one visit: merge into the stored map and write it back. Write errors are
 *  swallowed — a full or blocked store costs the seat its memory, never the click its
 *  navigation. */
function rememberLastPage(pluginId: string, pageId: string): void {
  try {
    const next = loadLastPages();
    next[pluginId] = pageId;
    localStorage.setItem(LAST_PAGE_KEY, JSON.stringify(next));
  } catch (e) { /* full or blocked — the seat falls back to the first page */ }
}

/** The page a plugin's seat opens: the last one visited when it is still a member of the
 *  group and usable, else the group's first page. Pure; the seat and the palette both call
 *  it, so the policy is written exactly once. A group with no pages targets "" — callers
 *  that can see such a group do not navigate on it. */
function targetPageFor(group: { id: string; pages: { id: string }[] }, last: Record<string, string>, usable: (id: string) => boolean): string {
  const remembered = last[group.id];
  if (remembered && group.pages.some((p) => { return p.id === remembered; }) && usable(remembered)) return remembered;
  return group.pages.length ? group.pages[0].id : "";
}

export { LAST_PAGE_KEY, loadLastPages, rememberLastPage, targetPageFor };
