/* Copyright 2026 young1lin
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

/** ==============================================================================================
 * Memory - the ask sheet (SPEC §memory.rag): one question answered from the project's
 * notes by the agent its layers name. Opened from the notes toolbar's Ask button, which
 * memory.ts places - this module only draws and runs the sheet.
 * ================================================================================================ */
import { fill, h } from "../h.js";
import { tr } from "../i18n.js";
import { btn, closeSheet, field, form, markdownNode, sheet, showSheet } from "../ui/index.js";
import type { LlmAgentRow } from "../types/llm.js";
import type { MemAskReply } from "../types/memory.js";
import { loadStatus, runtimeWord, taskAgents } from "./llm-core.js";
import { errMessage, req } from "./memory-core.js";

/** The ask sheet over one project: the question, the agent to answer it (left out, the
 *  layers decide), and the answer naming the note ids it rests on. */
export async function openAsk(projectId: string, projectName: string): Promise<void> {
  // The agents to pick from (a db agent serves the Data page alone); the llm plugin off (or
  // not answering) leaves the choice to the layers and the field says why.
  const llm = await loadStatus();
  const agents: LlmAgentRow[] = taskAgents(llm.data);
  const question = h("textarea", { id: "memAskQ", rows: 3,
    placeholder: tr("memory.ask.placeholder"), aria: { label: tr("memory.ask") } }) as HTMLTextAreaElement;
  const agent = h("select", { id: "memAskAgent", aria: { label: tr("memory.ask.agent") } },
    h("option", { value: "" }, tr("memory.ask.inherit")),
    agents.map((a) => h("option", { value: a.name }, a.name + " · " + runtimeWord(a.runtime))));
  agent.value = "";
  const say = h("p", { id: "memAskErr", class: "err" });
  say.hidden = true;
  const answer = h("div", { id: "memAskAnswer" });
  const meta = h("p", { id: "memAskMeta", class: "quiet" });
  meta.hidden = true;
  const ask = btn(tr("memory.ask.run"), { id: "memAskRun", kind: "primary" });
  const close = btn(tr("memory.close"));
  const body = form([
    field({ label: tr("memory.ask"), control: question, hint: tr("memory.ask.why") }),
    field({ label: tr("memory.ask.agent"), control: agent,
      hint: llm.data ? tr("memory.ask.agentWhy") : (llm.err || tr("memory.ask.agentWhy")) }),
    say,
    answer,
    meta,
  ]);
  showSheet(sheet({
    title: tr("memory.ask.title", { name: projectName }),
    body,
    foot: [ask, h("span", { class: "grow" }), close],
  }));
  question.focus();
  close.onclick = (): void => closeSheet();
  const onAsk = async (): Promise<void> => {
    say.hidden = true;
    const text = question.value.trim();
    if (!text) {
      say.textContent = tr("memory.ask.empty");
      say.hidden = false;
      return;
    }
    ask.disabled = true;
    fill(answer, []);
    meta.hidden = true;
    try {
      const body: Record<string, unknown> = { project: projectId, question: text };
      if (agent.value) body.agent = agent.value;
      const r = await req<MemAskReply>("POST", "/ask", body);
      if (!r.ok || !r.data) {
        say.textContent = errMessage(r.err);
        say.hidden = false;
        return;
      }
      // The answer names the note ids it rests on; plain text that reads best as Markdown.
      fill(answer, [h("div", { id: "memAskText" }, markdownNode(r.data.answer))]);
      const worked = r.data.mode === "tools";
      meta.textContent = worked
        ? tr("memory.ask.metaTools", { agent: r.data.agent })
        : tr("memory.ask.metaInline", { agent: r.data.agent });
      meta.hidden = false;
    } finally {
      ask.disabled = false;
    }
  };
  ask.onclick = (): void => { void onAsk(); };
}
