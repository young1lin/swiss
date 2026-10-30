/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
/* Vendored from cytoscape 3.34.3 (MIT, https://github.com/cytoscape/cytoscape.js) -
   dist/cytoscape.min.js, byte-identical (the npm tarball's own file, sha1 of the tarball
   checked against the registry's shasum). The memory page's graphs (SPEC §memory 11.2,
   ui/graph.ts) draw on it: a canvas renderer with real layouts (cose, concentric), pan and
   zoom, edge labels and arrowheads - the things a hand-rolled SVG graph did badly. The
   ~435KB cost is paid only when a graph is first shown - the loader below is the only path
   there. The UMD bundle assigns globalThis.cytoscape when evaluated as a classic script;
   this shim injects that script once through the shared loader and hands the factory back
   as a named export (SPEC §panel.vendor). Upgrading = a new versioned directory plus a
   changed import path; this directory dies in the same commit. */
/* The classic-script loader was born in the xterm vendoring (SPEC §terminal.panel) and lives beside
   its shims, OUTSIDE the version directories - so this path stays put across xterm upgrades. */
import { loadClassic } from "../../xterm/load-classic.js";

export async function loadCytoscape() {
  await loadClassic(new URL("./cytoscape.min.js", import.meta.url));
  var lib = window.cytoscape;
  if (typeof lib !== "function") throw new Error("cytoscape.min.js loaded but window.cytoscape carries nothing");
  return lib;
}
