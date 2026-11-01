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

/* ================================================================================================
   LLM Providers - the llm plugin's first page (#llm-providers), SPEC §llm.providers.

   Named endpoints, each with its wire (OpenAI chat completions or Anthropic messages), its key
   as a reference and the models it serves. Nothing else in the row holds a URL or invents a
   model: the embedding block and every agent name a provider and pick one of ITS models, so
   an endpoint or a model list changes in one place. The first chat model is what an agent
   asks when it picks none, the first embedding model the embedding tier's.

   A row says who uses it; a provider in use cannot be deleted until its users name another.
   Discover asks the SAVED provider for its own listing and offers the ids it does not list
   yet; Test asks it one prompt, unreported (a provider is no agent).
   ================================================================================================ */
import { $, targetEl, toast } from "../util.js";
import { fill, h } from "../h.js";
import { tk, tr, trn } from "../i18n.js";
import {
  btn, card, checkField, checkGrid, checkRow, closeSheet, editDetail, editLine, editList, editSub, emptyNode, failNote, field, formActions, formFold, hint, iconBtn, iconNode, kvRow,
  moreBtn, note, pair, paneBody, paneHead, popupMenu, row, section, sheet, showSheet, tag, valueBlock,
} from "../ui/index.js";
import { actionBtn } from "../ui/action.js";
import type {
  LlmModel, LlmModelKind, LlmModelRow, LlmModelsReply, LlmProviderEntry, LlmProviderRow, LlmRow, LlmStatus, LlmTestReply,
} from "../types/llm.js";
import { PROVIDER_NAME_RE, call, isError, kindWord, loadRow, loadStatus, putRow, refField, remoteHost, testNode } from "./llm-core.js";

let status = null as LlmStatus | null;
let painted = ""; // the drawn list's signature; a poll that changes nothing repaints nothing
let visit = 0; // bumped by mount and unmount: a late answer from an ended visit paints nothing

/** The server's caps (swiss-llm provider.rs): models per provider, characters per id and per
 *  name, and the token counts a model may state. */
const MODELS_MAX = 64;
const MODEL_ID_MAX = 200;
const MODEL_NAME_MAX = 80;
const WINDOW_MIN = 1024;
const OUTPUT_MIN = 256;
const TOKENS_MAX = 100_000_000;

type ModelDetails = Pick<LlmModel, "contextWindow" | "maxOutput" | "image" | "thinking">;
const chat = (id: string, name: string, o: ModelDetails = {}): LlmModel => ({ id, kind: "chat", name, ...o });
const embedding = (id: string, name: string): LlmModel => ({ id, kind: "embedding", name });
/** The details most of a vendor's line shares, written once. */
const GPT6: ModelDetails = { contextWindow: 1_050_000, maxOutput: 128_000, image: true };
const CLAUDE: ModelDetails = { contextWindow: 1_000_000, maxOutput: 128_000, image: true };

/** What the Add sheet's preset fills: a local Ollama, the model vendors at their own API (a
 *  Chinese vendor at its China mainland host, whose keys are not the international site's) and
 *  OpenRouter, each named as the vendor writes itself and listing the models a fresh setup
 *  most likely asks, the everyday one first (the default). The ids are the vendor's wire ids
 *  as of 2026-10: DeepSeek V4.1 Flash is `deepseek-flash` on DeepSeek's own API. Kimi and
 *  MiniMax take the Anthropic wire, which carries their signed thinking back between tool
 *  calls; DeepSeek the OpenAI wire, whose reasoning replay is verified on its host. A key is a
 *  reference the operator then seals in the vault under the preset's key; Ollama needs none.
 *  Each model carries what its vendor states (its pages and OpenRouter's catalog, 2026-10): a
 *  count nobody states stays unset - Doubao's window, Kimi's output - rather than guessed, and
 *  Qwen3.8 Flash and MiniMax M3 take no thinking effort. */
const PRESETS: Record<string, LlmProviderEntry & { name: string }> = {
  custom: { name: "", style: "openai", baseUrl: "" },
  ollama: {
    name: "Ollama", style: "openai", baseUrl: "http://127.0.0.1:11434/v1",
    models: [chat("qwen3.5:4b", "Qwen3.5 4B"), embedding("qwen3-embedding:0.6b", "Qwen3 Embedding 0.6B")],
  },
  openai: {
    name: "OpenAI", style: "openai", baseUrl: "https://api.openai.com/v1", apiKey: "${secret://openai}",
    models: [chat("gpt-6.1-terra", "GPT-6.1 Terra", GPT6), chat("gpt-6-astra", "GPT-6 Astra", GPT6), chat("gpt-6.1-sol", "GPT-6.1 Sol", GPT6),
      chat("gpt-6-luna", "GPT-6 Luna", GPT6), embedding("text-embedding-3-small", "Text Embedding 3 Small")],
  },
  anthropic: {
    name: "Anthropic", style: "anthropic", baseUrl: "https://api.anthropic.com", apiKey: "${secret://anthropic}",
    models: [chat("claude-sonnet-5-5", "Claude Sonnet 5.5", CLAUDE), chat("claude-opus-5-5", "Claude Opus 5.5", CLAUDE),
      chat("claude-haiku-5-5", "Claude Haiku 5.5", CLAUDE), chat("claude-fable-5-1", "Claude Fable 5.1", CLAUDE)],
  },
  gemini: {
    name: "Gemini", style: "openai", baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai", apiKey: "${secret://gemini}",
    models: [chat("gemini-3.8-flash", "Gemini 3.8 Flash", { contextWindow: 1_048_576, maxOutput: 65_536, image: true }),
      chat("gemini-4-argon", "Gemini 4 Argon", { contextWindow: 1_048_576, image: true }), embedding("gemini-embedding-2", "Gemini Embedding 2")],
  },
  deepseek: {
    name: "DeepSeek", style: "openai", baseUrl: "https://api.deepseek.com", apiKey: "${secret://deepseek}",
    models: [chat("deepseek-flash", "DeepSeek V4.1 Flash", { contextWindow: 1_048_576, maxOutput: 393_216, image: true }),
      chat("deepseek-v4-pro", "DeepSeek V4 Pro", { contextWindow: 1_048_576, maxOutput: 393_216 })],
  },
  glm: {
    name: "GLM", style: "anthropic", baseUrl: "https://open.bigmodel.cn/api/anthropic", apiKey: "${secret://glm}",
    models: [chat("glm-5.3-flash", "GLM-5.3 Flash", { contextWindow: 1_048_576, maxOutput: 131_072, image: true }),
      chat("glm-5.3", "GLM-5.3", { contextWindow: 1_048_576, maxOutput: 131_072 })],
  },
  kimi: {
    name: "Kimi", style: "anthropic", baseUrl: "https://api.moonshot.cn/anthropic", apiKey: "${secret://kimi}",
    models: [chat("kimi-k3.1-pro", "Kimi K3.1 Pro", { contextWindow: 1_048_576, image: true }),
      chat("kimi-k3.1-pro-highspeed", "Kimi K3.1 Pro Highspeed", { contextWindow: 1_048_576, image: true }),
      chat("kimi-k3", "Kimi K3", { contextWindow: 1_048_576, image: true })],
  },
  qwen: {
    name: "Qwen", style: "openai", baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1", apiKey: "${secret://qwen}",
    models: [chat("qwen3.8-flash", "Qwen3.8 Flash", { contextWindow: 1_000_000, maxOutput: 131_072, image: true, thinking: false }),
      chat("qwen3.8-max", "Qwen3.8 Max", { contextWindow: 1_000_000, maxOutput: 131_072, image: true }),
      embedding("text-embedding-v4", "Text Embedding v4")],
  },
  minimax: {
    name: "MiniMax", style: "anthropic", baseUrl: "https://api.minimax.cn/anthropic", apiKey: "${secret://minimax}",
    models: [chat("MiniMax-M3.1-Flash", "MiniMax M3.1 Flash", { contextWindow: 1_048_576, maxOutput: 524_288, image: true }),
      chat("MiniMax-M3", "MiniMax M3", { contextWindow: 1_048_576, maxOutput: 512_000, image: true, thinking: false })],
  },
  doubao: {
    name: "Doubao", style: "openai", baseUrl: "https://ark.cn-beijing.volces.com/api/v3", apiKey: "${secret://doubao}",
    models: [chat("doubao-seed-2-1-lite-260915", "Doubao Seed 2.1 Lite", { image: true }),
      chat("doubao-seed-2-1-pro-260915", "Doubao Seed 2.1 Pro", { image: true })],
  },
  openrouter: {
    name: "OpenRouter", style: "openai", baseUrl: "https://openrouter.ai/api/v1", apiKey: "${secret://openrouter}",
    models: [
      chat("deepseek/deepseek-v4.1-flash", "DeepSeek V4.1 Flash", { contextWindow: 1_048_576, image: true }),
      chat("moonshotai/kimi-k3.1-pro", "Kimi K3.1 Pro", { contextWindow: 1_048_576, image: true }),
      chat("z-ai/glm-5.3", "GLM-5.3", { contextWindow: 1_048_576, maxOutput: 131_072 }),
      chat("qwen/qwen3.8-flash", "Qwen3.8 Flash", { contextWindow: 1_000_000, maxOutput: 131_072, image: true, thinking: false }),
      chat("minimax/minimax-m3", "MiniMax M3", { contextWindow: 1_048_576, maxOutput: 512_000, image: true, thinking: false }),
      chat("anthropic/claude-sonnet-5.5", "Claude Sonnet 5.5", CLAUDE),
      chat("openai/gpt-6.1-terra", "GPT-6.1 Terra", GPT6),
      chat("google/gemini-3.8-flash", "Gemini 3.8 Flash", { contextWindow: 1_048_576, maxOutput: 65_536, image: true }),
      embedding("openai/text-embedding-3-small", "Text Embedding 3 Small"),
    ],
  },
};
/** Every preset's model by id (the first preset listing it wins): what Fill known details and
 *  Discover's take fill a line with, so a provider saved before the details existed - or one
 *  made from Custom - gets them in a click. */
const KNOWN = new Map<string, LlmModel>();
for (const p of Object.values(PRESETS)) for (const m of p.models || []) if (!KNOWN.has(m.id)) KNOWN.set(m.id, m);
const PRESET_LABELS: Record<string, string> = {
  custom: tk("llm.providers.preset.custom"),
  ollama: tk("llm.providers.preset.ollama"),
  openai: tk("llm.providers.preset.openai"),
  anthropic: tk("llm.providers.preset.anthropic"),
  gemini: tk("llm.providers.preset.gemini"),
  deepseek: tk("llm.providers.preset.deepseek"),
  glm: tk("llm.providers.preset.glm"),
  kimi: tk("llm.providers.preset.kimi"),
  qwen: tk("llm.providers.preset.qwen"),
  minimax: tk("llm.providers.preset.minimax"),
  doubao: tk("llm.providers.preset.doubao"),
  openrouter: tk("llm.providers.preset.openrouter"),
};

function find(name: string | undefined): LlmProviderRow | undefined {
  return status ? status.providers.find((p) => p.name === name) : undefined;
}

/** Who names the provider: the embedding block, then each agent - by name. */
function consumers(name: string): string[] {
  if (!status) return [];
  const out: string[] = [];
  if (status.embedding && status.embedding.provider === name) out.push(tr("llm.providers.usedEmbedding"));
  for (const a of status.agents) if (a.provider === name) out.push(a.name);
  return out;
}

function usedCol(name: string): string {
  if (!status) return "";
  const parts: string[] = [];
  if (status.embedding && status.embedding.provider === name) parts.push(tr("llm.providers.usedEmbedding"));
  const n = status.agents.filter((a) => a.provider === name).length;
  if (n) parts.push(trn(n, "llm.providers.nAgents.one", "llm.providers.nAgents.other"));
  return parts.length ? parts.join(" · ") : tr("llm.providers.unused");
}

/** A token count as typed: digits, or a number with K (a thousand) or M (a million) - 1M,
 *  256K, 1.5M, 131072. Null when blank, NaN when it reads as no count. */
export function parseTokens(text: string): number | null {
  const t = text.trim().replace(/[\s,_]/g, "");
  if (!t) return null;
  const m = /^(\d+(?:\.\d+)?)([kKmM]?)$/.exec(t);
  if (!m || (!m[2] && m[1].includes("."))) return NaN;
  return Math.round(Number(m[1]) * (!m[2] ? 1 : m[2].toLowerCase() === "k" ? 1e3 : 1e6));
}

/** A stored count as the field shows it: K or M when that reads back as the same count
 *  (128000 is 128K), else the digits (131072), so a save never moves it. */
export function tokensField(n: number | null | undefined): string {
  if (n == null) return "";
  if (n % 1_000_000 === 0) return String(n / 1_000_000) + "M";
  if (n % 1000 === 0) return String(n / 1000) + "K";
  return String(n);
}

/** A count as a summary line says it, rounded: 1048576 and 1000000 both read 1M, 131072 128K
 *  (a power-of-two count in KiB steps, every other in thousands). */
export function tokensShort(n: number): string {
  const base = n % 1000 === 0 || n % 1024 !== 0 ? 1000 : 1024;
  const unit = (v: number, u: string): string => String(Math.round(v * 100) / 100) + u;
  if (n >= base * base) return unit(n / (base * base), "M");
  if (n >= base) return unit(n / base, "K");
  return String(n);
}

/** What a model's line says about it beyond its id: its name, then a chat model's window,
 *  output cap, image input and thinking. */
function modelFacts(m: LlmModel | LlmModelRow): string[] {
  const out: string[] = [];
  if (m.name) out.push(m.name);
  if (m.kind === "embedding") return out;
  if (m.contextWindow) out.push(tr("llm.providers.factWindow", { n: tokensShort(m.contextWindow) }));
  if (m.maxOutput) out.push(tr("llm.providers.factOutput", { n: tokensShort(m.maxOutput) }));
  if (m.image) out.push(tr("llm.providers.factImage"));
  out.push(tr(m.thinking === false ? "llm.providers.factNoThinking" : "llm.providers.factThinking"));
  return out;
}

/** The row's models, one line each: the id, then its kind and what it states, the first of
 *  each kind marked as the default. */
function modelRows(models: LlmModelRow[]): HTMLElement[] {
  const seen = new Set<LlmModelKind>();
  return models.map((m) => {
    const first = !seen.has(m.kind);
    seen.add(m.kind);
    return kvRow(m.id, [[kindWord(m.kind), ...modelFacts(m)].join(" · "), first ? [" ", tag(tr("llm.providers.default"))] : null]);
  });
}

function rowNode(p: LlmProviderRow): HTMLElement {
  const wire = p.style === "anthropic" ? tr("llm.style.anthropic") : tr("llm.style.openai");
  const chat = p.models.filter((m) => m.kind === "chat").length;
  const emb = p.models.length - chat;
  const counts = p.models.length
    ? [chat ? trn(chat, "llm.providers.nChat.one", "llm.providers.nChat.other") : null,
        emb ? trn(emb, "llm.providers.nEmbedding.one", "llm.providers.nEmbedding.other") : null].filter(Boolean).join(" · ")
    : tr("llm.providers.noModels");
  return row({
    name: p.name,
    sub: [wire, " · ", h("code", null, p.baseUrl),
      p.apiKey ? [" · ", h("code", null, p.apiKey)] : null,
      p.proxy ? " · " + tr("llm.providers.viaProxy") : null],
    // The endpoint (the sub line, which an opened row hides) and the models are the provider's
    // record: listed in place, a click away from the row.
    detail: [
      valueBlock({ label: tr("llm.providers.endpoint"), data: { lpblock: "endpoint" } },
        kvRow(tr("llm.providers.style"), wire),
        kvRow(tr("llm.providers.baseUrl"), h("code", null, p.baseUrl)),
        p.apiKey ? kvRow(tr("llm.providers.apiKey"), h("code", null, p.apiKey)) : null,
        p.proxy ? kvRow(tr("llm.providers.proxy"), h("code", null, p.proxy)) : null),
      p.models.length
        ? valueBlock({ label: tr("llm.providers.models"), data: { lpblock: "models" } }, modelRows(p.models))
        : valueBlock({ label: tr("llm.providers.models"), data: { lpblock: "models" }, text: tr("llm.providers.noModelsWhy") }),
    ],
    cols: [{ v: counts, w: "l", title: p.models.map((m) => m.id).join(", ") }, { v: usedCol(p.name), w: "l" }],
    primary: actionBtn("test", tr("llm.test"), { ghost: true, title: tr("llm.testHint"), data: { lptest: p.name } }),
    more: moreBtn(tr("llm.providers.actionsName", { name: p.name }), { data: { lpmore: p.name } }),
    data: { lprow: p.name },
  });
}

function signature(): string {
  if (!status) return "";
  return JSON.stringify([status.serving, status.providers, status.embedding && status.embedding.provider,
    status.agents.map((a) => a.provider)]);
}

function render(): void {
  const s = status;
  if (!s) return;
  painted = signature();
  fill($("pane"), paneBody({ wide: true },
    paneHead({
      desc: tr("llm.providers.desc"),
      sub: s.serving ? null : tr("llm.notServing"),
      actions: [actionBtn("new", tr("llm.providers.add"), { id: "lpAdd" })],
    }),
    section({},
      s.providers.length
        ? card(s.providers.map(rowNode))
        : emptyNode({ icon: "sparkles", title: tr("llm.providers.emptyTitle"), hint: tr("llm.providers.emptyHint"), action: tr("llm.providers.add") }))));
  $("pane").onclick = onClick;
  $("countChip").textContent = countText();
}

/** The sheet's model list: one line per model - its id, its kind, a mark on the first of each
 *  kind (the default), Details, Move up and Remove - and under it a quiet line of what the
 *  model states, which Details opens into its fields: the name a menu shows, and for a chat
 *  model its context window, its output cap, the inputs it reads and whether it thinks. Read
 *  back in order when the sheet saves. */
interface ModelsEditor {
  node: HTMLElement;
  add(m: LlmModel): void;
  ids(): Set<string>;
  /** The list to store, or the sentence that refuses it. */
  read(): LlmModel[] | { error: string };
  /** An Anthropic wire serves no embeddings: its kind picks offer chat only. */
  setAnthropic(on: boolean): void;
}

/** One line's detail fields, found by their marks. */
interface DetailInputs { name: HTMLInputElement; window: HTMLInputElement; output: HTMLInputElement; image: HTMLInputElement; thinking: HTMLInputElement }

function detailInputs(line: HTMLElement): DetailInputs {
  const get = (mark: string): HTMLInputElement => line.querySelector<HTMLInputElement>("[data-" + mark + "]") as HTMLInputElement;
  return { name: get("lpname"), window: get("lpwindow"), output: get("lpoutput"), image: get("lpimage"), thinking: get("lpthinking") };
}

/** A line's details as the store keeps them: only what says something - a name, a stated
 *  count, image input, no thinking. Refused with the line's id when a count reads as none,
 *  sits out of bounds or the output passes the window. */
function readDetails(id: string, kind: LlmModelKind, d: DetailInputs): Omit<LlmModel, "id" | "kind"> | { error: string } {
  const out: Omit<LlmModel, "id" | "kind"> = {};
  const name = d.name.value.trim();
  if (name) out.name = name.slice(0, MODEL_NAME_MAX);
  if (kind === "embedding") return out;
  const window = parseTokens(d.window.value);
  const output = parseTokens(d.output.value);
  if (window != null && !(window >= WINDOW_MIN && window <= TOKENS_MAX)) return { error: tr("llm.providers.badWindow", { id }) };
  if (output != null && !(output >= OUTPUT_MIN && output <= TOKENS_MAX)) return { error: tr("llm.providers.badOutput", { id }) };
  if (window != null && output != null && output > window) {
    return { error: tr("llm.providers.outputPastWindow", { id, output: tokensField(output), window: tokensField(window) }) };
  }
  if (window != null) out.contextWindow = window;
  if (output != null) out.maxOutput = output;
  if (d.image.checked) out.image = true;
  if (!d.thinking.checked) out.thinking = false;
  return out;
}

function modelsEditor(initial: LlmModel[], anthropic: boolean, extra: HTMLElement | null): ModelsEditor {
  // A line is the id, its kind, the default's mark, Details, Move up and Remove: five cells
  // after the id, the details box under them across the line.
  const list = editList({ id: "lpModels", cols: 5 });
  const empty = hint(tr("llm.providers.modelsEmpty"), { id: "lpModelsEmpty" });
  let noEmbedding = anthropic;

  const lines = (): HTMLElement[] => Array.from(list.querySelectorAll<HTMLElement>("[data-lpmodel]"));
  const kindOf = (line: HTMLElement): LlmModelKind =>
    (line.querySelector<HTMLSelectElement>("select")?.value === "embedding" ? "embedding" : "chat");
  const idOf = (line: HTMLElement): string => (line.querySelector<HTMLInputElement>("input")?.value || "").trim();
  const knownOf = (line: HTMLElement): LlmModel | undefined => {
    const m = KNOWN.get(idOf(line));
    return m && (m.kind || "chat") === kindOf(line) ? m : undefined;
  };

  // A line's quiet summary says what its fields hold, read as the store would keep it; a
  // count that reads as none says nothing until Save names it.
  const paintFacts = (line: HTMLElement): void => {
    const kind = kindOf(line);
    const d = detailInputs(line);
    const got = readDetails(idOf(line) || "?", kind, d);
    // Nothing stated (no name, no count, the default inputs and thinking): no line at all.
    const facts = isError(got) || !Object.keys(got).length ? [] : modelFacts({ id: "", kind, ...got });
    const sum = line.querySelector<HTMLElement>("[data-lpfacts]");
    const box = line.querySelector<HTMLElement>("[data-lpdetail]");
    if (sum) {
      sum.textContent = facts.join(" · ");
      sum.hidden = !facts.length || !box || !box.hidden;
    }
    const chatOnly = line.querySelector<HTMLElement>("[data-lpchatonly]");
    if (chatOnly) chatOnly.hidden = kind === "embedding";
  };

  // The marks, the first line's Move up and Fill known details follow every change to the
  // order, an id or a kind.
  const paint = (): void => {
    const seen = new Set<LlmModelKind>();
    lines().forEach((line, i) => {
      const kind = kindOf(line);
      const first = idOf(line) !== "" && !seen.has(kind);
      if (first) seen.add(kind);
      const mark = line.querySelector<HTMLElement>("[data-lpmark]");
      if (mark) fill(mark, first ? tag(tr("llm.providers.default"), { title: tr(kind === "embedding" ? "llm.providers.defaultEmbeddingWhy" : "llm.providers.defaultChatWhy") }) : null);
      const up = line.querySelector<HTMLButtonElement>("[data-lpmodelup]");
      if (up) up.disabled = i === 0;
      paintFacts(line);
    });
    empty.hidden = lines().length > 0;
    knownBtn.disabled = !lines().some((l) => knownOf(l));
  };

  const setOpen = (line: HTMLElement, open: boolean): void => {
    const box = line.querySelector<HTMLElement>("[data-lpdetail]");
    const more = line.querySelector<HTMLButtonElement>("[data-lpmodelmore]");
    if (box) box.hidden = !open;
    if (more) {
      more.setAttribute("aria-expanded", String(open));
      fill(more, iconNode(open ? "chevron-up" : "chevron-down"));
    }
    paintFacts(line);
  };

  const putDetails = (line: HTMLElement, m: LlmModel): void => {
    const d = detailInputs(line);
    d.name.value = m.name || "";
    d.window.value = tokensField(m.contextWindow);
    d.output.value = tokensField(m.maxOutput);
    d.image.checked = !!m.image;
    d.thinking.checked = m.thinking !== false;
  };

  const line = (m: LlmModel): HTMLElement => {
    const id = h("input", {
      value: m.id, autocomplete: "off", spellcheck: false, maxLength: MODEL_ID_MAX,
      placeholder: tr("llm.providers.modelIdPh"), aria: { label: tr("llm.providers.modelId") },
    });
    const embedding = h("option", { value: "embedding", selected: m.kind === "embedding" }, tr("llm.kind.embedding"));
    embedding.disabled = noEmbedding && m.kind !== "embedding";
    const kind = h("select", { aria: { label: tr("llm.providers.modelKind") } },
      h("option", { value: "chat", selected: m.kind !== "embedding" }, tr("llm.kind.chat")), embedding);
    const more = iconBtn("chevron-down", tr("llm.providers.modelDetails"), { ghost: true, data: { lpmodelmore: "" } });
    const up = iconBtn("arrow-up", tr("llm.providers.moveUp"), { ghost: true, data: { lpmodelup: "" } });
    const rm = iconBtn("x", tr("llm.providers.removeModel"), { ghost: true, data: { lpmodelrm: "" } });
    const text = (mark: string, ph: string): HTMLInputElement =>
      h("input", { autocomplete: "off", spellcheck: false, placeholder: ph, data: { [mark]: "" } });
    const nameIn = text("lpname", tr("llm.providers.modelNamePh"));
    nameIn.maxLength = MODEL_NAME_MAX;
    const windowIn = text("lpwindow", "1M");
    const outputIn = text("lpoutput", "128K");
    const box = (mark: string): HTMLInputElement => h("input", { type: "checkbox", data: { [mark]: "" } });
    const textIn = h("input", { type: "checkbox", checked: true, disabled: true });
    const imageIn = box("lpimage");
    const thinkingIn = box("lpthinking");
    const detail = editDetail({ data: { lpdetail: "" } },
      field({ label: tr("llm.providers.modelName"), control: nameIn, hint: tr("llm.providers.modelNameWhy") }),
      h("div", { data: { lpchatonly: "" } },
        pair(
          field({ label: tr("llm.providers.contextWindow"), control: windowIn, hint: tr("llm.providers.contextWindowWhy") }),
          field({ label: tr("llm.providers.maxOutput"), control: outputIn, hint: tr("llm.providers.maxOutputWhy") })),
        pair(
          field({
            group: true, label: tr("llm.providers.inputTypes"),
            control: checkRow({ control: textIn, label: tr("llm.providers.inputText") }, { control: imageIn, label: tr("llm.providers.inputImage") }),
          }),
          field({
            group: true, label: tr("llm.providers.thinking"), hint: tr("llm.providers.thinkingWhy"),
            control: checkRow({ control: thinkingIn, label: tr("llm.providers.thinkingOn") }),
          }))));
    const node = editLine({ data: { lpmodel: "" } }, id, kind, h("span", { data: { lpmark: "" } }), more, up, rm,
      editSub({ data: { lpfacts: "" } }), detail);
    putDetails(node, m);
    id.oninput = paint;
    kind.onchange = paint;
    for (const input of [nameIn, windowIn, outputIn]) input.oninput = (): void => { paintFacts(node); };
    for (const input of [imageIn, thinkingIn]) input.onchange = (): void => { paintFacts(node); };
    more.onclick = (): void => { setOpen(node, detail.hidden === true); };
    up.onclick = (): void => {
      const prev = node.previousElementSibling;
      if (prev) list.insertBefore(node, prev);
      paint();
      up.focus();
    };
    rm.onclick = (): void => {
      const next = (node.nextElementSibling || node.previousElementSibling) as HTMLElement | null;
      node.remove();
      paint();
      (next?.querySelector("input") || addBtn).focus();
    };
    return node;
  };

  const addBtn = btn(tr("llm.providers.addModel"), { id: "lpAddModel", icon: "plus" });
  // A known id's details as its vendor states them, over what the line holds: a provider
  // saved before lines had details, or made from Custom, gets them in one click.
  const knownBtn = btn(tr("llm.providers.fillKnown"), { id: "lpFillKnown", kind: "ghost", title: tr("llm.providers.fillKnownWhy") });
  addBtn.onclick = (): void => {
    const node = line({ id: "", kind: "chat" });
    list.appendChild(node);
    // A new line opens on its fields: nothing is filled in yet to summarise.
    setOpen(node, true);
    paint();
    node.querySelector("input")?.focus();
  };
  knownBtn.onclick = (): void => {
    for (const l of lines()) {
      const m = knownOf(l);
      if (m) putDetails(l, m);
    }
    paint();
  };
  for (const m of initial) list.appendChild(line(m));
  paint();

  return {
    node: h("div", null, list, empty, formActions(addBtn, extra, knownBtn)),
    add(m: LlmModel): void { list.appendChild(line(m)); paint(); },
    ids(): Set<string> { return new Set(lines().map(idOf).filter(Boolean)); },
    read(): LlmModel[] | { error: string } {
      const out: LlmModel[] = [];
      for (const l of lines()) {
        const id = idOf(l);
        if (!id) continue; // a blank line is one the operator added and left: nothing to store
        const kind = kindOf(l);
        if (out.some((m) => m.id === id)) return { error: tr("llm.providers.modelTwice", { id }) };
        if (noEmbedding && kind === "embedding") return { error: tr("llm.providers.anthropicEmbedding", { id }) };
        const details = readDetails(id, kind, detailInputs(l));
        if (isError(details)) { setOpen(l, true); return details; }
        out.push({ id, kind, ...details });
      }
      if (out.length > MODELS_MAX) return { error: tr("llm.providers.tooManyModels", { n: out.length, max: MODELS_MAX }) };
      return out;
    },
    setAnthropic(on: boolean): void {
      noEmbedding = on;
      for (const l of lines()) {
        const opt = l.querySelector<HTMLOptionElement>("option[value=embedding]");
        // A line already set to embedding keeps its pick, so Save names it instead of the
        // list quietly turning it into a chat model.
        if (opt) opt.disabled = on && kindOf(l) !== "embedding";
      }
      paint();
    },
  };
}

/** What the row's users need from the provider's new list: a model one of them picks by id
 *  stays listed with its kind, and one that takes the default keeps a model of its kind. The
 *  server refuses the same; checked here so the refusal names who is in the way. */
function listConflict(r: LlmRow, name: string, list: LlmModel[]): string | null {
  const has = (kind: LlmModelKind, id?: string): boolean =>
    list.some((m) => (m.kind || "chat") === kind && (id == null || m.id === id));
  const picked = new Map<string, string[]>();
  const defaults: Record<LlmModelKind, string[]> = { chat: [], embedding: [] };
  const need = (user: string, kind: LlmModelKind, id: string | undefined): void => {
    if (!id) defaults[kind].push(user);
    else if (!has(kind, id)) picked.set(id, [...(picked.get(id) || []), user]);
  };
  const e = r.embedding;
  if (e && e.provider === name) need(tr("llm.providers.usedEmbedding"), "embedding", e.model);
  for (const [n, a] of Object.entries(r.agents || {})) {
    const rt = a.runtime || "builtin";
    if ((rt === "builtin" || rt === "chat") && a.provider === name) need(n, "chat", a.model);
  }
  for (const [id, users] of picked) return tr("llm.providers.modelInUse", { id, users: users.join(", ") });
  if (defaults.chat.length && !has("chat")) return tr("llm.providers.needChat", { users: defaults.chat.join(", ") });
  if (defaults.embedding.length && !has("embedding")) return tr("llm.providers.needEmbedding", { users: defaults.embedding.join(", ") });
  return null;
}

/** Discover's answer: the ids the provider lists that the sheet does not, as checks to take. */
function foundNode(r: LlmModelsReply, ed: ModelsEditor, out: HTMLElement): HTMLElement {
  if (!r.ok) return hint(tr("llm.providers.discoverFailed", { url: r.url || "", error: r.error || "" }), { bad: true });
  const all = r.models || [];
  const have = ed.ids();
  const fresh = all.filter((m) => !have.has(m.id));
  if (!fresh.length) return hint(tr("llm.providers.discoverNoNew", { n: all.length, url: r.url || "" }));
  const checks = fresh.map((m) => checkField({
    label: [h("code", null, m.id), m.kind === "embedding" ? [" ", tag(tr("llm.kind.embedding"))] : null],
    control: h("input", { type: "checkbox", data: { lpfound: m.id, kind: m.kind } }),
  }));
  const take = btn(tr("llm.providers.discoverTake"), { id: "lpFoundTake", kind: "primary" });
  const drop = btn(tr("llm.providers.discoverDrop"), { id: "lpFoundDrop", kind: "ghost" });
  take.onclick = (): void => {
    const picked = Array.from(out.querySelectorAll<HTMLInputElement>("[data-lpfound]")).filter((c) => c.checked);
    for (const c of picked) {
      const id = c.dataset.lpfound || "";
      const kind: LlmModelKind = c.dataset.kind === "embedding" ? "embedding" : "chat";
      // A model a preset lists comes with what its vendor states.
      const known = KNOWN.get(id);
      ed.add(known && (known.kind || "chat") === kind ? known : { id, kind });
    }
    fill(out, picked.length ? hint(trn(picked.length, "llm.providers.discoverTook.one", "llm.providers.discoverTook.other")) : null);
    out.hidden = !picked.length;
  };
  drop.onclick = (): void => { fill(out, null); out.hidden = true; };
  // The finds under their own head, its two buttons in the head: a long listing scrolls in its
  // box and the buttons stay in view.
  return section({
    cap: tr("llm.providers.discoverCap"),
    note: tr("llm.providers.discoverFound", { n: all.length, fresh: fresh.length, url: r.url || "", ms: r.ms ?? 0 }),
    tools: [drop, take],
  }, checkGrid({ id: "lpFoundList" }, checks));
}

/** The Add/Edit sheet over the stored entry (never the status's resolved copy). The name
 *  keys the config block, so a rename is a RE-KEY: the save writes the entry under the new
 *  name, drops the old key, and re-points every consumer that names it (agents, the
 *  embedding tier) in the same CAS write - the block never passes through a dangling
 *  reference, and the operator never re-edits three consumers by hand. */
async function openSheet(name: string | null): Promise<void> {
  const got = await loadRow();
  if (!got.data) { toast(got.err, true); return; }
  const whole = got.data;
  const stored = whole.providers || {};
  const entry: LlmProviderEntry = (name && stored[name]) || { style: "openai", baseUrl: "" };

  const preset = h("select", { id: "lpPreset" },
    Object.keys(PRESETS).map((k) => h("option", { value: k }, tr(PRESET_LABELS[k]))));
  const nameIn = h("input", { id: "lpName", value: name || "", autocomplete: "off", spellcheck: false, placeholder: tr("llm.providers.namePh") });
  const style = h("select", { id: "lpStyle" },
    h("option", { value: "openai", selected: entry.style !== "anthropic" }, tr("llm.style.openai")),
    h("option", { value: "anthropic", selected: entry.style === "anthropic" }, tr("llm.style.anthropic")));
  const base = h("input", { id: "lpBase", value: entry.baseUrl, autocomplete: "off", spellcheck: false, placeholder: tr("llm.providers.baseUrlPh") });
  const key = h("input", { id: "lpKey", value: entry.apiKey || "", autocomplete: "off", spellcheck: false, placeholder: tr("llm.providers.apiKeyPh") });
  const proxy = h("input", { id: "lpProxy", value: entry.proxy || "", autocomplete: "off", spellcheck: false, placeholder: tr("llm.providers.proxyPh") });
  const proxyFold = formFold({ summary: tr("llm.providers.proxy"), id: "lpProxyFold" },
    field({ label: tr("llm.providers.proxy"), control: proxy, hint: tr("llm.providers.proxyWhy") }));
  if (entry.proxy) proxyFold.setAttribute("open", "");
  // Discover asks the SAVED provider: what it lists at the stored URL with the stored key.
  const discover = name ? btn(tr("llm.providers.discover"), { id: "lpDiscover", icon: "search", kind: "ghost" }) : null;
  const models = modelsEditor(entry.models || [], entry.style === "anthropic", discover);
  // Discover's answer; hidden while there is none, so it takes no grid step in the sheet.
  const found = h("div", { id: "lpFound", hidden: true });
  const privacy = hint("", { id: "lpRemote", hidden: true });
  const err = hint("", { id: "lpErr", bad: true, hidden: true, live: true });
  const say = (text: string): void => { err.textContent = text; err.hidden = !text; };
  const cancel = btn(tr("llm.cancel"), { id: "lpCancel" });
  const save = btn(name ? tr("llm.save") : tr("llm.add"), { id: "lpSave", kind: "primary" });

  const paintPrivacy = (): void => {
    const host = remoteHost(base.value);
    privacy.hidden = !host;
    privacy.textContent = host ? tr("llm.providers.remote", { host }) : "";
  };
  showSheet(sheet({
    title: name ? tr("llm.providers.editName", { name }) : tr("llm.providers.addTitle"),
    body: [
      name ? null : field({ label: tr("llm.providers.preset"), control: preset, hint: tr("llm.providers.presetWhy") }),
      pair(
        field({ label: tr("llm.name"), control: nameIn, required: true, hint: name ? tr("llm.providers.renameWhy") : tr("llm.providers.nameWhy") }),
        field({ label: tr("llm.providers.style"), control: style, hint: tr("llm.providers.styleWhy") })),
      field({ label: tr("llm.providers.baseUrl"), control: base, required: true, hint: tr("llm.providers.baseUrlWhy") }),
      privacy,
      field({ label: tr("llm.providers.apiKey"), control: key, hint: tr("llm.providers.apiKeyWhy") }),
      field({
        group: true, label: tr("llm.providers.models"), control: models.node,
        hint: name ? tr("llm.providers.modelsWhy") : tr("llm.providers.modelsWhyNew"),
      }),
      found,
      proxyFold,
      err,
    ],
    foot: [h("span", { class: "grow" }), cancel, save],
  }));
  paintPrivacy();

  // A preset fills the wire and its models; the name follows it until the operator types their own.
  let presetName = "";
  preset.onchange = (): void => {
    const p = preset.value in PRESETS ? PRESETS[preset.value] : PRESETS.custom;
    if (!nameIn.value.trim() || nameIn.value.trim() === presetName) nameIn.value = p.name;
    presetName = p.name;
    style.value = p.style;
    base.value = p.baseUrl;
    key.value = p.apiKey || "";
    proxy.value = "";
    const list = models.node.querySelector<HTMLElement>("#lpModels");
    if (list) fill(list, null);
    models.setAnthropic(p.style === "anthropic");
    for (const m of p.models || []) models.add(m);
    paintPrivacy();
  };
  style.onchange = (): void => { models.setAnthropic(style.value === "anthropic"); };
  base.oninput = paintPrivacy;
  cancel.onclick = closeSheet;
  if (discover && name) {
    discover.onclick = async (): Promise<void> => {
      discover.disabled = true;
      found.hidden = false;
      fill(found, note(tr("llm.providers.discovering"), { busy: true }));
      const r = await call<LlmModelsReply>("POST", "/api/llm/providers/models", { provider: name });
      discover.disabled = false;
      fill(found, r.data ? foundNode(r.data, models, found) : hint(r.err, { bad: true }));
    };
  }
  const onSave = async (): Promise<void> => {
    say("");
    const n = nameIn.value.trim();
    if (!PROVIDER_NAME_RE.test(n)) { say(tr("llm.providers.badName")); return; }
    // A taken name refuses both an add and a rename; keeping the same name is the
    // plain edit the sheet always did. A name apart from another only in case is taken too
    // (GLM beside glm reads as one in a model menu, and the server refuses the pair); glm's
    // own rename to GLM is not.
    const clash = Object.keys(stored).find((k) => k !== name && k.toLowerCase() === n.toLowerCase());
    if (clash) { say(tr("llm.providers.taken", { name: clash })); return; }
    if (!base.value.trim()) { say(tr("llm.providers.needBase")); return; }
    const k = refField(key.value, tr("llm.providers.apiKey"));
    if (isError(k)) { say(k.error); return; }
    const list = models.read();
    if (isError(list)) { say(list.error); return; }
    const conflict = name ? listConflict(whole, n, list) : null;
    if (conflict) { say(conflict); return; }
    const next: LlmProviderEntry = { style: style.value === "anthropic" ? "anthropic" : "openai", baseUrl: base.value.trim() };
    if (list.length) next.models = list;
    if (k) next.apiKey = k;
    if (proxy.value.trim()) next.proxy = proxy.value.trim();
    save.disabled = true;
    try {
      const failure = await putRow((r) => {
        // The rename case: the old key leaves, the entry lands under the new one, and
        // every consumer that named the old key is re-pointed in the same write.
        const providers: Record<string, LlmProviderEntry> = {};
        for (const [k, v] of Object.entries(r.providers || {})) {
          if (name && k === name && n !== name) continue;
          providers[k] = v;
        }
        providers[n] = next;
        r.providers = providers;
        if (name && n !== name) {
          for (const a of Object.values(r.agents || {})) {
            if (a.provider === name) a.provider = n;
          }
          if (r.embedding && r.embedding.provider === name) r.embedding.provider = n;
        }
      });
      if (failure) { say(failure); return; }
      closeSheet();
      toast(tr("llm.providers.saved", { name: n }));
      await refresh();
    } finally {
      save.disabled = false;
    }
  };
  save.onclick = (): void => { void onSave(); };
  (name ? base : nameIn).focus();
}

/** One prompt to the saved provider through one of its chat models: the answer, the model that
 *  gave it, how long it took. */
function openTest(p: LlmProviderRow): void {
  const chats = p.models.filter((m) => m.kind === "chat");
  const prompt = h("textarea", { id: "lpTestPrompt", rows: 3, placeholder: tr("llm.test.promptPh") });
  const model = h("select", { id: "lpTestModel" }, chats.map((m) => h("option", { value: m.id }, m.id)));
  const out = h("div", { id: "lpTestOut" });
  const close = btn(tr("llm.close"), { id: "lpTestClose" });
  const run = btn(tr("llm.test.run"), { id: "lpTestRun", kind: "primary", disabled: !chats.length });
  showSheet(sheet({
    title: tr("llm.providers.testName", { name: p.name }),
    body: [
      hint(tr("llm.providers.testWhy", { url: p.url })),
      field({ label: tr("llm.test.prompt"), control: prompt }),
      chats.length
        ? field({ label: tr("llm.test.model"), control: model })
        : hint(tr("llm.providers.testNeedsModel"), { bad: true }),
      out,
    ],
    foot: [h("span", { class: "grow" }), close, run],
  }));
  close.onclick = closeSheet;
  const onRun = async (): Promise<void> => {
    run.disabled = true;
    fill(out, note(tr("llm.test.asking"), { busy: true }));
    const body: Record<string, string> = { provider: p.name, model: model.value };
    if (prompt.value.trim()) body.prompt = prompt.value.trim();
    const r = await call<LlmTestReply>("POST", "/api/llm/providers/test", body);
    run.disabled = false;
    fill(out, r.data ? testNode(r.data) : hint(r.err, { bad: true }));
  };
  run.onclick = (): void => { void onRun(); };
  (chats.length ? run : close).focus();
}

async function remove(name: string): Promise<void> {
  const users = consumers(name);
  if (users.length) { toast(tr("llm.providers.inUse", { name, users: users.join(", ") }), true); return; }
  if (!confirm(tr("llm.providers.deleteConfirm", { name }))) return;
  const failure = await putRow((r) => {
    r.providers = Object.fromEntries(Object.entries(r.providers || {}).filter(([k]) => k !== name));
  });
  if (failure) { toast(failure, true); return; }
  toast(tr("llm.providers.deleted", { name }));
  await refresh();
}

function onClick(event: MouseEvent): void {
  const t = targetEl(event);
  if (!t) return;
  if (t.closest("#lpAdd") || t.closest("[data-empty-action]")) { void openSheet(null); return; }
  const test = t.closest<HTMLElement>("[data-lptest]");
  if (test) {
    const p = find(test.dataset.lptest);
    if (p) openTest(p);
    return;
  }
  const more = t.closest<HTMLElement>("[data-lpmore]");
  if (more) {
    // The opening click must not reach document (the menu closes on outside clicks).
    event.stopPropagation();
    const name = more.dataset.lpmore || "";
    popupMenu(more.getBoundingClientRect(), [
      { label: tr("llm.edit"), fn: () => { void openSheet(name); } },
      { sep: true },
      { label: tr("llm.delete"), danger: true, fn: () => { void remove(name); } },
    ]);
  }
}

export async function mount(): Promise<void> {
  visit++;
  painted = "";
  const at = visit;
  const r = await loadStatus();
  if (at !== visit) return;
  if (!r.data) {
    fill($("pane"), paneBody({ wide: true }, failNote({ text: tr("llm.loadFailed"), why: r.err })));
    return;
  }
  status = r.data;
  render();
}

export function unmount(): void { visit++; }

export async function refresh(): Promise<void> {
  const at = visit;
  const r = await loadStatus();
  if (at !== visit || !r.data) return; // keep the last paint; the next poll asks again
  status = r.data;
  if (signature() !== painted) render();
}

export async function poll(): Promise<void> { await refresh(); }

export function countText(): string {
  const n = status ? status.providers.length : 0;
  return n ? trn(n, "llm.providers.n.one", "llm.providers.n.other") : "";
}
