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

/* The conversation column (SPEC §panel.ui), shared by live and recorded dialogues.
 * The user's words sit at the right on a quiet surface; the assistant's prose and
 * tool flow start at the left. Callers may fold context, thinking and tool data;
 * this shape owns the reading rhythm and the speaker's position. */
                                               
import { h } from "../h.js";
import { tr } from "../i18n.js";
import { copyBtn } from "./copy.js";
import { iconNode } from "./icon.js";
import { markdownNode } from "./markdown.js";
import { tag } from "./status.js";

                                 
                                                                     
                              
                                                                                                    
               
                                                                                      
              
                                         
                
                                                                                
                     
                                                                           
                 
                                                      
                
                                                                      
                  
                                                                                                 
                                                                                              
                                                              
                
                                                                                   
                 
 

/** The turn's prose body alone, in the transcript's voice - plain text, monospace when
 *  it is data, dimmed when it is reasoning - for a view that shows the text without a
 *  turn's head (a trace step's opened body). [transcriptTurn] draws turns from it. */
export function transcriptText(text        , o                                    = {})              {
  const cls = ["tc-text"];
  if (o.mono) cls.push("mono");
  if (o.dim) cls.push("dim");
  return h("div", { class: cls.join(" ") }, text);
}

/** Who spoke, as a mark at a turn's head: a person for the reader, a robot for the model
 *  (2026-11-04: the USER/ASSISTANT words read as labels, not speakers). The localized word
 *  stays the mark's accessible name and its tooltip. */
export function speaker(who                      )              {
  const word = tr(who === "user" ? "llm.history.role.user" : "llm.history.role.assistant");
  return h("span", { class: "tc-who " + who, title: word }, iconNode(who === "user" ? "user" : "bot", word));
}

/** One turn: its one-word head and its content, nothing hidden. */
export function transcriptTurn(t                )              {
  let body         = null;
  if (t.code && t.code.length > 0) {
    body = h("div", { class: "tc-code" }, ...t.code);
  } else if (t.text != null && t.text !== "") {
    body = t.markdown ? markdownNode(t.text, { class: "tc-md", copy: true }) : transcriptText(t.text, { mono: t.mono, dim: t.dim });
  }
  return h("div", { class: "tc-turn " + (t.side || "assistant"), ...(t.data || {}) },
    h("div", { class: "tc-head" },
      h("span", { class: "tc-role" }, t.role),
      t.at == null ? null : h("span", { class: "tc-at" }, t.at)),
    body,
    t.copy ? h("div", { class: "tc-acts" }, copyBtn(t.copy)) : null);
}

/** The whole conversation, oldest first - the order it happened in. */
export function transcript(turns                  )              {
  return h("div", { class: "tc-col" }, ...turns.map(transcriptTurn));
}

/** A tool ask as a chip: the tool's name as a mono tag, its arguments as code beside
 * it - what the model wanted, readable at a glance. */
export function toolChip(name        , args        )              {
  return h("div", { class: "tc-chip" }, tag(name, { mono: true }), h("code", null, args));
}
