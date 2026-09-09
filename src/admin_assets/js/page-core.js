/* Pure page registration: descriptors are data, loaders are lazy, and failures are retryable. */
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
    list: function () { return Array.from(entries.values()).sort(function (a, b) { return a.order - b.order; }); },
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

export { createPageRegistry };
