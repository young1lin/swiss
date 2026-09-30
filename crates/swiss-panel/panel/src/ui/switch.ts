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

/* The on/off switch (SPEC §panel.ui): a state you set, not an action you fire. A <button
 * role="switch"> rather than a checkbox, so it takes the same delegated click and disabled
 * handling as every other row control; aria-checked is both the state and the CSS hook. The
 * label is required - a switch beside a row name still needs to say WHAT it switches. */
import type { AttrMap } from "../h.js";
import { h } from "../h.js";

export function sw(on: boolean, label: string, o: { id?: string; data?: AttrMap; title?: string; disabled?: boolean } = {}): HTMLButtonElement {
  return h("button", {
    type: "button", class: "sw", role: "switch",
    id: o.id, title: o.title, disabled: o.disabled, data: o.data,
    aria: { checked: on ? "true" : "false", label },
  });
}
