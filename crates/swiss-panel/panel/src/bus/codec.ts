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

/* The vendored protobuf runtime (SPEC §bus.codegen, SPEC §panel.vendor). One import
   site, so the served path and the type mirror stay the same relative path. */

export { create } from "../vendor/protobuf/2.16.0/create.js";
export { fromBinary } from "../vendor/protobuf/2.16.0/from-binary.js";
export { toBinary } from "../vendor/protobuf/2.16.0/to-binary.js";
