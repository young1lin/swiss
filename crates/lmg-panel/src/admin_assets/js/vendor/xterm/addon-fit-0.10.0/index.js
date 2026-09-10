/* Vendored from the npm package whose tarball held lib/addon-fit.js — byte-identical, no
   further minification, no patches (docs/14 §2). The UMD build assigns window.FitAddon
   as a classic script; this shim loads it once and hands the class back as a named
   export. Upgrading = a new versioned directory plus a changed import path; this
   directory dies in the same commit. */
import { loadClassic } from "../load-classic.js";

export async function loadFitAddon() {
  await loadClassic(new URL("./addon-fit.js", import.meta.url));
  if (typeof window.FitAddon !== "function") {
    throw new Error("addon-fit.js loaded but window.FitAddon is missing");
  }
  return window.FitAddon;
}
