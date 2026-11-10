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

/* The workspace template's parts (SPEC §panel.nav "workspace", §panel.ui). A workspace page owns
 * the whole pane (pane({ full }) - no padding, no measure, no scroll of its own) and divides it
 * into columns that each scroll on their own, read left to right:
 *
 *   workspace  the frame: a side column (wsSide) and the main column beside it.
 *   wsSide     a source list on the sidebar's own surface: a caption with its tools, then the
 *              sideRow()s - the projects, the connections.
 *   wsHead     the main column's head: the subject's name and a sub line, its actions, the tabs.
 *   wsSplit    two panes side by side under the head: one at a fixed width (a list you pick
 *              from at the start, an inspector at the end) and one that takes the rest; the
 *              edge between them is a grip the reader drags.
 *   wsBar      a strip of controls across a pane (its tools; `rows` a line each), or under it
 *              (`foot`: a count).
 *   wsScroll   the part of a pane that scrolls; `pad` for a reading column (a record, a form).
 *   pickList / pickRow
 *              the list a detail is picked from. A sideRow is one line because its name is the
 *              whole story; a pick row is a record (a note, a message) and earns a second line:
 *              its state and why it is in the list - an excerpt, the reasons a search matched.
 *              The list is a listbox and the row an option: aria-selected is the state and the
 *              style hook, and the selected row is the list's one tab stop (the page moves it
 *              with the arrow keys). `depth` indents a row under the one before it: a list
 *              that is a tree (a project's domains), at most four steps in.
 *   docHead    a record read whole, at the top of its detail: tags, the title, a meta line of
 *              values (its id, who, when), the verbs.
 *   (a record's own words are ui/markdown.ts markdownNode: the Markdown it is written in, drawn.)
 *   refLink    another record named in this one (a link's far end, a "replaced by"): its name,
 *              which goes there - text that reads as a link, not a push button among the verbs.
 *   catChip    a categorical identity (one of the --cat-1..5 slots, the graph's tones): the
 *              swatch and its word, or the swatch alone where a list has no room for the word.
 *
 * No handlers: each part carries the caller's data hooks for its delegated listener. */
                                               
import { h } from "../h.js";
import { grip } from "./grip.js";

/** A wsSide comes with its grip: the reader sets its width (one for every workspace page) or
 *  folds it away; the --ws-side-w token stays the default. */
export function workspace(side                    , ...main          )              {
  const edge = side && side.classList.contains("ws-side")
    ? grip(side, { axis: "x", key: "ws-side", min: 160, max: 520, label: side.getAttribute("aria-label") ?? "", fold: true })
    : null;
  return h("div", { class: "ws" }, side, edge, h("div", { class: "ws-main" }, ...main));
}

export function wsSide(o                                                , ...list          )              {
  return h("aside", { class: "ws-side", id: o.id, aria: { label: o.cap } },
    h("div", { class: "ws-side-head" },
      h("span", { class: "sec-cap" }, o.cap),
      o.tools && o.tools.length ? h("span", { class: "sec-tools" }, o.tools) : null),
    h("nav", { class: "side-list", role: "listbox", aria: { label: o.cap } }, ...list));
}

/** The name row (name, sub line, actions), then the nav - a seg of the subject's tabs. */
export function wsHead(o                                                                                     )              {
  return h("header", { class: "ws-head", id: o.id },
    h("div", { class: "ws-head-row" },
      h("div", { class: "ws-head-name" },
        h("h1", { class: "ws-title", title: o.title }, o.title),
        o.sub != null && o.sub !== false ? h("div", { class: "ws-sub" }, o.sub) : null),
      o.actions && o.actions.length ? h("div", { class: "ws-acts" }, o.actions) : null),
    o.nav ? h("div", { class: "ws-nav" }, o.nav) : null);
}

/** What the flexible pane keeps however far the fixed one is dragged, in px: the --ws-rest-min
 *  token, which caps the fixed pane in the stylesheet too (a width remembered in a wider window). */
const WS_REST_MIN = 320;
/** The bound of a drag on a split that is not laid out, so has no width to read. */
const WS_FIXED_MAX = 960;

/** `fixed`: which pane keeps its width - the start (a list, --ws-list-w) or the end (an
 *  inspector, --ws-aside-w). Each pane is a column; what scrolls in it is a wsScroll.
 *  The edge between the two is a grip: the reader drags the fixed pane wider or narrower, a
 *  double-click puts the token's width back. The width is one per kind of pane (`ws-list`,
 *  `ws-aside`), so the tabs of a page and the pages themselves keep one line; `key` names a
 *  split that should remember its own. It does not fold: the list is how a record is picked. */
export function wsSplit(o                                                                                                                           )              {
  const pane = (fixed         , label                    , body        )              =>
    h("section", { class: "ws-pane" + (fixed ? " fixed " + o.fixed : ""), aria: { label } }, body);
  const atEnd = o.fixed === "end";
  const start = pane(!atEnd, o.startLabel, o.start);
  const end = pane(atEnd, o.endLabel, o.end);
  const split = h("div", { class: "ws-split", id: o.id });
  const edge = grip(atEnd ? end : start, {
    axis: "x", key: o.key ?? (atEnd ? "ws-aside" : "ws-list"), min: atEnd ? 240 : 220, end: atEnd,
    label: (atEnd ? o.endLabel : o.startLabel) ?? "",
    max: () => {
      const room = Math.round(split.getBoundingClientRect().width);
      return room > 0 ? room - WS_REST_MIN : WS_FIXED_MAX;
    },
  });
  split.append(start, edge, end);
  return split;
}

/** `rows` stacks the controls a line each (a narrow list's tools: the mode, the search, the
 *  filters); without it they run along one line and wrap. */
export function wsBar(o                                                                    , ...controls          )              {
  if (o.rows) {
    return h("div", { class: "ws-bar rows", id: o.id, data: o.data },
      o.rows.map((r) => h("div", { class: "ws-bar-row" }, r)));
  }
  return h("div", { class: "ws-bar" + (o.foot ? " foot" : ""), id: o.id, data: o.data }, ...controls);
}

export function wsScroll(o                                , ...body          )              {
  return h("div", { class: "ws-scroll" + (o.pad ? " pad" : ""), id: o.id }, ...body);
}

export function pickList(o                                , ...rows          )              {
  return h("div", { class: "pick-list", id: o.id, role: "listbox", aria: { label: o.label } }, ...rows);
}

;                             
                                                           
                
               
                                                         
                
                     
                                                                             
               
                     
                                                          
                  
                                                                    
                 
                 
 

export function pickRow(o             )                    {
  const depth = Math.min(Math.max(Math.trunc(o.depth || 0), 0), 4);
  const b = h("button", {
    type: "button", class: "pick-row" + (depth ? " d" + String(depth) : "") + (o.muted ? " muted" : ""), role: "option",
    aria: { selected: o.selected ? "true" : "false" }, data: o.data,
  },
    h("span", { class: "pick-top" },
      o.lead != null && o.lead !== false ? h("span", { class: "pick-lead" }, o.lead) : null,
      h("span", { class: "pick-name", title: o.name }, o.name),
      o.meta != null && o.meta !== false ? h("span", { class: "pick-meta", title: o.metaTitle }, o.meta) : null),
    o.sub != null && o.sub !== false ? h("span", { class: "pick-sub" }, o.sub) : null);
  b.tabIndex = o.selected ? 0 : -1;
  return b;
}

/** Move a pick list's selection in place - the mark and the tab stop - without a repaint, so the
 *  list keeps its scroll. Returns the newly selected row. */
export function pickSelect(list             , match                               )                     {
  let hit                     = null;
  list.querySelectorAll             (".pick-row").forEach((r) => {
    const on = match(r);
    r.setAttribute("aria-selected", on ? "true" : "false");
    r.tabIndex = on ? 0 : -1;
    if (on && !hit) hit = r;
  });
  return hit;
}

/** `lede` is the line under the title saying what the page is; `meta` is a run of values (an id,
 *  who, when), drawn with a quiet separator between them. */
export function docHead(o                                                                                      )              {
  const meta           = [];
  for (const m of o.meta || []) {
    if (m == null || m === false || m === "") continue;
    if (meta.length) meta.push(h("span", { class: "doc-sep", aria: { hidden: "true" } }, "·"));
    meta.push(m);
  }
  return h("header", { class: "doc-head" },
    o.tags != null && o.tags !== false ? h("div", { class: "doc-tags" }, o.tags) : null,
    h("h2", { class: "doc-title" }, o.title),
    o.lede != null && o.lede !== false && o.lede !== "" ? h("p", { class: "doc-lede" }, o.lede) : null,
    meta.length ? h("div", { class: "doc-meta" }, meta) : null,
    o.actions && o.actions.length ? h("div", { class: "doc-acts" }, o.actions) : null);
}

/** A reference to another record by its name: the caller's data hook says where it goes. */
export function refLink(label        , o                                                     = {})                    {
  return h("button", { type: "button", class: "ref", data: o.data, title: o.title ?? label }, o.code ? h("code", null, label) : label);
}

/** `tone` is a categorical slot, 1-5 (ui/graph.ts GraphTone). `bare` draws the swatch alone and
 *  puts the word in its tooltip and its accessible name. */
export function catChip(tone                   , label        , o                     = {})              {
  return o.bare
    ? h("span", { class: "cat bare c" + String(tone), title: label, role: "img", aria: { label } })
    : h("span", { class: "cat c" + String(tone) }, label);
}
