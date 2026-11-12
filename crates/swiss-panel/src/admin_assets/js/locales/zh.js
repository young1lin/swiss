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

/* The Chinese dictionary (SPEC §panel.i18n): the same part files and keys as locales/en/.
   A plural's entry is the .other form only — Chinese has no "one" category under
   Intl.PluralRules("zh-CN"), so trn() never looks for one. */

import memory from "./zh/memory.js";
import llm from "./zh/llm.js";
import gallery from "./zh/gallery.js";
import dataMongo from "./zh/data-mongo.js";
import data from "./zh/data.js";
import ops from "./zh/ops.js";
import shell from "./zh/shell.js";

const zh                         = {
  ...memory,
  ...llm,
  ...gallery,
  ...dataMongo,
  ...data,
  ...ops,
  ...shell,
};

export default zh;
