/* Vendored from the npm package whose tarball held lib/addon-webgl.js — byte-identical, no
   further minification, no patches (docs/14 §2). The UMD build assigns window.WebglAddon
   as a classic script; this shim loads it once and hands the class back as a named
   export. Upgrading = a new versioned directory plus a changed import path; this
   directory dies in the same commit. */
import { loadClassic, unwrapGlobal } from "../load-classic.js";

export async function loadWebglAddon() {
  await loadClassic(new URL("./addon-webgl.js", import.meta.url));
  var cls = unwrapGlobal(window.WebglAddon, "WebglAddon");
  if (!cls) {
    throw new Error("addon-webgl.js loaded but window.WebglAddon carries no WebglAddon class");
  }
  return cls;
}