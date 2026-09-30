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

/* The segmented control (SPEC §panel.ui): the L3 switcher - a resource's panes (Tools /
 * Resources / … / Logs), a list's filter (Actions / Everything), a picker inside a sheet.
 * Never an L2 page switcher: sibling pages belong to the context bar's underline tabs
 * (skill §5). It replaces the two hand-built strips, .seg and .db-tabs.
 *
 * Each button carries the page's own data hook (`key`, default "seg") with the item id, so a
 * view migrating onto this keeps the data-* name its delegated listener and its tests already
 * use (SPEC §panel.ui). */
                                       
import { h } from "../h.js";

                          
             
                
                                                           
                             
                 
                   
 

export function seg(items           , selected        , o                                                              
                                                                                               
                   = {})              {
  const key = o.key || "seg";
  return h("div", { class: "seg" + (o.fill ? " fill" : ""), role: "tablist", id: o.id, data: o.data, aria: { label: o.label } },
    items.map((it) => {
      return h("button", {
        type: "button", role: "tab", title: it.title, hidden: it.hidden,
        data: { [key]: it.id },
        aria: { selected: it.id === selected ? "true" : "false" },
      }, it.label, it.n != null ? h("span", { class: "seg-n" }, String(it.n)) : null);
    }));
}
