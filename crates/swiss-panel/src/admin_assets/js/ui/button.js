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

/* Buttons (SPEC §panel.ui). Three shapes and no fourth:
 *
 *   btn      a word. The page's one primary is kind "primary"; everything else is the plain
 *            push button, or "ghost" where it must not compete (a row's secondary act).
 *   iconBtn  a glyph that stands for a word - the word is the aria-label and the tooltip,
 *            always, so an icon button is never a mystery to a screen reader or a hover.
 *   moreBtn  the ⋯ that holds everything rare or destructive (rule 4). Ghost, so a column of
 *            them reads as one quiet affordance, not a toolbar per row.
 *
 * No handlers: a view answers through its delegated listener, addressing the button by the
 * data hook it passed in (SPEC §panel.toolchain). */
                                       
import { h } from "../h.js";
import { iconNode } from "./icon.js";

                          
                                        
                                                                                              
                
              
                 
                 
                     
                                                                                      
                   
 

export function btn(label        , o          = {})                    {
  return h("button", {
    type: "button",
    class: "btn" + (o.kind ? " " + o.kind : "") + (o.icon ? " with-ic" : ""),
    id: o.id, title: o.title, disabled: o.disabled, hidden: o.hidden, data: o.data,
  }, o.icon ? iconNode(o.icon) : null, label);
}

;                             
              
                 
                                                                           
                 
                                                                         
                    
                  
                     
                                                       
                   
 

export function iconBtn(icon        , label        , o              = {})                    {
  return h("button", {
    type: "button",
    class: "btn icon" + (o.ghost ? " ghost" : ""),
    id: o.id, title: o.title ?? label, disabled: o.disabled, hidden: o.hidden, data: o.data,
    aria: { label, pressed: o.pressed == null ? null : String(o.pressed) },
  }, iconNode(icon));
}

/** A ⋯ always opens a menu, so it says so (aria-haspopup): a screen reader announces the menu
 *  before the click, and every caller gets it without remembering to. */
export function moreBtn(label        , o                                                    = {})                    {
  const b = iconBtn("ellipsis", label, { id: o.id, data: o.data, hidden: o.hidden, ghost: true });
  b.setAttribute("aria-haspopup", "menu");
  return b;
}
