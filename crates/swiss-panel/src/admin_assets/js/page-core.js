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

/* Pure page registration: descriptors are data, loaders are lazy, and failures are retryable. */

/* Group page descriptors by the plugin that contributed them. Pure data in, pure data out:
   the shell renders the result, so this stays unit-testable without a DOM.
   - group order is the SMALLEST page order in the group, so a plugin cannot jump the row by
     contributing one late page, and no groupOrder field has to exist;
   - inside a group, page order decides, and replace() order breaks ties;
   - a page whose pluginId has no inventory row (the client-side "plugins" page, pluginId
     "host") still gets a group: fallback label first, then the page's own label. Never drop
     a page because its plugin row is missing - a page that cannot be reached is worse than
     a group with an ugly name. */
                                                   
                                                                                       
import { tr } from "./i18n.js";
function groupPages(pages                  , plugins                                   , fallbackLabels                                           )              {
  const byId = new Map((plugins || []).map((p) => { return [p.id, p]; }));
  const groups = new Map                   ();
  pages.forEach((page) => {
    const gid = page.pluginId || page.id;
    let group = groups.get(gid);
    if (!group) {
      const known = byId.get(gid);
      const label = (known && known.label) || (fallbackLabels && fallbackLabels[gid]) || page.label;
      group = { id: gid, label: label, order: page.order, pages: [] };
      groups.set(gid, group);
    }
    if (page.order < group.order) group.order = page.order;
    group.pages.push(page);
  });
  return Array.from(groups.values())
    .map((g) => { g.pages.sort((a, b) => { return a.order - b.order; }); return g; })
    .sort((a, b) => { return a.order - b.order; });
}

function createPageRegistry(importer                                         ) {
  let entries = new Map                        ();
  const modules = new Map                                                         ();
  importer = importer || ((entry        ) => { return import(entry); });
  function valid(page           )                 {
    if (!page || !/^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(page.id || "")) throw new Error(tr("Invalid page id"));
    if (typeof page.label !== "string" || !page.label) throw new Error(tr("Page label is required"));
    const entryOk = /^\/admin\/(?:js\/views|plugins)\/[a-zA-Z0-9_./-]+\.js$/.test(page.entry || "") && !(page.entry || "").includes("..");
    if (!entryOk) throw new Error(tr("Page entry must be a local admin module"));
    return Object.assign({}, page, { order: Number.isFinite(page.order) ? page.order : 0 })                  ;
  }
  function listSorted()                   { return Array.from(entries.values()).sort((a, b) => { return a.order - b.order; }); }
  return {
    replace: (pages             ) => {
      if (!Array.isArray(pages)) throw new Error(tr("Pages must be an array"));
      const next = new Map();
      pages.forEach((page) => {
        page = valid(page);
        if (next.has(page.id)) throw new Error(tr("Duplicate page: {id}", { id: String(page.id) }));
        next.set(page.id, page);
      });
      entries = next;
      modules.forEach((cached, id) => {
        if (!entries.has(id) || entries.get(id) .entry !== cached.entry) modules.delete(id);
      });
    },
    get: (id        ) => { return entries.get(id); },
    list: listSorted,
    groups: (plugins                                   , fallbackLabels                         ) => { return groupPages(listSorted(), plugins, fallbackLabels); },
    load: (id        ) => {
      const page = entries.get(id);
      if (!page) return Promise.reject(new Error(tr("Unknown page: {id}", { id })));
      const cached = modules.get(id);
      if (cached) return cached.promise;
      const promise = Promise.resolve().then(() => { return importer(page?.entry); }).catch((error) => {
        if (modules.get(id) && modules.get(id) .promise === promise) modules.delete(id);
        throw error;
      });
      modules.set(id, { entry: page.entry, promise: promise });
      return promise;
    },
  };
}

export { createPageRegistry, groupPages };