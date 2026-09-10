/* Vendored from the npm package whose tarball held lib/addon-unicode11.js — byte-identical, no
   further minification, no patches (docs/14 §2). The UMD build assigns window.Unicode11Addon
   as a classic script; this shim loads it once and hands the class back as a named
   export. Upgrading = a new versioned directory plus a changed import path; this
   directory dies in the same commit. */
import { loadClassic } from "../load-classic.js";

export async function loadUnicode11Addon() {
  await loadClassic(new URL("./addon-unicode11.js", import.meta.url));
  if (typeof window.Unicode11Addon !== "function") {
    throw new Error("addon-unicode11.js loaded but window.Unicode11Addon is missing");
  }
  return window.Unicode11Addon;
}
