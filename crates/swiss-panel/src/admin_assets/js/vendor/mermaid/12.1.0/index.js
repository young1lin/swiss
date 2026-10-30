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
/* Vendored from mermaid 12.1.0 (MIT, https://github.com/mermaid-js/mermaid) - the @mermaid-js/tiny
   build of that release, dist/mermaid.tiny.js, byte-identical (the npm tarball's own file, sha1 of
   the tarball checked against the registry's shasum). A ```mermaid fence in a note is drawn as its
   diagram by ui/markdown.ts (flowchart, sequence, class, state, ER, gantt ...). Tiny, not the full
   build: the full mermaid.min.js bundles the Eclipse ELK layout engine (elkjs, EPL-2.0), copyleft
   this repository does not ship (deny.toml's posture, extended to the panel), and is twice the
   size. Tiny drops mindmap and architecture diagrams and KaTeX; such a fence stays as code with
   the reason in its title. The ~2.8MB cost is paid only when a note with a diagram is first shown -
   the loader below is the only path there. The IIFE bundle assigns globalThis.mermaid when
   evaluated as a classic script; this shim injects that script once through the shared loader and
   hands the API back as a named export (SPEC §panel.vendor). Upgrading = a new versioned directory
   plus a changed import path; this directory dies in the same commit. */
/* The classic-script loader was born in the xterm vendoring (SPEC §terminal.panel) and lives beside
   its shims, OUTSIDE the version directories - so this path stays put across xterm upgrades. */
import { loadClassic } from "../../xterm/load-classic.js";

export async function loadMermaid() {
  await loadClassic(new URL("./mermaid.tiny.js", import.meta.url));
  var lib = window.mermaid;
  if (!lib || typeof lib.render !== "function") throw new Error("mermaid.tiny.js loaded but window.mermaid carries nothing");
  return lib;
}
