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
import type { AttrMap, HChild } from "../h.js";
import { h } from "../h.js";
import { copyBtn } from "./copy.js";
import { markdownNode } from "./markdown.js";
import { tag } from "./status.js";

export interface TranscriptTurn {
  /** Speaker identity is independent of the localized role label. */
  side?: "user" | "assistant";
  /** The role, already a word (the caller localizes it; it says who spoke). */
  role: HChild;
  /** The moment, at the head's right end; omitted when the column needs no clocks. */
  at?: HChild;
  /** The turn's whole text, as prose. */
  text?: string;
  /** The text is an answer the model wrote: render it as the markdown it is. */
  markdown?: boolean;
  /** The text is data (a tool's result, a log line): a monospace block. */
  mono?: boolean;
  /** Reasoning, not speech: the same text, dimmed. */
  dim?: boolean;
  /** Structured content instead of text: JSON blocks, a code view. */
  code?: HChild[];
  /** What the turn's Copy puts on the clipboard - the answer as the model wrote it, the words as
   *  the reader sent them: a quiet glyph closing the turn (.tc-acts). An answer's code blocks
   *  carry their own Copy whether or not the turn has one. */
  copy?: string;
  /** The caller's hooks on the turn node (an id for tests, data for listeners). */
  data?: AttrMap;
}

/** The turn's prose body alone, in the transcript's voice - plain text, monospace when
 *  it is data, dimmed when it is reasoning - for a view that shows the text without a
 *  turn's head (a trace step's opened body). [transcriptTurn] draws turns from it. */
export function transcriptText(text: string, o: { mono?: boolean; dim?: boolean } = {}): HTMLElement {
  const cls = ["tc-text"];
  if (o.mono) cls.push("mono");
  if (o.dim) cls.push("dim");
  return h("div", { class: cls.join(" ") }, text);
}

/** One turn: its one-word head and its content, nothing hidden. */
export function transcriptTurn(t: TranscriptTurn): HTMLElement {
  let body: HChild = null;
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
export function transcript(turns: TranscriptTurn[]): HTMLElement {
  return h("div", { class: "tc-col" }, ...turns.map(transcriptTurn));
}

/** A tool ask as a chip: the tool's name as a mono tag, its arguments as code beside
 * it - what the model wanted, readable at a glance. */
export function toolChip(name: string, args: HChild): HTMLElement {
  return h("div", { class: "tc-chip" }, tag(name, { mono: true }), h("code", null, args));
}
