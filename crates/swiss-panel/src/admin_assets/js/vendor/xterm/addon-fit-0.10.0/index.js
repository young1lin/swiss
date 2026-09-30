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
/* Vendored from the npm package whose tarball held lib/addon-fit.js — byte-identical, no
   further minification, no patches (SPEC §terminal.panel). As a classic script the UMD assigns
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