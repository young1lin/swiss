/* A classic-script loader for the vendored UMD bundles (docs/14 §2, constraint 2).
   The panel is native ES modules with no bundler, and the UMD builds assign their
   exports onto the GLOBAL object the way classic scripts do — @xterm/addon-unicode11
   even binds to top-level "this", which is undefined inside a module, so the bundles
   cannot simply be import()ed. One <script> per URL, requested in order, resolved
   once; a failed load is evicted so a retry can try again. */

var inflight = new Map();   // href -> Promise (settled loads stay, as a once-only guard)
var settled = new Set();    // hrefs that finished, so repeat wants resolve immediately

/* The vendored addon UMD bundles assign a NAMESPACE object onto the global they are
   handed (window.FitAddon = { FitAddon: class, __esModule: true }) - the class lives one
   level down. The core bundle instead copies each export straight onto the global, so
   window.Terminal already IS the class. unwrapGlobal accepts either shape, either way
   round, so a future xterm that changes convention needs a one-line shim change, not a
   rewrite. Returns null when neither shape is present - the shims turn that into their
   "loaded but missing" error, which is what a 404-in-disguise looks like here. */
export function unwrapGlobal(ns, name) {
  if (ns && typeof ns[name] === "function") return ns[name];
  return typeof ns === "function" ? ns : null;
}

export function loadClassic(href) {
  var url = String(href);
  if (settled.has(url)) return Promise.resolve();
  if (!inflight.has(url)) {
    inflight.set(url, new Promise(function (resolve, reject) {
      var tag = document.createElement("script");
      tag.src = url;
      // async=false keeps evaluation order when several packages are wanted together;
      // the bundles are self-contained, but deterministic order costs nothing.
      tag.async = false;
      tag.onload = function () { settled.add(url); resolve(); };
      tag.onerror = function () {
        inflight.delete(url);   // a retry gets a fresh script tag, not a cached failure
        tag.remove();
        reject(new Error("could not load " + url));
      };
      document.head.appendChild(tag);
    }));
  }
  return inflight.get(url);
}
