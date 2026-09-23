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

/* Page scaffolding (docs/46 §2.2). A content page is, top to bottom: a head (one sentence
 * of description, the page's actions), maybe an inline create form, sections of cards, a
 * foot. Each piece is one function here so every page draws it the same way:
 *
 *   pane       the page body itself: the padded, scrolling frame under the bars, every block
 *              in it capped at the measure (skill §6). `wide` for genuinely wide rows (and the
 *              frame the pinned head lives in), `full` for a workspace (Data, Terminal).
 *   paneHead   pinned on a content page (ui.css, docs/46 U9). No location title - the
 *              context bar already says where you are (skill §7); `title` is for a RESOURCE
 *              head (the selected MCP), where the name is the page's subject.
 *   section    a product-named caption over its body, with the section's tools at its end.
 *              A caption the user named is a group band instead (rule 5).
 *   card       the grouped inset surface rows sit on.
 *   pageFoot   the revision line. Never a count the context bar already shows (rule 25).
 *   inlineForm one row: the fields, a Group select, the one primary (docs/35 §3).
 *   emptyNode  the one "nothing here" shape (docs/18 V7); its action answers
 *              [data-empty-action] in the owning view's delegated listener. */
                                      
import { h } from "../h.js";
import { iconNode } from "./icon.js";

                           
                                                                                          
                 
                                                                                            
                 
              
 

/** The page body. The shell's #pane is this element (index.html); a gallery scene builds its
 *  own, which is how a design shows a whole page without a line of its own CSS. */
export function pane(o          , ...body          )              {
  return h("main", { class: o.full ? "pane full" : "pane", id: o.id },
    o.wide ? h("div", { class: "wide" }, ...body) : body);
}

export function paneHead(o                                                                     )              {
  return h("div", { class: "pane-head" },
    h("div", null,
      o.title != null && o.title !== false ? h("h1", { class: "pane-title" }, o.title) : null,
      o.desc != null && o.desc !== false ? h("div", { class: "pane-desc" }, o.desc) : null,
      o.sub != null && o.sub !== false ? h("div", { class: "pane-sub" }, o.sub) : null),
    o.actions && o.actions.length ? h("div", { class: "pane-actions" }, o.actions) : null);
}

/** `note` is one line under the caption that says what the section holds - a sentence, so
 *  sans and --text-3 (the page's own copy, never a value). */
export function section(o                                                   , ...body          )              {
  const head = o.cap || (o.tools && o.tools.length)
    ? h("div", { class: "sec-head" },
        o.cap ? h("span", { class: "sec-cap" }, o.cap) : h("span"),
        o.tools && o.tools.length ? h("span", { class: "sec-tools" }, o.tools) : null)
    : null;
  const note = o.note != null && o.note !== false ? h("div", { class: "hint sec-note" }, o.note) : null;
  return h("section", { class: "sec" }, head, note, ...body);
}

export function card(...rows          )              {
  return h("div", { class: "group" }, ...rows);
}

/** `note` is prose (sans); `rev` is a value you would quote back (mono, rule 1). */
export function pageFoot(o                                 )              {
  return h("div", { class: "page-foot" },
    h("span", null, o.note),
    o.rev ? h("span", { class: "page-foot-rev" }, o.rev) : null);
}

export function inlineForm(...controls          )              {
  return h("div", { class: "inline-form" }, ...controls);
}

;                           
               
                
                
                  
 

export function emptyNode(o           )              {
  return h("div", { class: "empty" },
    h("div", null,
      h("span", { class: "empty-ic" }, iconNode(o.icon)),
      h("h2", null, o.title),
      o.hint ? h("p", { class: "hint" }, o.hint) : null,
      o.action ? h("button", { class: "btn ghost", data: { "empty-action": o.action } }, o.action) : null));
}
