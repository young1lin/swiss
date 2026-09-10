/* A classic-script loader for the vendored UMD bundles (docs/14 §2, constraint 2).
   The panel is native ES modules with no bundler, and the UMD builds assign their
   exports onto the GLOBAL object the way classic scripts do — @xterm/addon-unicode11
   even binds to top-level "this", which is undefined inside a module, so the bundles
   cannot simply be import()ed. One <script> per URL, requested in order, resolved
   once; a failed load is evicted so a retry can try again. */

var inflight = new Map();   // href -> Promise (settled loads stay, as a once-only guard)
var settled = new Set();    // hrefs that finished, so repeat wants resolve immediately

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
