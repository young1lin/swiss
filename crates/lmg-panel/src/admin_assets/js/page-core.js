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
function groupPages(pages, plugins, fallbackLabels) {
  var byId = new Map((plugins || []).map(function (p) { return [p.id, p]; }));
  var groups = new Map();
  pages.forEach(function (page) {
    var gid = page.pluginId || page.id;
    var group = groups.get(gid);
    if (!group) {
      var known = byId.get(gid);
      var label = (known && known.label) || (fallbackLabels && fallbackLabels[gid]) || page.label;
      group = { id: gid, label: label, order: page.order, pages: [] };
      groups.set(gid, group);
    }
    if (page.order < group.order) group.order = page.order;
    group.pages.push(page);
  });
  return Array.from(groups.values())
    .map(function (g) { g.pages.sort(function (a, b) { return a.order - b.order; }); return g; })
    .sort(function (a, b) { return a.order - b.order; });
}

function createPageRegistry(importer) {
  var entries = new Map();
  var modules = new Map();
  importer = importer || function (entry) { return import(entry); };
  function valid(page) {
    if (!page || !/^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(page.id || "")) throw new Error("Invalid page id");
    if (typeof page.label !== "string" || !page.label) throw new Error("Page label is required");
    var entryOk = /^\/admin\/(?:js\/views|plugins)\/[a-zA-Z0-9_./-]+\.js$/.test(page.entry || "") && (page.entry || "").indexOf("..") === -1;
    if (!entryOk) throw new Error("Page entry must be a local admin module");
    return Object.assign({}, page, { order: Number.isFinite(page.order) ? page.order : 0 });
  }
  function listSorted() { return Array.from(entries.values()).sort(function (a, b) { return a.order - b.order; }); }
  return {
    replace: function (pages) {
      if (!Array.isArray(pages)) throw new Error("Pages must be an array");
      var next = new Map();
      pages.forEach(function (page) {
        page = valid(page);
        if (next.has(page.id)) throw new Error("Duplicate page: " + page.id);
        next.set(page.id, page);
      });
      entries = next;
      modules.forEach(function (cached, id) {
        if (!entries.has(id) || entries.get(id).entry !== cached.entry) modules.delete(id);
      });
    },
    get: function (id) { return entries.get(id); },
    list: listSorted,
    groups: function (plugins, fallbackLabels) { return groupPages(listSorted(), plugins, fallbackLabels); },
    load: function (id) {
      var page = entries.get(id);
      if (!page) return Promise.reject(new Error("Unknown page: " + id));
      var cached = modules.get(id);
      if (cached) return cached.promise;
      var promise = Promise.resolve().then(function () { return importer(page.entry); }).catch(function (error) {
        if (modules.get(id) && modules.get(id).promise === promise) modules.delete(id);
        throw error;
      });
      modules.set(id, { entry: page.entry, promise: promise });
      return promise;
    },
  };
}

export { createPageRegistry, groupPages };
