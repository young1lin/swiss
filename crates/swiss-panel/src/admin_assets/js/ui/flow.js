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

/* The flow (SPEC §panel.ui): work as it reads - an agent's rounds and tool calls, a sync's
 * runs - one quiet line per step, prose between the lines where the work said something, and a
 * step opened in place for what it carried. The event list (ui/timeline.ts) is a log a reader
 * scans by time; the flow is a story a reader follows, so it drops the clock column and lets
 * the glyph, the words and the space between rows carry the order:
 *
 *   [◎] Thought  ·  weighing the notes first                       3.2 s
 *       The note holds; the second file is next.          <- flowProse()
 *   [▤] read_file  ·  src/a.rs :4+30                             120 ms
 *   [▤] read_file  ·  src/b.rs                         ~~shimmer~~ ● 3.0 s   <- live
 *
 *   lead     a glyph (an icon name) or the caller's node (a state dot); a chevron takes its
 *            place on hover and while the step is open, so the row says it opens
 *   title    the step's name: a word, or a value you would type (`mono`: a tool's name)
 *   summary  one line after a dot separator, cut with an ellipsis - its whole text on hover
 *   detail   a second line under the row, cut at two lines while closed (a run's verdict)
 *   meta     right-aligned and tabular: a duration, a moment
 *   status   a failure is a toned tag, never a red row (rule 2)
 *   live     still in flight: the summary shimmers, and a pulse stands before the meta - unless
 *            the lead is the caller's own state dot, which already pulses
 *   tools    controls beside the row, outside its toggle (a Stop)
 *
 * No handlers: the view's delegated click answers .fl-sum and calls flowToggle with the body it
 * owns, as with the event list. A closed step's body is painted on open, or up front with
 * `eager` (hidden until opened) when the list is short and its text must be findable. */
                                               
import { h } from "../h.js";
import { copyBtn } from "./copy.js";
import { iconNode } from "./icon.js";
import { dot, tag } from "./status.js";

                           
                                                                                   
             
                              
                
                 
                   
                        
                  
                                              
                      
                
                                                  
                 
                 
                 
 

                           
                     
                                            
                                 
                                                                
                  
 

function leadNode(lead                  )              {
  const glyph = typeof lead === "string" ? iconNode(lead) : lead || iconNode("chevron-right");
  return h("span", { class: "fl-lead" },
    h("span", { class: "fl-glyph" }, glyph),
    h("span", { class: "fl-chev" }, iconNode("chevron-right")));
}

function stepNode(s          , o          )              {
  const open = !!o.open && o.open.has(s.id);
  const body = o.body && (open || o.eager) ? o.body(s) : null;
  const pulse = !!s.live && !(s.lead instanceof HTMLElement);
  return h("div", { class: "fl-step" + (open ? " open" : "") + (s.live ? " live" : ""), data: Object.assign({ "fl-id": s.id }, s.data) },
    h("div", { class: "fl-row" },
      h("button", { type: "button", class: "fl-sum", aria: { expanded: String(open) } },
        leadNode(s.lead),
        h("span", { class: "fl-main" },
          h("span", { class: "fl-line" },
            h("span", { class: "fl-title" + (s.mono ? " mono" : "") }, s.title),
            s.summary ? h("span", { class: "fl-sep", aria: { hidden: "true" } }) : null,
            s.summary ? h("span", { class: "fl-summary" + (s.summaryMono ? " mono" : ""), title: s.summary }, s.summary) : null),
          s.detail != null && s.detail !== false && s.detail !== ""
            ? h("span", { class: "fl-detail" + (s.detailBad ? " bad" : "") }, s.detail)
            : null),
        s.status ? h("span", { class: "fl-flag" }, tag(s.status.text, { tone: s.status.tone })) : null,
        s.meta != null || pulse
          ? h("span", { class: "fl-meta" }, pulse ? dot("starting", null) : null, s.meta)
          : null),
      s.tools != null && s.tools !== false ? h("span", { class: "fl-tools" }, s.tools) : null),
    body != null ? h("div", { class: "fl-body", hidden: !open }, body) : null);
}

/** The steps, with the caller's prose nodes (flowProse) standing between them as given. */
export function flow(items                                      , o           = {})              {
  return h("div", { class: "fl" }, items.map((it) => (it == null ? null : it instanceof HTMLElement ? it : stepNode(it, o))));
}

/** What the work said between two steps - a model's words - at the title's indent. */
export function flowProse(...body          )              {
  return h("div", { class: "fl-prose" }, ...body);
}

/** Open or close one step in place: the class, aria-expanded, and its body - the one handed in
 *  (painted on open, replacing the old), else the one painted up front, shown or hidden. Pure
 *  DOM, no listener - the view's delegated click calls it. Answers the new open state; false
 *  when no step has that id. */
export function flowToggle(root             , id        , body         )          {
  const step = Array.prototype.find.call(root.querySelectorAll             (".fl-step"),
    (n             ) => n.dataset.flId === id)                           ;
  if (!step) return false;
  const open = !step.classList.contains("open");
  step.classList.toggle("open", open);
  const sum = step.querySelector(".fl-sum");
  if (sum) sum.setAttribute("aria-expanded", String(open));
  let held = step.querySelector             (":scope > .fl-body");
  if (open && body != null) {
    if (held) held.remove();
    held = h("div", { class: "fl-body" }, body);
    step.appendChild(held);
  }
  if (held) held.hidden = !open;
  return open;
}

/** A step's prose body past the prose cap (a round's thinking, a prompt part): it scrolls in
 *  place, so one long thought cannot push the answer, the next step and the box below it off
 *  the column. The flow's own shell - a view asks for it rather than drawing the class. */
export function scrollBox(body        )              {
  return h("div", { class: "fl-scroll" }, body);
}

/** A step's input and output as one surface: each part under its caption in a gutter, scrolled
 *  past a cap so one long result never floods the flow. Notes ride beside a caption (a size);
 *  `copy` puts the part's own text on the clipboard from a glyph in that caption - what was
 *  sent and what came back are things a reader takes away, not retypes. */
export function flowIO(parts                                                                                                                    )              {
  return h("div", { class: "fl-io" }, parts.map((p) => (p
    ? h("div", { class: "fl-io-part", data: p.data },
        h("div", { class: "fl-io-label" },
          p.label,
          (p.notes || []).map((n) => h("span", { class: "fl-io-note" }, n)),
          p.copy == null ? null : copyBtn(p.copy, p.copyLabel == null ? {} : { label: p.copyLabel })),
        h("div", { class: "fl-io-body" }, p.body))
    : null)));
}

/** The head over a flow: its fact lines stacked (factLine), the first the strong one - what a
 *  sheet's run is, before its steps. */
export function flowHead(...lines          )              {
  return h("div", { class: "fl-head" }, ...lines);
}

/** Facts on one line, joined by the dot separator: a head's state, who ran it, what it cost.
 *  Empty parts drop out. `lead` is the strong first line of a head. */
export function factLine(parts          , o                                     = {})              {
  const kids           = [];
  parts.filter((p) => p != null && p !== false && p !== "").forEach((p, i) => {
    if (i) kids.push(h("span", { class: "fl-sep", aria: { hidden: "true" } }));
    kids.push(h("span", { class: "fl-fact" }, p));
  });
  return h("div", { class: "fl-facts" + (o.lead ? " lead" : ""), data: o.data }, kids);
}
