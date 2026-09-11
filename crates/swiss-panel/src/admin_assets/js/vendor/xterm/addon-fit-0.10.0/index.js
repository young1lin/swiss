/* Vendored from the npm package whose tarball held lib/addon-fit.js — byte-identical, no
   further minification, no patches (docs/14 §2). As a classic script the UMD assigns
   window.FitAddon = { FitAddon: class } — a NAMESPACE, the class one level down; this
   shim loads it once, unwraps the class, and hands it back as a named export. Upgrading
   = a new versioned directory plus a changed import path; this directory dies in the
   same commit. */
import { loadClassic, unwrapGlobal } from "../load-classic.js";

export async function loadFitAddon() {
  await loadClassic(new URL("./addon-fit.js", import.meta.url));
  var cls = unwrapGlobal(window.FitAddon, "FitAddon");
  if (!cls) {
    throw new Error("addon-fit.js loaded but window.FitAddon carries no FitAddon class");
  }
  return cls;
}