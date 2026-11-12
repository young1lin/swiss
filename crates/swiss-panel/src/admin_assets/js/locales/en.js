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

/* The English dictionary (SPEC §panel.i18n, normalized 2026-09-20): symbolic keys, one file
   per area under locales/en/. This barrel spreads them into the flat table callers import.
   English copy is editable in the part files without touching call sites or any other locale. */

import memory from "./en/memory.js";
import llm from "./en/llm.js";
import gallery from "./en/gallery.js";
import dataMongo from "./en/data-mongo.js";
import data from "./en/data.js";
import ops from "./en/ops.js";
import shell from "./en/shell.js";

const en                         = {
  ...memory,
  ...llm,
  ...gallery,
  ...dataMongo,
  ...data,
  ...ops,
  ...shell,
};

export default en;
