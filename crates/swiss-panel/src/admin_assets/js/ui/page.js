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

/* Page scaffolding (SPEC §panel.ui). A content page is, top to bottom: a head (one sentence
 * of description, the page's actions), maybe an inline create form, sections of cards, a
 * foot. Each piece is one function here so every page draws it the same way:
 *
 *   pane       the page body itself: the padded, scrolling frame under the bars, every block
 *              in it capped at the measure (SPEC §panel.nav). `wide` for genuinely wide rows (and the
 *              frame the pinned head lives in), `full` for a workspace (Data, Terminal).
 *   paneBody   the same measure frame for a view that fills the shell's own #pane.
 *   paneHead   pinned on a content page (ui.css, SPEC §panel.design). No location title - the
 *              context bar already says where you are (SPEC §panel.nav).
 *   resHead    a RESOURCE head (the selected MCP), where the name is the page's subject: the
 *              name row and the resource's tabs pin, its words scroll away (SPEC §panel.pages).
 *   section    a product-named caption over its body, with the section's tools at its end.
 *              A caption the user named is a group band instead (rule 5).
 *   card       the grouped inset surface rows sit on.
 *   note       a quiet line under a body (busy: with the spinner; err: a contained failure).
 *   failNote   a load that failed, in place: the sentence, the status, the retry.
 *   pager      newer / where you are / older under a paged list.
 *   filterInput the search box in a section's tools.
 *   pageFoot   the revision line. Never a count the context bar already shows (rule 25).
 *   inlineForm one row: the fields, a Group select, the one primary (SPEC §panel.groups).
 *   emptyNode  the one "nothing here" shape (SPEC §panel.design); its action answers
 *              [data-empty-action] in the owning view's delegated listener. */
                                               
import { h } from "../h.js";
import { iconNode } from "./icon.js";
import { spinner } from "./status.js";

                           
                                                                                          
                 
                                                                                            
                 
              
 

/** The page body. The shell's #pane is this element (index.html); a gallery scene builds its
 *  own, which is how a design shows a whole page without a line of its own CSS. */
export function pane(o          , ...body          )              {
  return h("main", { class: o.full ? "pane full" : "pane", id: o.id },
    o.wide ? paneBody({ wide: true }, ...body) : body);
}

/** A page's body inside the shell's own #pane (which a view fills, never replaces): one frame
 *  so the measure is applied once and header, tabs and body share a left edge. `wide` is the
 *  wider measure and the frame a pinned head (paneHead, resHead) sticks in. */
export function paneBody(o                    , ...body          )              {
  return h("div", o.wide ? { class: "wide" } : null, ...body);
}

export function paneHead(o                                                                     )              {
  return h("div", { class: "pane-head" },
    h("div", null,
      o.title != null && o.title !== false ? h("h1", { class: "pane-title" }, o.title) : null,
      o.desc != null && o.desc !== false ? h("div", { class: "pane-desc" }, o.desc) : null,
      o.sub != null && o.sub !== false ? h("div", { class: "pane-sub" }, o.sub) : null),
    o.actions && o.actions.length ? h("div", { class: "pane-actions" }, o.actions) : null);
}

/** A RESOURCE head (SPEC §panel.pages) - the selected MCP's name, words, state and sections - as
 *  two pinned layers instead of one block. The name row pins at the top (the content-page
 *  pin, .pane > .wide > .pane-head); the description and state line are ordinary flow and
 *  scroll away beneath it; the nav (a seg) pins under the name row. Stuck, it reads as the
 *  head "collapsing" to name + tabs, with no script state and no change in the head's own
 *  height - a head that shrank on scroll would shorten the page it was scrolling. The shell
 *  measures the layers into --pin-title-h / --pane-head-h (pane-scroll.ts). Returned as the
 *  pane's children, since each layer must be a child of the .wide frame to stick in it. */
export function resHead(o                                                                                       )                {
  const has = (x                    )          => x != null && x !== false;
  // The name row clips a long name to one line; the tooltip keeps the whole of it.
  const whole = typeof o.title === "string" ? o.title : undefined;
  const out = [h("div", { class: "pane-head res" },
    h("h1", { class: "pane-title", title: whole }, o.title),
    o.actions && o.actions.length ? h("div", { class: "pane-actions" }, o.actions) : null)];
  if (has(o.desc) || has(o.sub)) {
    out.push(h("div", { class: "res-meta" },
      has(o.desc) ? h("div", { class: "pane-desc" }, o.desc) : null,
      has(o.sub) ? h("div", { class: "pane-sub" }, o.sub) : null));
  }
  if (o.nav) out.push(h("div", { class: "pane-nav" }, o.nav));
  return out;
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

/** A quiet line under a body: what just happened, what is loading. `busy` leads it with the
 *  spinner; `err` is a failure from the far side - contained, tinted, scrolled, never the
 *  loudest thing on the screen (ui.css .note.err). */
export function note(body        , o                                                                 = {})              {
  return h("div", { class: o.err ? "note err" : "note", id: o.id, data: o.data },
    o.busy ? [spinner(), " "] : null, body);
}

/** A load that failed, in place: the sentence, the status code (the only secondary text - a
 *  response body never lands in the panel, SPEC §mcp.calls) and the way out. */
export function failNote(o                                                                   )              {
  return h("div", { class: "fail-note", id: o.id, role: "status" },
    h("span", null, o.text),
    o.why != null && o.why !== false && o.why !== "" ? h("span", { class: "fail-why" }, o.why) : null,
    o.action || null);
}

/** A list's pages: the newer button, where you are, the older button. The status is a live
 *  region, so a switch is announced; the view patches the buttons and the status in place
 *  rather than repainting the list (SPEC §mcp.calls), which is why it names them by id. */
export function pager(o                                                                                                         )              {
  return h("div", { class: "pager", id: o.id, role: "navigation", aria: { label: o.label } },
    o.prev,
    h("span", { class: "pager-status", id: o.statusId, aria: { live: "polite" } }, o.status),
    o.next);
}

/** A filter box for a section's tools (section({ tools })): a search field that takes the width
 *  a filter needs, not a form field's full width. */
export function filterInput(o                                                                     )                   {
  return h("input", {
    class: "filter", id: o.id, type: "search", autocomplete: "off",
    placeholder: o.placeholder, aria: { label: o.label }, value: o.value ?? "",
  })                    ;
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
