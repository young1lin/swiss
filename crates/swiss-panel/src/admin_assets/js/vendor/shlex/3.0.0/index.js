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
/* Vendored from the npm package shlex 3.0.0 (MIT, https://github.com/rgov/node-shlex) —
   shlex.js beside this file, byte-identical, no patches (docs/14 §2). A port of CPython's
   shlex, the same lexer the gateway's redis console splits a line with (the shlex crate,
   7017a51): the console's completion counts words through it, so a quoted argument is one
   word on both sides of the wire (docs/50). Upstream is already an ES module; this shim only
   names the one export the panel calls. Upgrading = a new versioned directory plus a
   changed import path; this directory dies in the same commit. */
export { split } from "./shlex.js";
