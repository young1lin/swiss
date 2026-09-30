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

/* Status marks (SPEC §panel.ui, rules 2 and 10). Saturation is for state: a dot's colour and a
 * tag's tone say "up", "failed", "slow" and nothing else. A descriptive word - a launch type,
 * "proxy", "copies use this" - is a tag with no tone, monochrome, sans. mono only when the
 * tag holds a value you would copy (an exit code, a port). */
import { h } from "../h.js";

                                                                                          

/** A 6px state dot. The title is required: a colour names no behaviour of its own (SPEC §panel.design),
 *  so the dot says its state aloud - on hover, and to assistive tech. "off" is the bare
 *  .dot - the quiet grey of nothing running and nothing wrong - so it adds no class for a
 *  rule that would only repeat the base one. `null` is the one exception, and it must be
 *  deliberate: a dot inside something whose own title already explains it (a tunnel's
 *  "serves ghost" for an MCP that does not exist) - a title here would shadow that hover, so
 *  the dot has none and stays out of assistive tech. */
export function dot(state          , title               )              {
  const cls = state === "off" ? "dot" : "dot " + state;
  if (title === null) return h("span", { class: cls, aria: { hidden: "true" } });
  return h("span", { class: cls, title, role: "img", aria: { label: title } });
}

/** The amber mark of writes held back until Commit (SPEC §data.tabs): an open object's tab and the
 *  overflow row that stands in for it carry it. Its title is the count in words. A held write
 *  is not a process state, so it is its own mark and not a dot(). `null`, as for dot(): the
 *  thing that holds it already says it (a menu row), so it has no title and stays out of AT. */
export function heldDot(title               )              {
  if (title === null) return h("span", { class: "db-tab-dot", aria: { hidden: "true" } });
  return h("span", { class: "db-tab-dot", title, role: "img", aria: { label: title } });
}

;                         
                                                                                   
                        
                 
                 
 

export function tag(text        , o          = {})              {
  return h("span", {
    class: "tag" + (o.mono ? " mono" : "") + (o.tone ? " " + o.tone : ""),
    title: o.title,
  }, text);
}

/** Work in flight, inline with the words that say what it is ("Loading calls…"). Decorative:
 *  the words carry the meaning, so it is hidden from assistive tech. */
export function spinner()              {
  return h("span", { class: "spin", aria: { hidden: "true" } });
}
