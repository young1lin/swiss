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

/* Status marks (docs/46 §2.1, rules 2 and 10). Saturation is for state: a dot's colour and a
 * tag's tone say "up", "failed", "slow" and nothing else. A descriptive word - a launch type,
 * "proxy", "copies use this" - is a tag with no tone, monochrome, sans. mono only when the
 * tag holds a value you would copy (an exit code, a port). */
import { h } from "../h.js";

                                                                                          

/** A 6px state dot. The title is required: a colour names no behaviour of its own (docs/18
 *  V6), so the dot says its state aloud - on hover, and to assistive tech. "off" is the bare
 *  .dot - the quiet grey of nothing running and nothing wrong - so it adds no class for a
 *  rule that would only repeat the base one. */
export function dot(state          , title        )              {
  return h("span", { class: state === "off" ? "dot" : "dot " + state, title, role: "img", aria: { label: title } });
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
