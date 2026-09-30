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

/* The object tab (SPEC §panel.pages): one open thing on a strip the page body owns - a Data table,
 * key or console (SPEC §data.tabs), a Terminal session. Flat, a hairline between neighbours; the
 * open one merges into the surface under it with a 2px accent on its top edge, the same
 * accent the context bar's page tab stands on. Its close is a real button BESIDE the name:
 * a tab is role=tab and holds no button of its own role (the terminal's tab was a button
 * holding a span that played one). The strip, the drag, the rename and the overflow stay
 * with the page; the tab is only the shape.
 *
 * No handlers: the view answers through its delegated listener, by the data hooks it passes
 * for the tab and for its close (SPEC §panel.toolchain). */
                                               
import { h } from "../h.js";
import { iconNode } from "./icon.js";

                             
                                                                                                 
               
                    
                
                                          
                                                                                           
                
                 
                                                                                              
                                                            
                                              
                                                                                          
                            
 

export function objTab(o            )              {
  /* A string name becomes the tab's own span; a node (an inline rename input) goes in as it
   * is. Hoisted out of the h() children on purpose: the L10b scanner descends into a
   * conditional sitting there and reads the typeof probe's "string" as visible copy. */
  const name         = typeof o.name === "string" ? h("span", { class: "otab-name" }, o.name) : o.name;
  return h("div", {
    class: o.selected ? "otab sel" : "otab", role: "tab",
    aria: { selected: o.selected ? "true" : "false" },
    // Roving tabindex, the tablist pattern: one Tab stop for the whole strip, the arrows move
    // inside it. Otherwise the only keyboard stop on a tab would be the button that closes it.
    tabIndex: o.selected ? 0 : -1,
    title: o.title, data: o.data,
  },
  o.icon ? iconNode(o.icon) : null,
  name,
  o.count ? h("span", { class: "otab-n tnum", title: o.count.title }, String(o.count.n)) : null,
  o.mark ?? null,
  h("button", {
    class: "otab-close", type: "button",
    aria: { label: o.close.label }, title: o.close.label, data: o.close.data,
  }, iconNode("x")));
}
