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

/* A fact chip (SPEC §panel.ui): one small mono value with its own state dot -
 * the Process page speaks about ports one chip at a time. The classes are
 * ui.css's (the class ratchet): a view never writes them, it calls this. */
import { h } from "../h.js";

export type ChipTone = "up" | "down" | "held";

export function portChip(text: string, o: { tone: ChipTone; title?: string }): HTMLElement {
  const cls = o.tone === "held" ? "pchip" : `pchip ${o.tone}`;
  return h("span", { class: cls, title: o.title }, text);
}
