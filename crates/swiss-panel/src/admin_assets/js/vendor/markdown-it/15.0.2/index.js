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
/* Vendored from markdown-it 15.0.2 (MIT, https://github.com/markdown-it/markdown-it) -
   dist/browser/markdown-it.esm.min.mjs, byte-identical (the npm tarball's own file, sha1 of the
   tarball checked against the registry's shasum). A CommonMark parser with GFM tables and
   strikethrough, its dependencies (entities, linkify-it, mdurl, punycode, uc.micro) bundled in:
   ui/markdown.ts draws a note, a hub and a brief with it (SPEC §panel.vendor). Upstream is
   already a self-contained ES module; this shim only names the one export the panel calls.
   Upgrading = a new versioned directory plus a changed import path; this directory dies in the
   same commit. */
export { default } from "./markdown-it.esm.min.mjs";
