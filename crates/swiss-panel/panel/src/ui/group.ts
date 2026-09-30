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

/* A user-named container (rule 5, SPEC §panel.groups): a header BAND over its members - chevron, name,
 * count leading; + (always, dimmed) and ⋯ (hover) trailing. Two densities: "side" (the
 * sidebar, 28px band, members one grid step in) and "page" (the band across the top of a
 * card, rows edge to edge under it). ui.css holds the x-coordinate contract.
 *
 * This is the MARKUP. groups.ts mountGroup builds on it and wires the behaviour - fold,
 * drag, drop, the + and ⋯ acts, the API writes - which is why groupNode hands back its
 * parts instead of one element: the wiring needs the toggle, the two buttons and the body,
 * and finding them again with querySelector would be a second contract to keep in step. */
import type { AttrMap, HChild } from "../h.js";
import { h } from "../h.js";
import { iconNode } from "./icon.js";

export interface GroupNodeOpts {
  /** The group's key - data-group, which drag and drop address. */
  name: string;
  /** What the band shows; defaults to the name. */
  label?: string;
  count: number;
  density: "side" | "page";
  collapsed?: boolean;
  /** Title (and aria-label) of the + ; null or absent draws no +. */
  addTitle?: string | null;
  /** Title (and aria-label) of the ⋯ ; null or absent draws no ⋯. */
  moreTitle?: string | null;
  /** The one quiet line an empty group shows (it is a place, not an empty state). */
  emptyText?: string;
  data?: AttrMap;
}

export interface GroupParts {
  root: HTMLElement;
  head: HTMLElement;
  toggle: HTMLButtonElement;
  add: HTMLButtonElement | null;
  more: HTMLButtonElement | null;
  body: HTMLElement;
  empty: HTMLElement | null;
}

export function groupNode(o: GroupNodeOpts, ...members: HChild[]): GroupParts {
  const page = o.density === "page";
  const toggle = h("button", { type: "button", class: "grp-toggle", aria: { expanded: String(!o.collapsed) } },
    h("span", { class: "grp-chev" }, iconNode("chevron-right")),
    h("span", { class: "grp-name" }, o.label ?? o.name),
    h("span", { class: "grp-n" }, String(o.count)));
  const add = o.addTitle
    ? h("button", { class: "grp-add", type: "button", title: o.addTitle, aria: { label: o.addTitle } }, iconNode("plus"))
    : null;
  const more = o.moreTitle
    ? h("button", { class: "grp-more", type: "button", title: o.moreTitle, aria: { label: o.moreTitle } }, iconNode("ellipsis"))
    : null;
  const head = h("div", { class: "grp-head" }, toggle, add, more);
  const empty = o.emptyText != null ? h("div", { class: "grp-empty" }, o.emptyText) : null;
  const body = h("div", { class: "grp-body" }, ...members, empty);
  // At page density the group IS the card: .group brings the ring, the radius and the clip.
  const root = h("div", {
    class: "grp grp--" + o.density + (page ? " group" : "") + (o.collapsed ? " collapsed" : ""),
    data: Object.assign({ group: o.name }, o.data),
  }, head, body);
  return { root, head, toggle, add, more, body, empty };
}
