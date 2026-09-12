/* Vendored from the npm package whose tarball held lib/addon-search.js — byte-identical, no
   further minification, no patches (docs/14 §2). As a classic script the UMD assigns
   window.SearchAddon = { SearchAddon: class } — a NAMESPACE, the class one level down; this
   shim loads it once (lazily — only when the find bar is first asked for, an idle terminal
   never pays for it), unwraps the class, and hands it back as a named export. Upgrading =
   a new versioned directory plus a changed import path; this directory dies in the same
   commit. Pure JS — no wasm anywhere in it (checked at vendoring time). */
import { loadClassic, unwrapGlobal } from "../load-classic.js";

export async function loadSearchAddon() {
  await loadClassic(new URL("./addon-search.js", import.meta.url));
  var cls = unwrapGlobal(window.SearchAddon, "SearchAddon");
  if (!cls) {
    throw new Error("addon-search.js loaded but window.SearchAddon carries no SearchAddon class");
  }
  return cls;
}
