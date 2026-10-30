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

/* An entry picker (SPEC §panel.ui): the inline browser a field opens when its
 * value is a file on this machine. This builds the SHAPE - the bar (a path
 * input, Go, Up) over a scrollable list - and hands the pieces back; the page
 * owns the loading and the choice. No handlers and no state here. */
import { h } from "../h.js";
import { btn } from "./button.js";

export interface PickPanel {
  root: HTMLElement;
  path: HTMLInputElement;
  go: HTMLButtonElement;
  up: HTMLButtonElement;
  list: HTMLElement;
}

/** The kind mark: a directory points, an executable is a filled square, the
 * rest are quiet middots - three shapes, no words to translate. */
export function pickerRow(kind: "dir" | "exe" | "other", name: string, mono: boolean): HTMLButtonElement {
  const mark = kind === "dir" ? "\u25b8" : kind === "exe" ? "\u25aa" : "\u00b7";
  return h("button", { class: "dirpick-row", type: "button" },
    h("span", { class: "dirpick-kind", aria: { hidden: true } }, mark),
    h("span", { class: mono ? "dirpick-name mono" : "dirpick-name" }, name)) as HTMLButtonElement;
}

export function pickPanel(pathPlaceholder: string, goLabel: string, upLabel: string): PickPanel {
  const path = h("input", { type: "text", placeholder: pathPlaceholder }) as HTMLInputElement;
  const go = btn(goLabel, { kind: "ghost" });
  const up = btn(upLabel, { kind: "ghost" });
  const list = h("div", { class: "dirpick-list" });
  const root = h("div", { class: "dirpick", hidden: true },
    h("div", { class: "dirpick-bar" }, path, go, up),
    list);
  return { root, path, go, up, list };
}
