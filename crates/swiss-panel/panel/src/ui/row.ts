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

/* The ONE list row (docs/46 §2.3). docs/46 §0.2 counted four hand-built rows - .row,
 * .tun-row, the remote row, the plugin row - that agreed on nothing but "a name on the left,
 * buttons on the right". This is that shape, once:
 *
 *   [lead] name                         [cols…] [toggle] [primary] [⋯]
 *          sub (or err, in red)
 *
 *   lead     a dot() - the state column. Its x is the groups contract (ui.css): row pad 16 +
 *            dot 6 + gap 12 puts the name at 34, under the group band's name.
 *   name     identity: sans, --w-name. Never mono (rule 1).
 *   sub      ONE line, ellipsized. Prose is sans; a value inside it is a <code> (mono by
 *            base.css), so "5432 → 127.0.0.1:5432 via bastion" mixes the two honestly.
 *   err      replaces sub: one red line, ellipsized, the whole reason in the tooltip - so a
 *            failing row is as tall as a healthy one.
 *   cols     right-aligned value columns (a port, a rule count, a last run).
 *   toggle / primary / more
 *            at most one word button per row (rule 4, by type: `primary` is ONE button).
 *
 *   detail   the item's whole record (a tool's description and input schema): the name and sub
 *            become a native disclosure - a chevron, and a <details> that opens under them. The
 *            row's own controls stay outside it, so Try or the switch never opens it.
 *
 * Separators are inset to the text column (docs/46 U7): .lrow.has-lead moves the line past
 * the dot, .has-disc past the chevron. No handlers - the row carries the caller's data hooks
 * for its delegated listener. */
import type { AttrMap, HChild } from "../h.js";
import { h } from "../h.js";
import { iconNode } from "./icon.js";

export interface RowCol {
  v: HChild;
  mono?: boolean;
  title?: string;
}

export interface RowOpts {
  lead?: HChild;
  name: HChild;
  sub?: HChild;
  err?: string;
  cols?: Array<HChild | RowCol>;
  toggle?: HTMLElement;
  primary?: HTMLButtonElement;
  more?: HTMLButtonElement;
  data?: AttrMap;
  draggable?: boolean;
  /** Dims the name - a switched-off item. Its state still belongs to a tag or the switch. */
  muted?: boolean;
  title?: string;
  /** The record behind the row, opened in place (a <details>: no script, keyboard for free). */
  detail?: HChild;
}

function isCol(c: HChild | RowCol): c is RowCol {
  return c != null && typeof c === "object" && !(c instanceof Node) && !Array.isArray(c) && "v" in c;
}

export function row(o: RowOpts): HTMLElement {
  const hasLead = o.lead != null && o.lead !== false;
  const disc = o.detail != null && o.detail !== false;
  const acts = [o.toggle, o.primary, o.more].filter((x): x is HTMLElement => x != null);
  const text: HChild[] = [
    h("div", { class: "lrow-name" }, o.name),
    o.err
      ? h("div", { class: "lrow-err", title: o.err }, o.err)
      : o.sub != null && o.sub !== false ? h("div", { class: "lrow-sub" }, o.sub) : null,
  ];
  const main = disc
    ? h("details", { class: "lrow-main lrow-disc" },
        h("summary", null, h("span", { class: "lrow-chev" }, iconNode("chevron-right")), h("span", { class: "lrow-text" }, text)),
        h("div", { class: "lrow-detail" }, o.detail))
    : h("div", { class: "lrow-main" }, text);
  const node = h("div", {
    class: "lrow" + (hasLead ? " has-lead" : "") + (disc ? " has-disc" : "") + (o.muted ? " muted" : ""),
    data: o.data, title: o.title,
  },
    hasLead ? h("span", { class: "lrow-lead" }, o.lead) : null,
    main,
    (o.cols || []).map((c) => {
      return isCol(c)
        ? h("span", { class: "lrow-col" + (c.mono ? " mono" : ""), title: c.title }, c.v)
        : h("span", { class: "lrow-col" }, c);
    }),
    acts.length ? h("div", { class: "lrow-acts" }, acts) : null);
  // The attribute, not the property: .lrow[draggable="true"] (the grab cursor) keys on it, and
  // writing it outright keeps the hook true whatever the DOM does about reflecting the property.
  if (o.draggable) node.setAttribute("draggable", "true");
  return node;
}

/** A label / value pair - config, system facts. The value is mono when it is one you would
 *  copy (a path, a key); a sentence stays sans. */
export function kvRow(label: string, value: HChild, o: { mono?: boolean; title?: string } = {}): HTMLElement {
  return h("div", { class: "kv" },
    h("span", { class: "kv-k" }, label),
    h("span", { class: "kv-v" + (o.mono ? " mono" : ""), title: o.title }, value));
}

export interface SideRowOpts {
  name: string;
  /** The state column: a dot(). */
  lead?: HChild;
  /** The trailing slot: what you scan the list for - a launch tag, or its glyph. */
  tail?: HChild;
  selected?: boolean;
  data?: AttrMap;
  title?: string;
}

/** One source-list row: the sidebar beside a detail pane (the MCP list; skill §17 "sidebar").
 *  One line, 30px: dot, name, the trailing tag. Everything else - the description, the source,
 *  the reason a row is red - lives in the title and in the pane head of the selected one (the
 *  second line it used to carry was a ruler of ellipses). A <button role=option>: the list is a
 *  listbox, the row is what you pick, and aria-selected is both the state and the style hook
 *  (base.css .side-row). Moves here from sidebar.ts's builder at P2; the shape is the same node
 *  for node. */
export function sideRow(o: SideRowOpts): HTMLButtonElement {
  return h("button", {
    type: "button", class: "side-row", role: "option", title: o.title, data: o.data,
    aria: { selected: o.selected ? "true" : "false" },
  },
  o.lead ?? null,
  h("span", { class: "side-name" }, o.name),
  h("span", { class: "side-type" }, o.tail ?? null));
}
