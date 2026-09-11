/* Vendored from the npm package whose tarball held lib/addon-web-links.js — byte-identical, no
   further minification, no patches (docs/14 §2). The UMD build assigns window.WebLinksAddon
   as a classic script; this shim loads it once and hands the class back as a named
   export. Upgrading = a new versioned directory plus a changed import path; this
   directory dies in the same commit. */
import { loadClassic, unwrapGlobal } from "../load-classic.js";

export async function loadWebLinksAddon() {
  await loadClassic(new URL("./addon-web-links.js", import.meta.url));
  var cls = unwrapGlobal(window.WebLinksAddon, "WebLinksAddon");
  if (!cls) {
    throw new Error("addon-web-links.js loaded but window.WebLinksAddon carries no WebLinksAddon class");
  }
  return cls;
}