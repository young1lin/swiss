/* Vendored from cronstrue 2.52.0 (MIT, https://github.com/bradymholt/cronstrue) —
   dist/cronstrue.js, byte-identical, not minified any further than upstream ships it. The UMD
   bundle assigns window.cronstrue when evaluated as a classic script; this shim injects that
   script once through the shared loader and hands the library back as a named export
   (docs/14 §2, vendoring rules). Upgrading = a new versioned directory plus a changed
   import path; this directory dies in the same commit. */
/* The classic-script loader was born in the xterm vendoring (docs/14 §2) and lives beside
   its shims, OUTSIDE the version directories - so this path stays put across xterm upgrades. */
import { loadClassic } from "../../xterm/load-classic.js";

export async function loadCronstrue() {
  await loadClassic(new URL("./cronstrue.js", import.meta.url));
  var lib = window.cronstrue;
  if (!lib) throw new Error("cronstrue.js loaded but window.cronstrue carries nothing");
  return lib;
}
