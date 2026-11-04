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

// @vitest-environment happy-dom

/* The llm plugin's three pages (SPEC §llm.panel): Providers, Embedding, Agents (with the
   Reporting section). A canned host answers the plugin row through the config API and derives
   GET /api/llm/status from it, the way the plugin does: every sheet prefills from the ROW,
   saves go GET-then-CAS-PUT, and a credential typed as a literal never leaves the browser. */

import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type * as ProvidersView from "../src/views/llm-providers.js";
import type * as EmbeddingView from "../src/views/llm-embedding.js";
import type * as AgentsView from "../src/views/llm-agents.js";

type Body = Record<string, unknown>;
type Row = {
  enabled?: boolean;
  providers?: Record<string, Body>;
  embedding?: Body;
  agents?: Record<string, Body>;
  telemetry?: Body;
};
interface Call { method: string; path: string; body: Body | null }
type Answer = [number, unknown];

const here = dirname(fileURLToPath(import.meta.url));

/* The shell skeleton from the SERVED index.html: the import graph wires shell controls at
 * evaluation time, so the ids exist before the import. */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

/* ------------------------------------------------------------------ the canned host */

let row: Row = {};
let revision = 1;
let calls: Call[] = [];
let telemetry: Body = {};
let confirmAnswer = true;
let override: ((c: Call) => Answer | undefined) | null = null;

const DEFAULT_PROMPT = "You distill a session into memory.";
const DB_PROMPT = "You are the database agent of swiss's Data page.";
/** Each runtime's tools and its prompt without one of the agent's own, as the plugin says. */
const CATALOG: Record<string, Array<{ name: string; summary: string }>> = {
  builtin: [{ name: "read", summary: "r" }, { name: "bash", summary: "b" }, { name: "sync", summary: "s" }, { name: "delegate", summary: "d" }, { name: "list", summary: "l" }, { name: "job_output", summary: "o" }, { name: "job_kill", summary: "k" }],
  chat: [],
  claude: [{ name: "Read", summary: "r" }, { name: "Grep", summary: "g" }, { name: "Glob", summary: "f" }],
  pi: [{ name: "read", summary: "r" }, { name: "grep", summary: "g" }, { name: "find", summary: "f" }, { name: "ls", summary: "l" }],
  db: [{ name: "list_tables", summary: "t" }, { name: "describe_table", summary: "d" }, { name: "run_sql", summary: "r" }, { name: "stage_sql", summary: "s" }],
};
const DEFAULT_PROMPTS: Record<string, string | null> = { builtin: DEFAULT_PROMPT, chat: DEFAULT_PROMPT, claude: null, pi: null, db: DB_PROMPT };

function initialRow(): Row {
  return {
    enabled: true,
    providers: {
      ollama: { style: "openai", baseUrl: "http://127.0.0.1:11434/v1", models: [{ id: "qwen3.5:4b" }, { id: "bge-m3", kind: "embedding" }] },
      glm: { style: "anthropic", baseUrl: "https://open.bigmodel.cn/api/anthropic", models: [{ id: "glm-5.3-flash", kind: "chat" }], apiKey: "${secret://glm}" },
    },
    embedding: { provider: "ollama", model: "bge-m3" },
    agents: {
      distiller: { runtime: "builtin", provider: "glm" },
      cc: { runtime: "claude", env: { ANTHROPIC_AUTH_TOKEN: "${secret://glm}" } },
    },
  };
}

function remote(url: string): boolean {
  return !/^https?:\/\/(127\.0\.0\.1|localhost)[:/]/.test(url);
}

/** A provider's models as the status shows them: kinds filled (`chat` when unset), every
 *  detail present (null, or the flag's default). */
function modelsOf(p: Body | undefined): Array<{ id: string; kind: string }> {
  const list = p && Array.isArray(p.models) ? p.models as Body[] : [];
  return list.map((m) => ({
    id: String(m.id), kind: typeof m.kind === "string" ? m.kind : "chat", name: m.name ?? null,
    contextWindow: m.contextWindow ?? null, maxOutput: m.maxOutput ?? null, image: m.image === true, thinking: m.thinking !== false,
  }));
}

function firstOf(p: Body | undefined, kind: string): string | null {
  const m = modelsOf(p).find((x) => x.kind === kind);
  return m ? m.id : null;
}

/** What GET /api/llm/status says over the row: defaults filled, models resolved. */
function status(): Body {
  const all = row.providers || {};
  const providers = Object.entries(all).map(([name, p]) => ({
    name, style: p.style, baseUrl: p.baseUrl, models: modelsOf(p), apiKey: p.apiKey ?? null, proxy: p.proxy ?? null,
    url: String(p.baseUrl) + (p.style === "anthropic" ? "/v1/messages" : "/chat/completions"),
  }));
  const e = row.embedding;
  const ep = e && e.provider !== "off" ? all[String(e.provider)] : undefined;
  const em = e ? e.model ?? firstOf(ep, "embedding") : null;
  const embedding = e && ep ? {
    provider: e.provider, model: em, batch: e.batch ?? 16, timeoutMs: e.timeoutMs ?? 30000,
    queryPrefix: e.queryPrefix ?? "", documentPrefix: e.documentPrefix ?? "", remote: remote(String(ep.baseUrl)),
    ...(e.dimensions ? { dimensions: e.dimensions } : {}), ...(e.minSimilarity != null ? { minSimilarity: e.minSimilarity } : {}),
    fingerprint: "openai:" + String(em) + ":native",
  } : null;
  const t = row.telemetry;
  const agents = Object.entries(row.agents || {}).map(([name, a]) => {
    const rt = String(a.runtime || "builtin");
    const asks = rt === "builtin" || rt === "chat" || rt === "db";
    const p = a.provider ? all[String(a.provider)] : undefined;
    const report = rt !== "pi" && a.report !== false;
    const names = CATALOG[rt].map((x) => x.name);
    const own = Array.isArray(a.tools) ? a.tools as string[] : null;
    return {
      name, runtime: rt, provider: a.provider ?? null, model: a.model ?? (asks ? firstOf(p, "chat") : null),
      timeoutMs: a.timeoutMs ?? 1_800_000, report, concurrency: a.concurrency ?? (asks ? 4 : 1),
      reports: !report || !t || t.enabled === false ? "off" : rt === "claude" ? "child" : "swiss",
      systemPrompt: a.systemPrompt ?? null, systemPromptDefault: DEFAULT_PROMPTS[rt],
      tools: own, toolsInForce: own ? names.filter((n) => own.includes(n)) : names,
      ...(rt === "claude" || rt === "pi" ? { command: a.command ?? rt, program: "C:/Users/me/.local/bin/" + rt + ".exe" } : {}),
      // As the server shows them: the resolved limits of a provider-asking runtime.
      ...(asks ? { maxTokens: a.maxTokens ?? 131072, maxContextKiB: a.maxContextKiB ?? 1024, effort: a.effort ?? null } : {}),
      ...(rt === "builtin" || rt === "db" ? { maxTurns: a.maxTurns ?? 24 } : {}),
      ...(rt === "builtin" ? { secrets: a.secrets ?? [],
        secretsInForce: ((a.secrets ?? []) as string[]).filter((g) => g !== "*" && !g.startsWith("group:")) } : {}),
    };
  });
  return {
    serving: true, providers, embedding, agents, catalog: CATALOG, defaultPrompts: DEFAULT_PROMPTS,
    telemetry: {
      enabled: !!t && t.enabled !== false, protocol: t ? t.protocol ?? "grpc" : null, endpoint: t ? t.endpoint : null,
      queued: 0, sent: 0, dropped: 0, rejected: 0, lastExportMs: null, lastError: null, lastErrorMs: null, ...telemetry,
    },
  };
}

function answer(c: Call): Answer {
  const mine = override ? override(c) : undefined;
  if (mine) return mine;
  const route = c.method + " " + c.path;
  if (route === "GET /api/llm/status") return [200, status()];
  if (route === "GET /api/plugins/llm/config") return [200, { revision, config: row, schema: {} }];
  if (route === "PUT /api/plugins/llm/config") {
    const b = c.body || {};
    if (b.revision !== revision) return [409, { error: "the config changed since it was read" }];
    row = structuredClone(b.config) as Row;
    revision++;
    return [200, { revision, config: row, schema: {}, warnings: [], applied: true }];
  }
  return [404, { error: "no canned answer for " + route }];
}

function sent(method: string, path: string): Call[] {
  return calls.filter((c) => c.method === method && c.path === path);
}
const puts = (): Body[] => sent("PUT", "/api/plugins/llm/config").map((c) => (c.body || {}).config as Body);

/* ------------------------------------------------------------------ the DOM side */

let providers: typeof ProvidersView;
let embedding: typeof EmbeddingView;
let agents: typeof AgentsView;

/** A body that yields the given server-sent event frames, for the live Test's
 * stream route: each frame is one `event:`/`data:` block as the wire sends it. */
function sseBody(frames: string[]): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  return new ReadableStream<Uint8Array>({
    start(controller) {
      for (const frame of frames) controller.enqueue(encoder.encode(frame));
      controller.close();
    },
  });
}

/** One server-sent event frame, spelled as the wire sends it. */
function sseFrame(event: string, data: unknown): string {
  return "event: " + event + "\ndata: " + JSON.stringify(data) + "\n\n";
}

/** A stream body the TEST drives: frames go out when push() says and the body ends
 * when end() says - for the live-dialogue tests that must stop mid-answer, watch a
 * late frame after a Stop, or die without a done. */
function liveBody(): { body: ReadableStream<Uint8Array>; push(event: string, data: unknown): void; end(): void } {
  const encoder = new TextEncoder();
  let ctl: ReadableStreamDefaultController<Uint8Array> | null = null;
  const body = new ReadableStream<Uint8Array>({ start(c) { ctl = c; }, cancel() { ctl = null; } });
  return {
    body,
    push: (event, data) => { ctl?.enqueue(encoder.encode(sseFrame(event, data))); },
    end: () => { ctl?.close(); ctl = null; },
  };
}

/** A held answer for the fetch shim: json() stays pending until the test resolves
 * it - the deferred-deterministic seam for restore races (the __sseLive sibling).
 * Each deferred is its own resolver; tests release what they hold in finally so
 * no dangling async crosses a test boundary. */
function deferred(): { promise: Promise<Record<string, unknown>>; resolve(v: Record<string, unknown>): void } {
  let settle: (v: Record<string, unknown>) => void = (): void => { /* replaced below */ };
  const promise = new Promise<Record<string, unknown>>((r) => { settle = r; });
  return { promise, resolve: (v) => settle(v) };
}

/** The AbortSignals the view handed fetch, with the path they rode on: the Stop
 * tests assert the loop's own request was aborted, not some other call's. */
let signals: Array<{ path: string; signal: AbortSignal }> = [];

beforeAll(async () => {
  Object.assign(globalThis, {
    fetch: (url: string, opts?: RequestInit): Promise<unknown> => {
      const raw = opts && typeof opts.body === "string" ? opts.body : null;
      const call: Call = { method: (opts && opts.method) || "GET", path: String(url), body: raw ? JSON.parse(raw) as Body : null };
      calls.push(call);
      const [status, json] = answer(call);
      if (opts && opts.signal) signals.push({ path: String(url), signal: opts.signal });
      // The live Test reads a streamed body: an answer carrying __sse frames
      // answers with a body that yields them, as the route's event stream does;
      // __sseLive hands over a stream the test drives itself.
      const frames = json && typeof json === "object" && Array.isArray((json as { __sse?: string[] }).__sse)
        ? (json as { __sse?: string[] }).__sse as string[]
        : null;
      const live = json && typeof json === "object" && Object.prototype.hasOwnProperty.call(json, "__sseLive")
        ? (json as { __sseLive?: ReadableStream<Uint8Array> }).__sseLive ?? null
        : null;
      const held = json && typeof json === "object" && Object.prototype.hasOwnProperty.call(json, "__defer")
        ? (json as { __defer?: Promise<Record<string, unknown>> }).__defer ?? null
        : null;
      return Promise.resolve(frames
        ? { status, ok: status >= 200 && status < 300, body: sseBody(frames) }
        : live
          ? { status, ok: status >= 200 && status < 300, body: live }
          : held
            ? { status, ok: status >= 200 && status < 300, json: () => held }
            : { status, ok: status >= 200 && status < 300, json: () => Promise.resolve(json) });
    },
    confirm: () => confirmAnswer,
  });
  document.body.innerHTML = shellSkeleton();
  providers = await import("../src/views/llm-providers.js");
  embedding = await import("../src/views/llm-embedding.js");
  agents = await import("../src/views/llm-agents.js");
});

beforeEach(() => {
  document.body.innerHTML = shellSkeleton();
  row = initialRow();
  revision = 1;
  calls = [];
  signals = [];
  telemetry = {};
  confirmAnswer = true;
  override = null;
});

afterEach(() => {
  providers.unmount();
  embedding.unmount();
  agents.unmount();
});

const settle = async (): Promise<void> => {
  for (let i = 0; i < 6; i++) await new Promise((r) => setTimeout(r, 0));
};

function q<T extends Element = HTMLElement>(sel: string): T {
  const el = document.querySelector<T>(sel);
  if (!el) throw new Error("nothing matches " + sel);
  return el;
}

function text(sel: string): string {
  const el = document.querySelector(sel);
  return el ? el.textContent || "" : "";
}

async function click(target: string | Element): Promise<void> {
  const el = typeof target === "string" ? q(target) : target;
  el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  await settle();
}

function type(sel: string, value: string): void {
  q<HTMLInputElement>(sel).value = value;
}

function change(sel: string, value: string, ev = "change"): void {
  const el = q<HTMLInputElement | HTMLSelectElement>(sel);
  el.value = value;
  el.dispatchEvent(new Event(ev, { bubbles: true }));
}

function check(sel: string, on: boolean): void {
  const el = q<HTMLInputElement>(sel);
  el.checked = on;
  el.dispatchEvent(new Event("change", { bubbles: true }));
}

async function menuPick(label: string): Promise<void> {
  const item = Array.from(document.querySelectorAll<HTMLButtonElement>(".menu.float button")).find((b) => b.textContent === label);
  if (!item) throw new Error("no menu item " + label);
  await click(item);
}

/** A select's options as [value, label]. */
function options(sel: string): string[][] {
  return [...q<HTMLSelectElement>(sel).options].map((o) => [o.value, o.textContent || ""]);
}

/** The provider sheet's model lines, as [id, kind, the default mark's text]. */
function modelLines(): string[][] {
  return [...document.querySelectorAll<HTMLElement>("#lpModels [data-lpmodel]")].map((l) => [
    l.querySelector<HTMLInputElement>("input")?.value ?? "",
    l.querySelector<HTMLSelectElement>("select")?.value ?? "",
    l.querySelector("[data-lpmark]")?.textContent ?? "",
  ]);
}

/** The n-th model line (1-based). */
const lineSel = (n: number): string => "#lpModels [data-lpmodel]:nth-child(" + String(n) + ")";

/* ------------------------------------------------------------------ Providers */

describe("the LLM plugin - Providers", () => {
  it("a row is the name, the wire and endpoint, the key as its reference, its models and who uses it", async () => {
    row.providers = { ...row.providers, bare: { style: "openai", baseUrl: "http://127.0.0.1:8000/v1" } };
    await providers.mount();
    const rows = [...document.querySelectorAll<HTMLElement>("[data-lprow]")];
    expect(rows.map((r) => r.dataset.lprow)).toEqual(["ollama", "glm", "bare"]);
    expect(text("[data-lprow=ollama] .lrow-sub")).toBe("OpenAI chat completions · http://127.0.0.1:11434/v1");
    expect(text("[data-lprow=glm] .lrow-sub")).toBe("Anthropic messages · https://open.bigmodel.cn/api/anthropic · ${secret://glm}");
    expect(text("[data-lprow=ollama]")).toContain("1 chat model · 1 embedding model");
    expect(text("[data-lprow=glm]")).toContain("1 chat model");
    // The row opens to the endpoint (its sub line, which opening hides) and the models, the
    // first of each kind marked as the default.
    const kv = (sel: string): Array<[string, string]> => [...document.querySelectorAll(sel + " .kv")]
      .map((k) => [k.querySelector(".kv-k")?.textContent ?? "", k.querySelector(".kv-v")?.textContent ?? ""]);
    expect(kv("[data-lprow=glm] [data-lpblock=endpoint]")).toEqual([
      ["Wire", "Anthropic messages"], ["Base URL", "https://open.bigmodel.cn/api/anthropic"], ["API key", "${secret://glm}"],
    ]);
    expect(kv("[data-lprow=ollama] [data-lpblock=endpoint]").map((r) => r[0])).toEqual(["Wire", "Base URL"]);
    // One line a model: its id, then its kind and what it states.
    expect(kv("[data-lprow=ollama] [data-lpblock=models]")).toEqual([["qwen3.5:4b", "Chat · thinking default"], ["bge-m3", "Embedding default"]]);
    expect(text("[data-lprow=bare]")).toContain("no models");
    expect(text("[data-lprow=bare]")).toContain("This provider lists no models");
    expect(text("[data-lprow=ollama]")).toContain("embedding");
    expect(text("[data-lprow=glm]")).toContain("1 agent");
    expect(text("#countChip")).toBe("3 providers");
  });

  it("no providers: the empty state's action opens the Add sheet", async () => {
    row = { enabled: true };
    await providers.mount();
    expect(text("#pane")).toContain("No providers yet");
    expect(text("#countChip")).toBe("");
    await click("[data-empty-action]");
    expect(text("#sheet h2")).toBe("Add provider");
  });

  it("a preset fills the sheet and its models; a pasted key never leaves the browser; Add merges the entry into the row", async () => {
    await providers.mount();
    await click("#lpAdd");
    // A new provider has nothing saved to ask: no Discover until it is.
    expect(document.querySelector("#lpDiscover")).toBe(null);
    expect(q("#lpModelsEmpty").hidden).toBe(false);
    // The vendors a fresh setup reaches for, each named as the vendor writes itself.
    expect([...document.querySelectorAll<HTMLOptionElement>("#lpPreset option")].map((o) => o.textContent)).toEqual([
      "Custom", "Ollama (local)", "OpenAI", "Anthropic (Claude)", "Google Gemini", "DeepSeek", "GLM (Zhipu, Anthropic endpoint)",
      "Kimi (Moonshot, Anthropic endpoint)", "Qwen (Alibaba Cloud Model Studio)", "MiniMax (Anthropic endpoint)",
      "Doubao (Volcengine Ark)", "OpenRouter",
    ]);
    // Every preset is one the sheet saves as filled: a name the server takes, a reference
    // for a key, a chat model to default to, and no embedding on the Anthropic wire.
    for (const o of [...document.querySelectorAll<HTMLOptionElement>("#lpPreset option")].slice(1)) {
      change("#lpPreset", o.value);
      expect(q<HTMLInputElement>("#lpName").value, o.value).toMatch(/^[A-Za-z0-9][A-Za-z0-9-]{0,31}$/);
      expect(q<HTMLInputElement>("#lpKey").value, o.value).toMatch(o.value === "ollama" ? /^$/ : /^\$\{secret:\/\/[a-z][a-z0-9-]*\}$/);
      expect(modelLines()[0], o.value).toEqual([expect.any(String), "chat", "default"]);
      if (q<HTMLSelectElement>("#lpStyle").value === "anthropic") expect(modelLines().map((l) => l[1]), o.value).not.toContain("embedding");
    }
    // DeepSeek V4.1 Flash is deepseek-flash on DeepSeek's own API, over the OpenAI wire.
    change("#lpPreset", "deepseek");
    expect([q<HTMLInputElement>("#lpName").value, q<HTMLSelectElement>("#lpStyle").value, q<HTMLInputElement>("#lpBase").value])
      .toEqual(["DeepSeek", "openai", "https://api.deepseek.com"]);
    expect(q<HTMLInputElement>("#lpKey").value).toBe("${secret://deepseek}");
    expect(modelLines()).toEqual([["deepseek-flash", "chat", "default"], ["deepseek-v4-pro", "chat", ""]]);
    // Kimi takes the Anthropic wire at its China mainland host.
    change("#lpPreset", "kimi");
    expect([q<HTMLInputElement>("#lpName").value, q<HTMLSelectElement>("#lpStyle").value, q<HTMLInputElement>("#lpBase").value])
      .toEqual(["Kimi", "anthropic", "https://api.moonshot.cn/anthropic"]);
    expect(modelLines()).toEqual([["kimi-k3.1-pro", "chat", "default"], ["kimi-k3.1-pro-highspeed", "chat", ""], ["kimi-k3", "chat", ""]]);
    // OpenRouter's ids carry the vendor; its embedding rides the OpenAI wire too.
    change("#lpPreset", "openrouter");
    expect(q<HTMLInputElement>("#lpBase").value).toBe("https://openrouter.ai/api/v1");
    const routed = modelLines();
    expect([routed[0], routed[routed.length - 1]]).toEqual([["deepseek/deepseek-v4.1-flash", "chat", "default"], ["openai/text-embedding-3-small", "embedding", "default"]]);
    change("#lpPreset", "openai");
    expect(q<HTMLInputElement>("#lpName").value).toBe("OpenAI");
    expect(q<HTMLInputElement>("#lpBase").value).toBe("https://api.openai.com/v1");
    expect(q<HTMLInputElement>("#lpKey").value).toBe("${secret://openai}");
    expect(modelLines()).toEqual([["gpt-6.1-terra", "chat", "default"], ["gpt-6-astra", "chat", ""], ["gpt-6.1-sol", "chat", ""],
      ["gpt-6-luna", "chat", ""], ["text-embedding-3-small", "embedding", "default"]]);
    expect(q("#lpModelsEmpty").hidden).toBe(true);
    // The privacy line names the host the text would go to.
    expect(q("#lpRemote").hidden).toBe(false);
    expect(text("#lpRemote")).toBe("Text sent to this provider leaves this machine for api.openai.com.");
    // Another preset replaces the models rather than piling them up.
    change("#lpPreset", "glm");
    expect(modelLines()).toEqual([["glm-5.3-flash", "chat", "default"], ["glm-5.3", "chat", ""]]);
    change("#lpPreset", "openai");

    type("#lpKey", "sk-live-123");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("API key must be a ${secret://name} or ${ENV_VAR} reference - never the key itself.");
    expect(puts()).toEqual([]);

    type("#lpName", "ollama");
    type("#lpKey", "${secret://openai}");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("A provider named ollama already exists");
    // A name apart from one in the row only in case is that name.
    type("#lpName", "GLM");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("A provider named glm already exists");
    type("#lpName", "Open AI");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("A provider name is 1-32 letters, digits and dashes, starting with a letter or digit.");

    type("#lpName", "OpenAI");
    await click("#lpSave");
    expect(puts().length).toBe(1);
    // The rest of the row rides back untouched; the new entry carries only what was typed.
    expect(puts()[0]).toEqual({
      ...initialRow(),
      providers: {
        ...initialRow().providers,
        OpenAI: {
          style: "openai", baseUrl: "https://api.openai.com/v1", apiKey: "${secret://openai}",
          // Each with what OpenAI states: GPT-6's 1.05M window, 128K output and image input.
          models: [
            { id: "gpt-6.1-terra", kind: "chat", name: "GPT-6.1 Terra", contextWindow: 1_050_000, maxOutput: 128_000, image: true },
            { id: "gpt-6-astra", kind: "chat", name: "GPT-6 Astra", contextWindow: 1_050_000, maxOutput: 128_000, image: true },
            { id: "gpt-6.1-sol", kind: "chat", name: "GPT-6.1 Sol", contextWindow: 1_050_000, maxOutput: 128_000, image: true },
            { id: "gpt-6-luna", kind: "chat", name: "GPT-6 Luna", contextWindow: 1_050_000, maxOutput: 128_000, image: true },
            { id: "text-embedding-3-small", kind: "embedding", name: "Text Embedding 3 Small" },
          ],
        },
      },
    });
    expect(text("#toast")).toBe("Provider OpenAI saved");
    expect(q("#sheet").hidden).toBe(true);
    expect(document.querySelector("[data-lprow=OpenAI]")).not.toBe(null);
  });

  it("the models list: add, reorder and remove lines; a twice-listed id and an embedding on the Anthropic wire are refused", async () => {
    await providers.mount();
    await click("#lpAdd");
    type("#lpName", "relay");
    type("#lpBase", "http://127.0.0.1:8000/v1");
    await click("#lpAddModel");
    await click("#lpAddModel");
    change(lineSel(1) + " input", "a-chat", "input");
    change(lineSel(2) + " input", "e-embed", "input");
    change(lineSel(2) + " select", "embedding");
    expect(modelLines()).toEqual([["a-chat", "chat", "default"], ["e-embed", "embedding", "default"]]);
    await click("#lpAddModel");
    change(lineSel(3) + " input", "b-chat", "input");
    expect(modelLines()[2]).toEqual(["b-chat", "chat", ""]);
    // Move up twice: b-chat becomes the first chat model, so the default mark moves with it.
    await click(lineSel(3) + " [data-lpmodelup]");
    await click(lineSel(2) + " [data-lpmodelup]");
    expect(modelLines()).toEqual([["b-chat", "chat", "default"], ["a-chat", "chat", ""], ["e-embed", "embedding", "default"]]);
    expect(q<HTMLButtonElement>(lineSel(1) + " [data-lpmodelup]").disabled).toBe(true);

    await click("#lpAddModel");
    change(lineSel(4) + " input", "a-chat", "input");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("a-chat is listed twice");
    await click(lineSel(4) + " [data-lpmodelrm]");
    // A line left blank is one nobody filled in: it is not stored.
    await click("#lpAddModel");

    change("#lpStyle", "anthropic");
    expect(q<HTMLOptionElement>(lineSel(1) + " option[value=embedding]").disabled).toBe(true);
    // The line already set to embedding keeps its pick, so Save names it.
    expect(q<HTMLSelectElement>(lineSel(3) + " select").value).toBe("embedding");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("e-embed: the Anthropic wire serves no embeddings - list it on an OpenAI-wire provider for that endpoint");
    expect(puts()).toEqual([]);

    change("#lpStyle", "openai");
    await click("#lpSave");
    expect((puts()[0].providers as Body).relay).toEqual({
      style: "openai", baseUrl: "http://127.0.0.1:8000/v1",
      models: [{ id: "b-chat", kind: "chat" }, { id: "a-chat", kind: "chat" }, { id: "e-embed", kind: "embedding" }],
    });
  });

  it("a model line opens to its details: name, context window, output cap, inputs and thinking - read as the store keeps them and refused when a count is off", async () => {
    await providers.mount();
    await click("#lpAdd");
    const facts = (n: number): string => text(lineSel(n) + " [data-lpfacts]");
    const box = (n: number): HTMLElement => q(lineSel(n) + " [data-lpdetail]");
    const input = (n: number, mark: string): HTMLInputElement => q<HTMLInputElement>(lineSel(n) + " [data-" + mark + "]");
    // A preset's lines are shut, each a quiet line of what its vendor states.
    change("#lpPreset", "deepseek");
    expect(box(1).hidden).toBe(true);
    expect(facts(1)).toBe("DeepSeek V4.1 Flash · 1M context · 384K output · image input · thinking");
    expect(facts(2)).toBe("DeepSeek V4 Pro · 1M context · 384K output · thinking");
    // Details opens the fields; the summary steps aside while they show.
    await click(lineSel(1) + " [data-lpmodelmore]");
    expect(box(1).hidden).toBe(false);
    expect(q(lineSel(1) + " [data-lpmodelmore]").getAttribute("aria-expanded")).toBe("true");
    expect(q(lineSel(1) + " [data-lpfacts]").hidden).toBe(true);
    expect([input(1, "lpname").value, input(1, "lpwindow").value, input(1, "lpoutput").value]).toEqual(["DeepSeek V4.1 Flash", "1048576", "393216"]);
    expect([input(1, "lpimage").checked, input(1, "lpthinking").checked]).toEqual([true, true]);
    const textInput = q<HTMLInputElement>(lineSel(1) + " .check-row input:not([data-lpimage]):not([data-lpthinking])");
    expect([textInput.checked, textInput.disabled], "text is every chat model's input").toEqual([true, true]);
    await click(lineSel(1) + " [data-lpmodelmore]");
    expect(box(1).hidden).toBe(true);
    // Qwen3.8 Flash takes no effort; an embedding model states a name alone.
    change("#lpPreset", "qwen");
    expect(facts(1)).toBe("Qwen3.8 Flash · 1M context · 128K output · image input · no thinking effort");
    expect(input(1, "lpthinking").checked).toBe(false);
    expect(facts(3)).toBe("Text Embedding v4");
    expect(q(lineSel(3) + " [data-lpchatonly]").hidden).toBe(true);

    change("#lpPreset", "custom");
    type("#lpName", "relay");
    type("#lpBase", "http://127.0.0.1:8000/v1");
    // A new line opens on its fields: nothing is filled in to summarise yet.
    await click("#lpAddModel");
    expect(box(1).hidden).toBe(false);
    change(lineSel(1) + " input", "m1", "input");
    change(lineSel(1) + " [data-lpwindow]", "256K", "input");
    change(lineSel(1) + " [data-lpoutput]", "512K", "input");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("m1: max output tokens (512K) is past the context window (256K)");
    change(lineSel(1) + " [data-lpoutput]", "lots", "input");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("m1: max output tokens is a count from 256 to 100M - 128K or 65536");
    change(lineSel(1) + " [data-lpoutput]", "", "input");
    change(lineSel(1) + " [data-lpwindow]", "512", "input");
    // A refused line opens on its fields, so the count Save names is in view.
    await click(lineSel(1) + " [data-lpmodelmore]");
    expect(box(1).hidden).toBe(true);
    await click("#lpSave");
    expect(text("#lpErr")).toBe("m1: the context window is a token count from 1K to 100M - 1M, 256K or 131072");
    expect(box(1).hidden).toBe(false);
    expect(puts()).toEqual([]);
    change(lineSel(1) + " [data-lpwindow]", "1.5M", "input");
    change(lineSel(1) + " [data-lpoutput]", "65536", "input");
    change(lineSel(1) + " [data-lpname]", "M One", "input");
    check(lineSel(1) + " [data-lpimage]", true);
    check(lineSel(1) + " [data-lpthinking]", false);
    await click(lineSel(1) + " [data-lpmodelmore]");
    expect(facts(1)).toBe("M One · 1.5M context · 64K output · image input · no thinking effort");
    // An embedding line keeps its name alone, whatever the chat fields held.
    await click("#lpAddModel");
    change(lineSel(2) + " input", "e1", "input");
    change(lineSel(2) + " [data-lpwindow]", "8K", "input");
    change(lineSel(2) + " select", "embedding");
    expect(q(lineSel(2) + " [data-lpchatonly]").hidden).toBe(true);
    // A line stating nothing stores the id and kind alone.
    await click("#lpAddModel");
    change(lineSel(3) + " input", "m2", "input");
    await click("#lpSave");
    expect((puts()[0].providers as Body).relay).toEqual({
      style: "openai", baseUrl: "http://127.0.0.1:8000/v1",
      models: [
        { id: "m1", kind: "chat", name: "M One", contextWindow: 1_500_000, maxOutput: 65_536, image: true, thinking: false },
        { id: "e1", kind: "embedding" },
        { id: "m2", kind: "chat" },
      ],
    });
  });

  it("Fill known details gives a saved provider's known ids what their vendor states; Discover's take brings them too; the row lists them", async () => {
    let listing: Answer = [200, {
      ok: true, url: "https://open.bigmodel.cn/api/anthropic/v1/models", ms: 5,
      models: [{ id: "glm-5.3", kind: "chat" }, { id: "glm-x", kind: "chat" }],
    }];
    override = (c) => c.path === "/api/llm/providers/models" ? listing : undefined;
    await providers.mount();
    await click("[data-lpmore=glm]");
    await menuPick("Edit");
    // glm-5.3-flash was saved before lines had details: nothing to summarise, one click to fill.
    expect(q(lineSel(1) + " [data-lpfacts]").hidden).toBe(true);
    expect(q<HTMLButtonElement>("#lpFillKnown").disabled).toBe(false);
    await click("#lpFillKnown");
    expect(text(lineSel(1) + " [data-lpfacts]")).toBe("GLM-5.3 Flash · 1M context · 128K output · image input · thinking");
    await click("#lpDiscover");
    check("[data-lpfound='glm-5.3']", true);
    check("[data-lpfound='glm-x']", true);
    await click("#lpFoundTake");
    expect(text(lineSel(2) + " [data-lpfacts]"), "a known id comes with its details").toBe("GLM-5.3 · 1M context · 128K output · thinking");
    expect(q(lineSel(3) + " [data-lpfacts]").hidden, "an unknown one with none").toBe(true);
    await click("#lpSave");
    expect(((puts()[0].providers as Body).glm as Body).models).toEqual([
      { id: "glm-5.3-flash", kind: "chat", name: "GLM-5.3 Flash", contextWindow: 1_048_576, maxOutput: 131_072, image: true },
      { id: "glm-5.3", kind: "chat", name: "GLM-5.3", contextWindow: 1_048_576, maxOutput: 131_072 },
      { id: "glm-x", kind: "chat" },
    ]);
    const lines = [...document.querySelectorAll("[data-lprow=glm] [data-lpblock=models] .kv")]
      .map((k) => [k.querySelector(".kv-k")?.textContent ?? "", k.querySelector(".kv-v")?.textContent ?? ""]);
    expect(lines).toEqual([
      ["glm-5.3-flash", "Chat · GLM-5.3 Flash · 1M context · 128K output · image input · thinking default"],
      ["glm-5.3", "Chat · GLM-5.3 · 1M context · 128K output · thinking"],
      ["glm-x", "Chat · thinking"],
    ]);

    // A provider listing no id a preset knows has nothing to fill.
    listing = [200, { ok: true, url: "", ms: 1, models: [] }];
    row.providers = { ...row.providers, bare: { style: "openai", baseUrl: "http://127.0.0.1:8000/v1", models: [{ id: "local-x" }] } };
    await providers.refresh();
    await click("[data-lpmore=bare]");
    await menuPick("Edit");
    expect(q<HTMLButtonElement>("#lpFillKnown").disabled).toBe(true);
  });

  it("token counts read K and M; a stored count shows back as the same count", () => {
    expect(["1M", "256K", "131072", "1.5M", "128k", "1,000,000", "", "  ", "lots", "1.5", "-1K"].map(providers.parseTokens))
      .toEqual([1_000_000, 256_000, 131_072, 1_500_000, 128_000, 1_000_000, null, null, NaN, NaN, NaN]);
    expect([1_000_000, 128_000, 131_072, 1_048_576, 1_050_000, null].map(providers.tokensField)).toEqual(["1M", "128K", "131072", "1048576", "1050K", ""]);
    for (const n of [1_000_000, 128_000, 131_072, 1_048_576, 1_050_000, 393_216]) expect(providers.parseTokens(providers.tokensField(n))).toBe(n);
    expect([1_048_576, 1_000_000, 131_072, 128_000, 393_216, 512_000, 1_050_000, 65_536, 800].map(providers.tokensShort))
      .toEqual(["1M", "1M", "128K", "128K", "384K", "512K", "1.05M", "64K", "800"]);
  });

  it("Edit prefills from the row; a rename re-keys the entry and re-points its consumers; Delete is refused while a consumer names it", async () => {
    await providers.mount();
    await click("[data-lpmore=glm]");
    await menuPick("Edit");
    expect(text("#sheet h2")).toBe("Edit glm");
    expect(q<HTMLInputElement>("#lpName").value).toBe("glm");
    expect(q<HTMLInputElement>("#lpName").disabled).toBe(false);
    expect(q<HTMLSelectElement>("#lpStyle").value).toBe("anthropic");
    expect(q<HTMLInputElement>("#lpKey").value).toBe("${secret://glm}");
    expect(modelLines()).toEqual([["glm-5.3-flash", "chat", "default"]]);
    // An edit has no preset: it would overwrite what is being edited.
    expect(document.querySelector("#lpPreset")).toBe(null);
    await click("#lpAddModel");
    change(lineSel(2) + " input", "glm-5.3", "input");
    await click("#lpSave");
    expect((puts()[0].providers as Body).glm).toEqual({
      style: "anthropic", baseUrl: "https://open.bigmodel.cn/api/anthropic", apiKey: "${secret://glm}",
      models: [{ id: "glm-5.3-flash", kind: "chat" }, { id: "glm-5.3", kind: "chat" }],
    });

    // A rename to a taken name is refused; a good one re-keys the entry and re-points
    // the consumers that named the old key - the agent, not the embedding tier that
    // names ollama - while everything else rides back untouched.
    await click("[data-lpmore=glm]");
    await menuPick("Edit");
    type("#lpName", "ollama");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("A provider named ollama already exists");
    type("#lpName", "Ollama");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("A provider named ollama already exists");
    expect(puts().length).toBe(1);
    // glm's own rename to GLM is no clash: the name is the provider's, capitals and all.
    type("#lpName", "GLM");
    await click("#lpSave");
    expect(puts().length).toBe(2);
    const renamed = puts()[1];
    expect(Object.keys(renamed.providers as Body)).toEqual(["ollama", "GLM"]);
    expect((renamed.agents as Record<string, Body>).distiller.provider).toBe("GLM");
    // cc carries no provider of its own: untouched.
    expect((renamed.agents as Record<string, Body>).cc.provider).toBeUndefined();
    expect((renamed.embedding as Body).provider).toBe("ollama");
    expect(text("#toast")).toBe("Provider GLM saved");

    // The list refreshed under the new name; the refusal names the renamed key and the
    // consumer that was re-pointed onto it.
    await click("[data-lpmore=GLM]");
    await menuPick("Delete");
    expect(text("#toast")).toBe("GLM is in use (distiller) - point those at another provider first");
    await click("[data-lpmore=ollama]");
    await menuPick("Delete");
    expect(text("#toast")).toBe("ollama is in use (embedding) - point those at another provider first");
    expect(puts().length).toBe(2);
  });

  it("Edit refuses a list that would strand a user: a picked model must stay, a default needs a model of its kind", async () => {
    row.agents = { distiller: { runtime: "builtin", provider: "glm", model: "glm-5.3-flash" }, quick: { runtime: "chat", provider: "ollama" } };
    await providers.mount();
    await click("[data-lpmore=glm]");
    await menuPick("Edit");
    await click(lineSel(1) + " [data-lpmodelrm]");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("glm-5.3-flash is picked by distiller - point those at another model first");
    await click("#lpCancel");

    await click("[data-lpmore=ollama]");
    await menuPick("Edit");
    await click(lineSel(2) + " [data-lpmodelrm]");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("bge-m3 is picked by embedding - point those at another model first");
    await click("#lpCancel");

    row.embedding = { provider: "ollama" };
    await click("[data-lpmore=ollama]");
    await menuPick("Edit");
    await click(lineSel(1) + " [data-lpmodelrm]");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("This provider's default chat model is used by quick - keep at least one chat model");
    await click("#lpAddModel");
    change(lineSel(2) + " input", "qwen3.5:9b", "input");
    await click(lineSel(1) + " [data-lpmodelrm]");
    await click("#lpSave");
    expect(text("#lpErr")).toBe("This provider's default embedding model is used by embedding - keep at least one embedding model");
    expect(puts()).toEqual([]);
  });

  it("Discover asks the saved provider for its listing and offers the ids not listed yet", async () => {
    let listing: Answer = [200, {
      ok: true, url: "http://127.0.0.1:11434/v1/models", ms: 12,
      models: [{ id: "qwen3.5:4b", kind: "chat" }, { id: "llama3.2:3b", kind: "chat" }, { id: "nomic-embed-text", kind: "embedding" }],
    }];
    override = (c) => c.path === "/api/llm/providers/models" ? listing : undefined;
    await providers.mount();
    await click("[data-lpmore=ollama]");
    await menuPick("Edit");
    await click("#lpDiscover");
    expect(sent("POST", "/api/llm/providers/models").map((c) => c.body)).toEqual([{ provider: "ollama" }]);
    expect(q("#lpFound").hidden).toBe(false);
    expect(text("#lpFound .sec-cap")).toBe("Listed by the provider");
    expect(text("#lpFound")).toContain("http://127.0.0.1:11434/v1/models answered in 12 ms. Not here yet: 2 of the 3 it lists.");
    expect([...document.querySelectorAll<HTMLInputElement>("[data-lpfound]")].map((c) => c.dataset.lpfound)).toEqual(["llama3.2:3b", "nomic-embed-text"]);
    check("[data-lpfound='nomic-embed-text']", true);
    await click("#lpFoundTake");
    expect(modelLines()).toEqual([["qwen3.5:4b", "chat", "default"], ["bge-m3", "embedding", "default"], ["nomic-embed-text", "embedding", ""]]);
    expect(text("#lpFound")).toBe("1 model added - save to keep it.");

    listing = [200, { ok: true, url: "http://127.0.0.1:11434/v1/models", ms: 3, models: [{ id: "qwen3.5:4b", kind: "chat" }] }];
    await click("#lpDiscover");
    expect(text("#lpFound")).toBe("http://127.0.0.1:11434/v1/models answered: every model it lists (1) is here already.");
    listing = [200, { ok: false, url: "http://127.0.0.1:11434/v1/models", error: "connection refused", ms: 2 }];
    await click("#lpDiscover");
    expect(text("#lpFound")).toBe("http://127.0.0.1:11434/v1/models did not list its models: connection refused");
    listing = [200, { ok: true, url: "http://127.0.0.1:11434/v1/models", ms: 3, models: [{ id: "mistral:7b", kind: "chat" }] }];
    await click("#lpDiscover");
    // Dismissed, the answer takes no room in the sheet.
    await click("#lpFoundDrop");
    expect(q("#lpFound").hidden).toBe(true);

    await click("#lpSave");
    expect(((puts()[0].providers as Body).ollama as Body).models).toEqual([
      { id: "qwen3.5:4b", kind: "chat" }, { id: "bge-m3", kind: "embedding" }, { id: "nomic-embed-text", kind: "embedding" },
    ]);
  });

  it("an unused provider deletes after a confirm", async () => {
    row.agents = {};
    await providers.mount();
    confirmAnswer = false;
    await click("[data-lpmore=glm]");
    await menuPick("Delete");
    expect(puts()).toEqual([]);
    confirmAnswer = true;
    await click("[data-lpmore=glm]");
    await menuPick("Delete");
    expect(Object.keys(puts()[0].providers as Body)).toEqual(["ollama"]);
    expect(text("#toast")).toBe("Provider glm deleted");
    expect(document.querySelector("[data-lprow=glm]")).toBe(null);
  });

  it("Test asks the saved provider one prompt through one of its chat models", async () => {
    override = (c) => c.path === "/api/llm/providers/test"
      ? [200, { ok: true, model: "glm-5.3-flash", text: "I am GLM.", ms: 812 }]
      : undefined;
    row.providers = {
      ...row.providers,
      glm: { ...(row.providers || {}).glm, models: [{ id: "glm-5.3-flash" }, { id: "glm-5.3" }] },
      bare: { style: "openai", baseUrl: "http://127.0.0.1:8000/v1" },
    };
    await providers.mount();
    await click("[data-lptest=glm]");
    expect(text("#sheet h2")).toBe("Test glm");
    expect(text("#sheet")).toContain("https://open.bigmodel.cn/api/anthropic/v1/messages");
    expect(options("#lpTestModel").map((o) => o[0])).toEqual(["glm-5.3-flash", "glm-5.3"]);
    await click("#lpTestRun");
    expect(sent("POST", "/api/llm/providers/test").map((c) => c.body)).toEqual([{ provider: "glm", model: "glm-5.3-flash" }]);
    expect(text("#lpTestOut")).toContain("Answered");
    expect(text("#lpTestOut")).toContain("I am GLM.");
    expect(text("#lpTestOut")).toContain("812 ms");
    change("#lpTestModel", "glm-5.3");
    type("#lpTestPrompt", "Say hi");
    await click("#lpTestRun");
    expect(sent("POST", "/api/llm/providers/test")[1].body).toEqual({ provider: "glm", model: "glm-5.3", prompt: "Say hi" });

    // A provider with no chat model has nothing to ask.
    await click("[data-lptest=bare]");
    expect(text("#sheet")).toContain("This provider lists no chat model - edit it and add one first.");
    expect(q<HTMLButtonElement>("#lpTestRun").disabled).toBe(true);
  });

  it("a stopped plugin: the page says so and loads nothing", async () => {
    override = (c) => c.path === "/api/llm/status" ? [503, { error: "plugin llm is not running" }] : undefined;
    await providers.mount();
    expect(text("#pane")).toContain("Could not load the LLM settings");
    expect(text("#pane")).toContain("plugin llm is not running");
  });
});

/* ------------------------------------------------------------------ Embedding */

describe("the LLM plugin - Embedding", () => {
  it("the aside shows the block in force; the selects offer OpenAI-wire providers and their embedding models", async () => {
    await embedding.mount();
    await settle();
    expect(text("#leInForce")).toContain("ollama");
    expect(text("#leInForce")).toContain("bge-m3");
    expect(text("#leInForce")).toContain("16 texts · 30000 ms");
    expect(text("#leInForce")).toContain("openai:bge-m3:native");
    expect(document.querySelector("#leInForce .tag.warn")).toBe(null);
    const opts = [...q<HTMLSelectElement>("#leProvider").options].map((o) => [o.value, o.disabled]);
    expect(opts).toEqual([["off", false], ["ollama", false], ["glm", true]]);
    expect(q<HTMLSelectElement>("#leProvider").value).toBe("ollama");
    // The provider's embedding models only - its chat model is no pick here.
    expect(options("#leModel")).toEqual([["", "The provider's default (now bge-m3)"], ["bge-m3", "bge-m3"]]);
    expect(q<HTMLSelectElement>("#leModel").value).toBe("bge-m3");
    expect(text("#countChip")).toBe("bge-m3");
    expect(embedding.hasPendingChanges()).toBe(false);
  });

  it("Save writes the block into the row; a bad number is refused first; Test refuses an unsaved edit", async () => {
    override = (c) => c.path === "/api/llm/embedding/test" ? [200, { ok: true, model: "openai:bge-m3:native", dims: 1024, ms: 40 }] : undefined;
    await embedding.mount();
    await settle();
    type("#leDims", "1k");
    expect(embedding.hasPendingChanges()).toBe(true);
    await click("#leTest");
    expect(text("#leErr")).toBe("Test asks the saved settings - save your changes first.");
    expect(sent("POST", "/api/llm/embedding/test")).toEqual([]);
    await click("#leSave");
    expect(text("#leErr")).toBe("Dimensions: a whole number from 1 to 8192");
    type("#leDims", "");
    type("#leFloor", "1.5");
    await click("#leSave");
    expect(text("#leErr")).toBe("Similarity floor: a number from 0 to 1");
    expect(puts()).toEqual([]);

    type("#leFloor", "0.5");
    type("#leQPrefix", "query: ");
    await click("#leSave");
    expect(puts()[0].embedding).toEqual({ provider: "ollama", model: "bge-m3", queryPrefix: "query: ", minSimilarity: 0.5 });
    expect(text("#toast")).toBe("Embedding settings saved");
    // The page reloaded over the saved block: no edit pending, and Test goes out.
    expect(embedding.hasPendingChanges()).toBe(false);
    await click("#leTest");
    expect(sent("POST", "/api/llm/embedding/test").map((c) => c.body)).toEqual([{}]);
    expect(text("#leTestOut")).toContain("1024");
  });

  it("each provider keeps its own pick; off keeps what was typed and hides it; a remote provider is named", async () => {
    row.providers = {
      ...row.providers,
      openai: { style: "openai", baseUrl: "https://api.openai.com/v1", apiKey: "${secret://openai}", models: [{ id: "text-embedding-3-small", kind: "embedding" }] },
    };
    await embedding.mount();
    await settle();
    change("#leProvider", "openai");
    expect(options("#leModel").map((o) => o[0])).toEqual(["", "text-embedding-3-small"]);
    expect(q<HTMLSelectElement>("#leModel").value).toBe("");
    expect(q("#leRemote").hidden).toBe(false);
    expect(text("#leRemote")).toBe("Text is sent to api.openai.com to be embedded. A local server keeps it on this machine.");
    change("#leProvider", "ollama");
    expect(q<HTMLSelectElement>("#leModel").value).toBe("bge-m3");
    change("#leProvider", "off");
    expect(q("#leFields").hidden).toBe(true);
    expect(q("#leRetrieval").hidden).toBe(true);
    expect(q("#leThroughput").hidden).toBe(true);
    await click("#leSave");
    expect(puts()[0].embedding).toEqual({ provider: "off", model: "bge-m3" });
    expect(text("#leInForce")).toContain("Off - memory search matches words only.");
    expect(q<HTMLButtonElement>("#leTest").disabled).toBe(true);
    // Stored off, the block still holds its pick: a save while off keeps it.
    await click("#leSave");
    expect(puts()[1].embedding).toEqual({ provider: "off", model: "bge-m3" });
  });

  it("a provider listing no embedding model, or no longer listing the stored one, is refused before the server", async () => {
    row.providers = {
      ...row.providers,
      chatty: { style: "openai", baseUrl: "http://127.0.0.1:8000/v1", models: [{ id: "llama-4" }] },
    };
    row.embedding = { provider: "ollama", model: "bge-large" };
    await embedding.mount();
    await settle();
    expect(options("#leModel")).toEqual([
      ["", "The provider's default (now bge-m3)"], ["bge-m3", "bge-m3"], ["bge-large", "bge-large (not listed by the provider)"],
    ]);
    expect(q<HTMLSelectElement>("#leModel").value).toBe("bge-large");
    await click("#leSave");
    expect(text("#leErr")).toBe("ollama does not list bge-large - pick a listed model, or add it on the Providers page.");
    change("#leProvider", "chatty");
    expect(q<HTMLSelectElement>("#leModel").disabled).toBe(true);
    expect(text("#leModelWhy")).toBe("chatty lists no embedding model - add one on the Providers page.");
    await click("#leSave");
    expect(text("#leErr")).toBe("chatty lists no embedding model - add one on the Providers page.");
    expect(puts()).toEqual([]);
    change("#leProvider", "ollama");
    change("#leModel", "");
    await click("#leSave");
    expect(puts()[0].embedding).toEqual({ provider: "ollama" });
  });
});

/* ------------------------------------------------------------------ Agents and Reporting */

/** The agent sheet's tool checks, as [name, checked]. */
function toolChecks(): Array<[string, boolean]> {
  return [...document.querySelectorAll<HTMLInputElement>("[data-latool]")].map((c) => [c.dataset.latool || "", c.checked]);
}

describe("the LLM plugin - Agents", () => {
  it("Test lists every allowed tool and sends the subset the user selected", async () => {
    await agents.mount();
    await click("[data-latest=distiller]");
    const picks = Array.from(document.querySelectorAll<HTMLInputElement>("#laTestTools input"));
    expect(picks.map((p) => p.value)).toEqual(CATALOG.builtin.map((t) => t.name));
    expect(picks.every((p) => p.checked)).toBe(true);
    picks.find((p) => p.value === "sync")!.checked = false;
    picks.find((p) => p.value === "read")!.checked = false;
    type("#laTestCwd", "dev/swiss");
    await click("#laTestRun");
    expect(sent("POST", "/api/llm/agents/test/stream")[0].body).toMatchObject({ tools: ["bash", "delegate", "list", "job_output", "job_kill"], cwd: "dev/swiss" });
  });

  it("a row is the runtime and what it runs, its tools, and whether its runs are reported", async () => {
    row.telemetry = { endpoint: "http://127.0.0.1:4317" };
    await agents.mount();
    expect([...document.querySelectorAll<HTMLElement>("[data-larow]")].map((r) => r.dataset.larow)).toEqual(["distiller", "cc"]);
    expect(text("[data-larow=distiller] .lrow-sub")).toBe("built-in · glm · glm-5.3-flash");
    expect(text("[data-larow=cc] .lrow-sub")).toBe("Claude Code · C:/Users/me/.local/bin/claude.exe");
    expect(text("[data-larow=distiller]")).toContain("read, bash, sync, delegate");
    expect(text("[data-larow=cc]")).toContain("Read, Grep, Glob");
    expect(text("[data-larow=distiller]")).toContain("reported");
    expect(text("[data-larow=cc]")).toContain("self-reported");
    expect(text("#countChip")).toBe("2 agents");
  });

  it("a row opens to the system prompt in force and every tool of its runtime", async () => {
    row.agents = {
      distiller: { runtime: "builtin", provider: "glm" },
      terse: { runtime: "builtin", provider: "glm", systemPrompt: "Be terse.", tools: ["read"] },
      cc: { runtime: "claude" },
      relay: { runtime: "claude", systemPrompt: "Answer briefly." },
      quick: { runtime: "chat", provider: "ollama" },
    };
    await agents.mount();
    const detail = (n: string): string => text("[data-larow=" + n + "] .lrow-detail");
    /** A detail block's lines, as [label, value]. */
    const kv = (n: string, block: string): Array<[string, string]> => [...document.querySelectorAll("[data-larow=" + n + "] [data-lablock=" + block + "] .kv")]
      .map((k) => [k.querySelector(".kv-k")?.textContent ?? "", k.querySelector(".kv-v")?.textContent ?? ""]);
    const tools = (n: string): Array<[string, string]> => kv(n, "tools");
    // What it runs on comes first: the sub line opening hides, and the limits.
    expect(kv("distiller", "setup")).toEqual([
      ["Runtime", "built-in"], ["Provider", "glm"], ["Model", "glm-5.3-flash"], ["Max turns", "24"], ["Max tokens", "131072"], ["Request budget (KiB)", "1024"],
      ["Thinking effort", "The model's default"], ["Concurrency", "4"],
      ["Run timeout (s)", "1800"],
    ]);
    expect(kv("cc", "setup").map((r) => r[0])).toEqual(["Runtime", "Command", "Concurrency", "Run timeout (s)"]);
    // No prompt of its own: the runtime's default, marked as such; every tool taken.
    expect(detail("distiller")).toContain("System promptdefault" + DEFAULT_PROMPT);
    expect(tools("distiller").map((t) => t[0])).toEqual(["read", "bash", "sync", "delegate", "list", "job_output", "job_kill"]);
    expect(tools("distiller").filter((t) => t[1].startsWith("not taken"))).toEqual([]);
    expect(tools("distiller")[0][1]).toBe("Read one file of the checkout (credential files refused)");
    expect(tools("distiller")[3][1]).toBe("Hand pieces of a lead's work to its workers, each at the effort it picks");
    // Its own prompt follows the default, never replaces it; the tools it left out say so.
    expect(text("[data-larow=terse] .lrow-sub")).toBe("built-in · glm · glm-5.3-flash · own prompt");
    expect(text("[data-larow=terse] [data-lablock=prompt] .vblock-head")).toBe("System promptthe default, then its own");
    expect([...document.querySelectorAll("[data-larow=terse] [data-lablock=prompt] .vblock-text")].map((n) => n.textContent)).toEqual([DEFAULT_PROMPT, "Be terse."]);
    expect(tools("terse").filter((t) => t[1].startsWith("not taken")).map((t) => t[0])).toEqual(["bash", "sync", "delegate", "list", "job_output", "job_kill"]);
    expect(detail("cc")).toContain("The CLI's own system prompt - nothing appended.");
    // A CLI's own prompt is appended to the CLI's, and says so.
    expect(text("[data-larow=relay] [data-lablock=prompt] .vblock-head")).toBe("System promptappended to the CLI's own");
    expect(text("[data-larow=relay] [data-lablock=prompt] .vblock-text")).toBe("Answer briefly.");
    expect(detail("quick")).toContain("None - a chat agent answers from the prompt alone.");
    expect(text("[data-larow=quick]")).toContain("no tools");
  });

  it("a builtin agent picks one of its provider's chat models; a provider listing none is refused", async () => {
    row.providers = { ...row.providers, bare: { style: "openai", baseUrl: "http://127.0.0.1:8000/v1" } };
    await agents.mount();
    await click("#laAdd");
    type("#laName", "summarizer");
    change("#laProvider", "bare");
    expect(q<HTMLSelectElement>("#laModelPick").disabled).toBe(true);
    expect(text("#laModelPickWhy")).toBe("bare lists no chat model - add one on the Providers page.");
    await click("#laSave");
    expect(text("#laErr")).toBe("bare lists no chat model - add one on the Providers page.");
    change("#laProvider", "ollama");
    // The provider's chat models only - bge-m3 is an embedding model.
    expect(options("#laModelPick")).toEqual([["", "The provider's default (now qwen3.5:4b)"], ["qwen3.5:4b", "qwen3.5:4b"]]);
    change("#laModelPick", "qwen3.5:4b");
    type("#laTurns", "99");
    await click("#laSave");
    expect(text("#laErr")).toBe("Max turns: a whole number from 1 to 64");
    type("#laTurns", "8");
    // The request budget, in KiB, within its range.
    type("#laContext", "32");
    await click("#laSave");
    expect(text("#laErr")).toBe("Request budget (KiB): a whole number from 64 to 8192");
    type("#laContext", "2048");
    type("#laTimeout", "600");
    await click("#laSave");
    expect((puts()[0].agents as Body).summarizer).toEqual({ runtime: "builtin", provider: "ollama", model: "qwen3.5:4b", maxTurns: 8, maxContextKiB: 2048, timeoutMs: 600_000 });
    expect(text("#toast")).toBe("Agent summarizer saved");
  });

  it("a builtin agent grants vault secrets to its shell; other runtimes have no such field", async () => {
    row.agents = { lead: { runtime: "builtin", provider: "ollama", secrets: ["api-key", "group:deploy"] } };
    await agents.mount();
    // The row says the grant as written and how many stored names it reaches now.
    expect(text("[data-larow=lead] [data-lablock=setup]")).toContain("api-key, group:deploy · 1 in the vault");
    await click("[data-lamore=lead]");
    await menuPick("Edit");
    expect(q<HTMLInputElement>("#laSecrets").value).toBe("api-key, group:deploy");
    type("#laSecrets", "api-key, API_KEY");
    await click("#laSave");
    expect(text("#laErr")).toBe("Secrets: API_KEY is not a secret name (lowercase kebab), group:<group> or *");
    type("#laSecrets", " api-key , group:Deploy Ops, * ");
    await click("#laSave");
    expect((puts()[0].agents as Body).lead).toEqual({ runtime: "builtin", provider: "ollama", secrets: ["api-key", "group:Deploy Ops", "*"] });
    // Only a builtin agent runs the shell a reference resolves in.
    await click("#laAdd");
    expect(q("#laSecrets").closest("[hidden]")).toBe(null);
    change("#laRuntime", "chat");
    expect(q("#laSecrets").closest("[hidden]")).not.toBe(null);
  });

  it("a builtin agent sets its thinking effort and concurrency; a claude agent its concurrency alone", async () => {
    row.agents = { lead: { runtime: "builtin", provider: "ollama", effort: "max", concurrency: 6 } };
    await agents.mount();
    // The row says the effort and the concurrency in force.
    expect(text("[data-larow=lead] [data-lablock=setup]")).toContain("max");
    expect(text("[data-larow=lead] [data-lablock=setup]")).toContain("6");
    await click("[data-lamore=lead]");
    await menuPick("Edit");
    expect(q<HTMLSelectElement>("#laEffort").value).toBe("max");
    expect(q<HTMLInputElement>("#laConcurrency").value).toBe("6");
    change("#laEffort", "low");
    type("#laConcurrency", "17");
    await click("#laSave");
    expect(text("#laErr")).toBe("Concurrency: a whole number from 1 to 16");
    type("#laConcurrency", "5");
    await click("#laSave");
    expect((puts()[0].agents as Body).lead).toEqual({ runtime: "builtin", provider: "ollama", effort: "low", concurrency: 5 });

    // A spawned CLI takes no effort: the field hides and nothing is written for it.
    await click("#laAdd");
    type("#laName", "cc");
    change("#laRuntime", "claude");
    expect(q("#laEffort").closest("[hidden]")).not.toBe(null);
    expect(q<HTMLInputElement>("#laConcurrency").placeholder).toBe("1");
    type("#laConcurrency", "2");
    await click("#laSave");
    expect((puts()[1].agents as Body).cc).toEqual({ runtime: "claude", concurrency: 2 });
  });

  it("a stored model the provider no longer lists shows as such, is refused, and survives a provider round trip", async () => {
    row.agents = { distiller: { runtime: "builtin", provider: "glm", model: "glm-4" } };
    await agents.mount();
    await click("[data-lamore=distiller]");
    await menuPick("Edit");
    expect(q<HTMLSelectElement>("#laModelPick").value).toBe("glm-4");
    expect(options("#laModelPick")[2]).toEqual(["glm-4", "glm-4 (not listed by the provider)"]);
    await click("#laSave");
    expect(text("#laErr")).toBe("glm does not list glm-4 - pick a listed model, or add it on the Providers page.");
    change("#laProvider", "ollama");
    expect(q<HTMLSelectElement>("#laModelPick").value).toBe("");
    change("#laProvider", "glm");
    expect(q<HTMLSelectElement>("#laModelPick").value).toBe("glm-4");
    change("#laModelPick", "");
    await click("#laSave");
    expect((puts()[0].agents as Body).distiller).toEqual({ runtime: "builtin", provider: "glm" });
  });

  it("the system prompt adds to the runtime's default shown above it; the tools are the runtime's catalog, all taken unless narrowed", async () => {
    await agents.mount();
    await click("#laAdd");
    type("#laName", "terse");
    // The default is always sent and shown as such; the box holds only what follows it.
    expect(text("#laPromptBase")).toBe(DEFAULT_PROMPT);
    expect(q("#laPromptBase").closest("[hidden]")).toBe(null);
    expect(q<HTMLTextAreaElement>("#laPrompt").placeholder).toBe("Appended after the default prompt");
    expect(document.querySelector("#laPromptDefault")).toBe(null);
    expect(toolChecks()).toEqual([["read", true], ["bash", true], ["sync", true], ["delegate", true], ["list", true], ["job_output", true], ["job_kill", true]]);
    // The default typed in is still the default: nothing of the agent's own is written.
    type("#laPrompt", DEFAULT_PROMPT);
    await click("#laSave");
    expect((puts()[0].agents as Body).terse).toEqual({ runtime: "builtin", provider: "ollama" });

    await click("[data-lamore=terse]");
    await menuPick("Edit");
    type("#laPrompt", "Be terse.");
    check("[data-latool=sync]", false);
    await click("#laSave");
    expect((puts()[1].agents as Body).terse).toEqual({ runtime: "builtin", provider: "ollama", systemPrompt: "Be terse.", tools: ["read", "bash", "delegate", "list", "job_output", "job_kill"] });

    await click("[data-lamore=terse]");
    await menuPick("Edit");
    expect(toolChecks()).toEqual([["read", true], ["bash", true], ["sync", false], ["delegate", true], ["list", true], ["job_output", true], ["job_kill", true]]);
    // Another runtime starts from all of its own tools; back on the stored one, the stored list.
    change("#laRuntime", "claude");
    expect(toolChecks()).toEqual([["Read", true], ["Grep", true], ["Glob", true]]);
    expect(q<HTMLTextAreaElement>("#laPrompt").placeholder).toBe("Appended to the CLI's own system prompt");
    expect(q("#laPromptBase").closest("[hidden]")).not.toBe(null);
    change("#laRuntime", "chat");
    expect(q("#laTools").closest("[hidden]")).not.toBe(null);
    change("#laRuntime", "builtin");
    expect(toolChecks()).toEqual([["read", true], ["bash", true], ["sync", false], ["delegate", true], ["list", true], ["job_output", true], ["job_kill", true]]);
    // None taken is a list too: the agent runs with no tools.
    for (const name of ["read", "bash", "delegate", "list", "job_output", "job_kill"]) check("[data-latool=" + name + "]", false);
    await click("#laSave");
    expect(((puts()[2].agents as Body).terse as Body).tools).toEqual([]);
  });

  it("a prompt past the cap is refused", async () => {
    await agents.mount();
    await click("#laAdd");
    type("#laName", "long");
    type("#laPrompt", "x".repeat(20_001));
    await click("#laSave");
    expect(text("#laErr")).toBe("The system prompt is at most 20000 characters");
    expect(puts()).toEqual([]);
  });

  it("a claude agent: a literal credential in its env is refused, a reference saved; reporting off is written", async () => {
    await agents.mount();
    await click("#laAdd");
    type("#laName", "relay");
    change("#laRuntime", "claude");
    expect(q("#laAsk").hidden).toBe(true);
    expect(q("#laCli").hidden).toBe(false);
    expect(q<HTMLInputElement>("#laCommand").placeholder).toBe("claude");
    type("#laEnv", "ANTHROPIC_BASE_URL=https://relay.example\nANTHROPIC_AUTH_TOKEN=sk-ant-live");
    await click("#laSave");
    expect(text("#laErr")).toBe("ANTHROPIC_AUTH_TOKEN names a credential: its value must be a ${secret://name} or ${ENV_VAR} reference.");
    expect(puts()).toEqual([]);
    type("#laEnv", "ANTHROPIC_BASE_URL=https://relay.example\nANTHROPIC_AUTH_TOKEN=${secret://relay}");
    check("#laReport", false);
    await click("#laSave");
    expect((puts()[0].agents as Body).relay).toEqual({
      runtime: "claude", env: { ANTHROPIC_BASE_URL: "https://relay.example", ANTHROPIC_AUTH_TOKEN: "${secret://relay}" }, report: false,
    });
  });

  it("a pi agent needs a provider/model id and cannot report", async () => {
    await agents.mount();
    await click("#laAdd");
    type("#laName", "pi-glm");
    change("#laRuntime", "pi");
    expect(q<HTMLInputElement>("#laReport").disabled).toBe(true);
    expect(q<HTMLInputElement>("#laReport").checked).toBe(false);
    expect(text("#laReportWhy")).toBe("pi has no telemetry hook; its runs are not reported.");
    expect(toolChecks().map((t) => t[0])).toEqual(["read", "grep", "find", "ls"]);
    type("#laModel", "glm-5.3-flash");
    await click("#laSave");
    expect(text("#laErr")).toBe("pi needs a model in \"provider/model\" form (say glm/glm-5.3-flash).");
    type("#laModel", "glm/glm-5.3-flash");
    await click("#laSave");
    expect((puts()[0].agents as Body)["pi-glm"]).toEqual({ runtime: "pi", model: "glm/glm-5.3-flash" });
  });

  it("Edit switches runtime and drops the old runtime's fields; an unchanged timeout keeps its milliseconds", async () => {
    row.agents = { distiller: { runtime: "builtin", provider: "glm", maxTurns: 12, timeoutMs: 90_500 } };
    await agents.mount();
    await click("[data-lamore=distiller]");
    await menuPick("Edit");
    expect(q<HTMLInputElement>("#laName").disabled).toBe(true);
    expect(q<HTMLInputElement>("#laTimeout").value).toBe("91");
    change("#laRuntime", "chat");
    expect(q("#laTurns").closest("[hidden]")).not.toBe(null);
    await click("#laSave");
    expect((puts()[0].agents as Body).distiller).toEqual({ runtime: "chat", provider: "glm", timeoutMs: 90_500 });
  });

  it("Test: a builtin agent answers one reported prompt; a claude agent is checked with --version", async () => {
    row.telemetry = { endpoint: "http://127.0.0.1:4317" };
    // The live dialogue: a stream of events (the typewriter) - reasoning, answer
    // deltas, a tool ask and its result, the final, the run's numbers.
    const frame = (event: string, data: unknown): string =>
      "event: " + event + "\ndata: " + JSON.stringify(data) + "\n\n";
    override = (c) => {
      if (c.path === "/api/llm/agents/test/stream") {
        return [200, { __sse: [
          frame("delta", { kind: "thinking", text: "weighing " }),
          frame("delta", { kind: "thinking", text: "the notes" }),
          frame("delta", { kind: "text", text: "I am " }),
          frame("delta", { kind: "text", text: "GLM." }),
          frame("tool", { name: "read", arguments: '{ "path": "notes.md" }' }),
          frame("toolResult", { name: "read", ok: true, text: "913 B" }),
          frame("final", { text: "I am GLM." }),
          frame("done", { ok: true, model: "glm-5.3-flash", text: "I am GLM.", ms: 900, turns: 3, tokensIn: 210, tokensOut: 64 }),
        ] }];
      }
      if (c.path !== "/api/llm/agents/test") return undefined;
      return (c.body || {}).agent === "cc"
        ? [200, { ok: true, program: "C:/Users/me/.local/bin/claude.exe", text: "2.3.0 (Claude Code)", env: ["ANTHROPIC_AUTH_TOKEN", "OTEL_LOGS_EXPORTER"], ms: 300 }]
        : [200, { ok: true, model: "glm-5.3-flash", text: "I am GLM.", session: "llm-1a2b", ms: 900,
            turns: 3, tokensIn: 210, tokensOut: 64 }];
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    expect(text("#sheet")).toContain("a reported run");
    // The builtin test says what its tools reach, and the loop's answer carries its
    // turn count and token totals (the dialogue itself files as a run with turns).
    expect(text("#sheet")).toContain("Select the tools");
    await click("#laTestRun");
    // The ask went to the LIVE route; the dialogue paints as the wire speaks.
    expect(sent("POST", "/api/llm/agents/test/stream").map((c) => c.body)).toEqual([{ agent: "distiller" }]);
    const live = text("#laTestOut");
    expect(live).toContain("weighing the notes");
    expect(live).toContain("I am GLM.");
    expect(live).toContain("read");
    expect(live).toContain("notes.md");
    expect(live).toContain("913 B");
    expect(live).toContain("3 rounds · 1 tool call · 210 in / 64 out");

    await click("[data-latest=cc]");
    expect(document.querySelector("#laTestPrompt")).toBe(null);
    expect(text("#laTestRun")).toBe("Check");
    await click("#laTestRun");
    expect(sent("POST", "/api/llm/agents/test")[0].body).toEqual({ agent: "cc" });
    expect(text("#laTestOut")).toContain("2.3.0 (Claude Code)");
    expect(text("#laTestOut")).toContain("ANTHROPIC_AUTH_TOKEN, OTEL_LOGS_EXPORTER");
  });

  it("a db agent serves the Data page alone: its prompt and four tools fixed, a turn cap, no secrets, no Test", async () => {
    row.agents = { ...row.agents, data: { runtime: "db", provider: "glm", effort: "medium" } };
    await agents.mount();
    expect(text("[data-larow=data] .lrow-sub")).toBe("DB · glm · glm-5.3-flash");
    expect(text("[data-larow=data]")).toContain("list_tables, describe_table, run_sql, stage_sql");
    expect(text("[data-larow=data] .lrow-detail")).toContain("System promptdefault" + DB_PROMPT);
    // It is asked on the Data page, on a connection: its row opens that page, not a Test.
    expect(document.querySelector("[data-latest=data]")).toBe(null);
    await click("[data-ladata=data]");
    expect(location.hash).toBe("#data");
    location.hash = "";
    await click("[data-lamore=data]");
    await menuPick("Edit");
    expect(q<HTMLSelectElement>("#laRuntime").value).toBe("db");
    expect(text("#laRuntimeWhy")).toContain("Serves the Data page alone");
    expect(text("#laPromptBase")).toBe(DB_PROMPT);
    expect(q("#laTurns").closest("[hidden]")).toBe(null);
    expect(q("#laSecrets").closest("[hidden]"), "no shell, no secrets").not.toBe(null);
    // The four tools are the page's: all taken, none to uncheck, none written.
    const boxes = [...document.querySelectorAll<HTMLInputElement>("#laTools [data-latool]")];
    expect(boxes.map((b) => [b.dataset.latool, b.checked, b.disabled])).toEqual([
      ["list_tables", true, true], ["describe_table", true, true], ["run_sql", true, true], ["stage_sql", true, true],
    ]);
    type("#laTurns", "12");
    await click("#laSave");
    expect((puts()[0].agents as Body).data).toEqual({ runtime: "db", provider: "glm", maxTurns: 12, effort: "medium" });
  });

  it("sent from the Data page to make one, the Add sheet opens once, preset to the db runtime and named db", async () => {
    const core = await import("../src/views/llm-core.js");
    core.presetAgent({ runtime: "db", name: "db" });
    await agents.mount();
    await settle();
    expect(q<HTMLInputElement>("#laName").value).toBe("db");
    expect(q<HTMLSelectElement>("#laRuntime").value).toBe("db");
    expect(q("#laCli").hidden).toBe(true);
    change("#laProvider", "glm");
    await click("#laSave");
    expect((puts()[0].agents as Body).db).toEqual({ runtime: "db", provider: "glm" });
    expect(core.takeAgentPreset(), "taken by that visit").toBe(null);
  });

  it("Delete takes the agent out of the row after a confirm", async () => {
    await agents.mount();
    await click("[data-lamore=cc]");
    await menuPick("Delete");
    expect(Object.keys(puts()[0].agents as Body)).toEqual(["distiller"]);
    expect(text("#toast")).toBe("Agent cc deleted");
  });
});

/* ------------------------------------------------------------------ The live dialogue
 * The conversation contract behind the Test sheet's stream route: the sheet
 * restores the agent's OPEN run from the history (the standard runs envelope
 * with open=true), the next ask names the run it continues, a provider failure
 * keeps the conversation while a 409 with a code locks asks until New closes
 * the run server-side, Stop cancels (route + abort + generation guard, never a
 * quiet success), and Enter asks. Realistic metadata throughout - model ids,
 * thinking and tool frames, token counts - the way a real provider answers. */

describe("the LLM plugin - Agents live dialogue", () => {
  it("keeps the composer outside the growing conversation's scroll area", async () => {
    await agents.mount();
    await click("[data-latest=distiller]");
    const prompt = q("#laTestPrompt");
    expect(prompt.closest(".sheet-body")).toBeNull();
    expect(prompt.closest(".sheet-compose")).toBeTruthy();
    expect(q("#laTestOut").closest(".sheet-body")).toBeTruthy();
    expect(prompt.closest(".sheet")?.contains(q("#laTestStop"))).toBe(true);
  });

  it("a newly sent long prompt collapses its context before any restore", async () => {
    await agents.mount();
    await click("[data-latest=distiller]");
    type("#laTestPrompt", "Read the project.\n" + Array.from({ length: 20 }, (_, i) => "## Context " + i + "\nSome supplied material.").join("\n"));
    await click("#laTestRun");
    const user = q("#laTestOut .tc-turn.user");
    const fold = user.querySelector<HTMLDetailsElement>("details");
    expect(fold).toBeTruthy();
    expect(fold!.open).toBe(false);
    expect(user.textContent).toContain("Context 19");
  });

  /** The agent's open run as the discovery read answers it (runs envelope). */
  const OPEN_RUN: Body = {
    id: 41, at: "2026-10-11T09:00:00.000Z", atEnd: null, agent: "distiller", runtime: "builtin",
    provider: "glm", model: "glm-5.3-flash", ok: null, ms: 940, turns: 4,
  };
  const OPEN_DIALOGUE: Body = {
    id: 41, at: "2026-10-11T09:00:00.000Z", atEnd: null, agent: "distiller", runtime: "builtin",
    provider: "glm", model: "glm-5.3-flash", ok: null, ms: 940,
    turns: [
      { seq: 1, at: "2026-10-11T09:00:00.000Z", role: "system", text: "You distill a session into memory." },
      { seq: 2, at: "2026-10-11T09:00:01.000Z", role: "user", text: "read the notes and file a proposal" },
      { seq: 3, at: "2026-10-11T09:00:02.000Z", role: "tool", text: "913 B" },
      { seq: 4, at: "2026-10-11T09:00:03.000Z", role: "final", text: "Filed." },
    ],
    calls: [],
  };
  const OPEN_GET = "/api/llm/history/runs?agent=distiller&open=true";
  const isStream = (c: Call): boolean => c.path === "/api/llm/agents/test/stream";

  it("restore: the agent's open run loads onto the column, and the next ask continues it by id", async () => {
    override = (c) => {
      if (c.method === "GET" && c.path.startsWith(OPEN_GET)) return [200, { runs: [OPEN_RUN], page: 0, pageSize: 1, more: false }];
      if (c.method === "GET" && c.path === "/api/llm/history/runs/41/dialogue") return [200, OPEN_DIALOGUE];
      if (c.method === "POST" && isStream(c)) return [200, { __sse: [
        sseFrame("run", { runId: 41, agent: "distiller", provider: "glm", model: "glm-5.3-flash", continued: true }),
        sseFrame("delta", { kind: "thinking", text: "the tunnels " }),
        sseFrame("tool", { name: "read", arguments: '{"path":"tunnels.md"}' }),
        sseFrame("toolResult", { name: "read", ok: true, text: "tunnel evidence" }),
        sseFrame("delta", { kind: "text", text: "Noted." }),
        sseFrame("final", { text: "Noted, and filed." }),
        sseFrame("done", { ok: true, runId: 41, ms: 140, turns: 6, tokensIn: 320, tokensOut: 18 }),
      ] }];
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    // Opening the sheet asks the history for the agent's open run, then its dialogue.
    expect(calls.some((c) => c.method === "GET" && c.path.startsWith(OPEN_GET))).toBe(true);
    expect(calls.some((c) => c.method === "GET" && c.path === "/api/llm/history/runs/41/dialogue")).toBe(true);
    const restored = text("#laTestOut");
    expect(restored).toContain("read the notes and file a proposal");
    expect(restored).toContain("Filed.");
    // The continued ask names the run it continues, and the column says so.
    type("#laTestPrompt", "and the tunnels?");
    await click("#laTestRun");
    expect(sent("POST", "/api/llm/agents/test/stream").map((c) => c.body)).toEqual([{ agent: "distiller", prompt: "and the tunnels?", runId: 41 }]);
    expect(document.querySelector("[data-laresume]")?.textContent || "").toContain("41");
    // The final frame swaps the streamed text for the canonical answer - the column
    // ends with the whole sentence, not the delta's prefix.
    expect(text("#laTestOut")).toContain("Noted, and filed.");
    // New live tools appended to a restored column own their click exactly once.
    const tool = q("#laTestOut [data-latool=read]");
    (tool.closest("details") as HTMLDetailsElement).open = true;
    await click(tool.querySelector(".fl-sum")!);
    expect(tool.querySelector(".fl-step.open .fl-body")?.textContent).toContain("tunnel evidence");
    await click(tool.querySelector(".fl-sum")!);
    expect(tool.querySelector(".fl-step.open")).toBeNull();
  });

  it("restore: a run another surface holds (the Data agent's, under a task) is not the Test's conversation", async () => {
    override = (c) => {
      if (c.method === "GET" && c.path.startsWith(OPEN_GET)) return [200, { runs: [
        { ...OPEN_RUN, id: 77, task: "data.agent", subject: "mattermost" }, { ...OPEN_RUN, task: null },
      ], page: 0, pageSize: 20, more: false }];
      if (c.method === "GET" && c.path === "/api/llm/history/runs/41/dialogue") return [200, OPEN_DIALOGUE];
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    expect(calls.some((c) => c.path === "/api/llm/history/runs/77/dialogue"), "the Data agent's run is left alone").toBe(false);
    expect(text("#laTestOut")).toContain("read the notes and file a proposal");
  });

  it("a round's words stand in the work where they were said, the final answer below it", async () => {
    // Each round thinks, says a line, then asks its tool: the line lands between that
    // round's thinking and its tool ask, never piled up below all the work.
    override = (c) => {
      if (c.method === "POST" && isStream(c)) return [200, { __sse: [
        sseFrame("run", { runId: 52, agent: "distiller", provider: "glm", model: "glm-5.3-flash", continued: false }),
        sseFrame("delta", { kind: "thinking", text: "the suite first " }),
        sseFrame("delta", { kind: "text", text: "I'll count them with " }),
        sseFrame("delta", { kind: "text", text: "`cargo`:" }),
        sseFrame("tool", { name: "powershell", arguments: '{"command":"cargo test -p swiss-remote -- --list","description":"List the remote tests"}' }),
        sseFrame("toolResult", { name: "powershell", ok: true, text: "41 tests" }),
        sseFrame("delta", { kind: "thinking", text: "and the sources " }),
        sseFrame("delta", { kind: "text", text: "Two more files." }),
        sseFrame("tool", { name: "read", arguments: '{"path":"src/lib.rs"}' }),
        sseFrame("toolResult", { name: "read", ok: true, text: "12 tests inline" }),
        sseFrame("delta", { kind: "text", text: "All done." }),
        sseFrame("final", { text: "All **done**." }),
        sseFrame("done", { ok: true, runId: 52, ms: 3200, turns: 3, tokensIn: 900, tokensOut: 120 }),
      ] }];
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    type("#laTestPrompt", "how many tests?");
    await click("#laTestRun");
    /** The innermost transcript turn holding the words: exactly one. */
    const turnOf = (needle: string): Element => {
      const hits = [...document.querySelectorAll("#laTestOut .tc-turn")].filter((t) => (t.textContent || "").includes(needle));
      const inner = hits.filter((t) => !hits.some((o) => o !== t && t.contains(o)));
      expect(inner).toHaveLength(1);
      return inner[0];
    };
    const precedes = (a: Node, b: Node): boolean => (a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;
    const first = turnOf("I'll count them with");
    const second = turnOf("Two more files.");
    const shell = q("#laTestOut [data-latool=powershell]");
    const read = q("#laTestOut [data-latool=read]");
    const work = shell.closest("details") as HTMLDetailsElement;
    expect(work.contains(first) && work.contains(second) && work.contains(read)).toBe(true);
    expect(precedes(first, shell) && precedes(shell, second) && precedes(second, read)).toBe(true);
    // A round that moved on renders its words as the markdown they are.
    expect(first.querySelector("code")?.textContent).toBe("cargo");
    // The closing round's streamed words are lifted out: the final stands once, below the work.
    const answer = turnOf("All done.");
    expect(work.contains(answer)).toBe(false);
    expect(precedes(work, answer)).toBe(true);
    expect(answer.querySelector("strong")?.textContent).toBe("done");
    // A call that says what it does reads as that, in prose; the command stays one click away.
    expect(shell.querySelector(".fl-summary")?.textContent).toBe("List the remote tests");
    expect(shell.querySelector(".fl-summary")?.classList.contains("mono")).toBe(false);
    expect(shell.querySelector(".fl-body")?.textContent).toContain("cargo test -p swiss-remote -- --list");
    expect(read.querySelector(".fl-summary")?.textContent).toBe("src/lib.rs");
    expect(read.querySelector(".fl-summary")?.classList.contains("mono")).toBe(true);
    // Each round's thinking is its own Think line, as dsh shows it: the brain, Think and the
    // thought's first line, the whole thought one click away; settled once the words came.
    const thoughts = [...document.querySelectorAll<HTMLElement>("#laTestOut [data-lathink]")];
    expect(thoughts).toHaveLength(2);
    expect(thoughts.map((t) => t.querySelector(".fl-summary")?.textContent)).toEqual(["the suite first", "and the sources"]);
    expect(thoughts[0].querySelector(".fl-title")?.textContent).toBe("Think");
    expect(thoughts[0].querySelector(".fl-glyph use")?.getAttribute("href")).toBe("#i-brain");
    expect(thoughts.some((t) => t.querySelector(".fl-step.live"))).toBe(false);
    expect(thoughts[0].querySelector(".fl-meta")?.textContent).toMatch(/\d/);
    expect(precedes(thoughts[0], first) && precedes(shell, thoughts[1]) && precedes(thoughts[1], second)).toBe(true);
    // No role word above a work line: the line names itself.
    expect(thoughts[0].closest(".tc-turn")?.querySelector(".tc-role")?.textContent).toBe("");
    expect(shell.closest(".tc-turn")?.querySelector(".tc-role")?.textContent).toBe("");
    await click(thoughts[0].querySelector(".fl-sum")!);
    expect(thoughts[0].querySelector(".fl-step")?.classList.contains("open")).toBe(true);
    expect(thoughts[0].querySelector(".fl-body .tc-text.dim")?.textContent).toBe("the suite first ");
  });

  it("a thought streams into its Think line: the first line on the row, live until the next frame", async () => {
    const live = liveBody();
    override = (c) => {
      if (c.method === "POST" && isStream(c)) return [200, { __sseLive: live.body }];
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    await click("#laTestRun");
    live.push("run", { runId: 53, agent: "distiller", continued: false });
    live.push("delta", { kind: "thinking", text: "The user wants " });
    live.push("delta", { kind: "thinking", text: "the port.\nFirst the config" });
    await settle();
    const thought = q("#laTestOut [data-lathink]");
    expect(thought.querySelector(".fl-summary")?.textContent).toBe("The user wants the port.");
    expect(thought.querySelector(".fl-step")?.classList.contains("live")).toBe(true);
    live.push("delta", { kind: "thinking", text: ", then the code." });
    await settle();
    expect(thought.querySelector(".fl-summary")?.textContent).toBe("The user wants the port.");
    expect(thought.querySelector(".fl-body .tc-text")?.textContent).toBe("The user wants the port.\nFirst the config, then the code.");
    live.push("tool", { name: "read", arguments: '{"path":"gateway.toml"}' });
    await settle();
    expect(thought.querySelector(".fl-step")?.classList.contains("live")).toBe(false);
    expect(document.querySelectorAll("#laTestOut [data-lathink]")).toHaveLength(1);
    live.end();
    await settle();
  });

  it("a restore that lands after a new ask started never overwrites the streaming dialogue", async () => {
    // The old dialogue's read is slow; the user asks and the new Q&A streams onto
    // the column. When the old dialogue finally lands it must paint nothing: the
    // streamed words stay, and the conversation still continues the live run.
    const d = deferred();
    const stream = liveBody();
    override = (c) => {
      if (c.method === "GET" && c.path.startsWith(OPEN_GET)) return [200, { runs: [OPEN_RUN], page: 0, pageSize: 1, more: false }];
      if (c.method === "GET" && c.path === "/api/llm/history/runs/41/dialogue") return [200, { __defer: d.promise }];
      if (c.method === "POST" && isStream(c)) return [200, { __sseLive: stream.body }];
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    // The discovery landed (the ask continues run 41), the dialogue read is held.
    type("#laTestPrompt", "what changed?");
    await click("#laTestRun");
    expect(sent("POST", "/api/llm/agents/test/stream")[0].body).toMatchObject({ runId: 41 });
    stream.push("run", { runId: 41, agent: "distiller", continued: true });
    stream.push("delta", { kind: "text", text: "Streaming " });
    stream.push("delta", { kind: "text", text: "answer." });
    await settle();
    expect(text("#laTestOut")).toContain("Streaming answer.");
    // The old dialogue lands NOW: the column keeps the streamed turns, and none of
    // the old words overwrite them.
    d.resolve(OPEN_DIALOGUE);
    await settle();
    expect(text("#laTestOut")).toContain("Streaming answer.");
    expect(text("#laTestOut")).not.toContain("Filed.");
    // The conversation continues the live run, and the done frame closes it out.
    stream.push("final", { text: "Streaming answer." });
    stream.push("done", { ok: true, runId: 41, ms: 10, turns: 2, tokensIn: 5, tokensOut: 5 });
    stream.end();
    await settle();
    type("#laTestPrompt", "next?");
    await click("#laTestRun");
    expect(sent("POST", "/api/llm/agents/test/stream")[1].body).toMatchObject({ runId: 41 });
  });
  it("a delayed discovery cannot replace the run a completed ask established", async () => {
    const discovery = deferred();
    override = (c) => {
      if (c.method === "GET" && c.path.startsWith(OPEN_GET)) return [200, { __defer: discovery.promise }];
      if (c.method === "GET" && c.path === "/api/llm/history/runs/41/dialogue") return [200, OPEN_DIALOGUE];
      if (c.method === "POST" && isStream(c)) return [200, { __sse: [
        sseFrame("run", { runId: 44, agent: "distiller", continued: false }),
        sseFrame("final", { text: "The current answer." }),
        sseFrame("done", { ok: true, runId: 44, ms: 9, turns: 2, tokensIn: 9, tokensOut: 2 }),
      ] }];
      return undefined;
    };
    try {
      await agents.mount();
      await click("[data-latest=distiller]");
      type("#laTestPrompt", "a fresh question");
      await click("#laTestRun");
      expect(sent("POST", "/api/llm/agents/test/stream")[0].body).not.toHaveProperty("runId");
      expect(q<HTMLButtonElement>("#laTestRun").disabled).toBe(false);
      discovery.resolve({ runs: [OPEN_RUN], page: 0, pageSize: 1, more: false });
      await settle();
      type("#laTestPrompt", "continue the current answer");
      await click("#laTestRun");
      expect(sent("POST", "/api/llm/agents/test/stream")[1].body).toMatchObject({ runId: 44 });
      expect(text("#laTestOut")).toContain("The current answer.");
      expect(calls.some((c) => c.path === "/api/llm/history/runs/41/dialogue")).toBe(false);
    } finally {
      discovery.resolve({});
    }
  });

  it("restore: no open run is a clean column, and a closed store never blocks a fresh ask", async () => {
    override = (c) => {
      if (c.method === "GET" && c.path.startsWith(OPEN_GET)) return [200, { runs: [], page: 0, pageSize: 1, more: false }];
      if (c.method === "POST" && isStream(c)) return [200, { __sse: [
        sseFrame("run", { runId: 42, agent: "distiller", continued: false }),
        sseFrame("final", { text: "Fresh answer." }),
        sseFrame("done", { ok: true, runId: 42, ms: 20, turns: 2, tokensIn: 40, tokensOut: 4 }),
      ] }];
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    expect(calls.some((c) => c.method === "GET" && c.path.startsWith(OPEN_GET))).toBe(true);
    expect(calls.some((c) => c.path.includes("/dialogue"))).toBe(false);
    await click("#laTestRun");
    expect(sent("POST", "/api/llm/agents/test/stream").map((c) => c.body)).toEqual([{ agent: "distiller" }]);

    // The store itself closed (plugin off): the sheet says so, and a fresh ask still goes out.
    await click("#laTestClose");
    override = (c) => {
      if (c.method === "GET" && c.path.startsWith(OPEN_GET)) return [503, { error: "the llm history is not open" }];
      if (c.method === "POST" && isStream(c)) return [200, { __sse: [
        sseFrame("final", { text: "Still asks." }),
        sseFrame("done", { ok: true, runId: 43, ms: 5, turns: 1, tokensIn: 9, tokensOut: 2 }),
      ] }];
      return undefined;
    };
    await click("[data-latest=distiller]");
    expect(text("#laTestOut")).toContain("not open");
    await click("#laTestRun");
    expect(sent("POST", "/api/llm/agents/test/stream").length).toBe(2);
  });

  it("a provider failure in the stream keeps the conversation: the retry carries the same run id", async () => {
    let fail = true;
    override = (c) => {
      if (c.method === "POST" && isStream(c)) {
        return fail
          ? [200, { __sse: [
              sseFrame("run", { runId: 41, agent: "distiller", continued: false }),
              sseFrame("delta", { kind: "text", text: "Half " }),
              sseFrame("done", { ok: false, runId: 41, error: "the provider answered 500", code: "provider" }),
            ] }]
          : [200, { __sse: [
              sseFrame("run", { runId: 41, agent: "distiller", continued: true }),
              sseFrame("final", { text: "Whole answer." }),
              sseFrame("done", { ok: true, runId: 41, ms: 30, turns: 2, tokensIn: 60, tokensOut: 5 }),
            ] }];
      }
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    await click("#laTestRun");
    expect(text("#laTestOut")).toContain("the provider answered 500");
    // A failed ask is a failed turn, not a dead conversation: Ask stays armed...
    expect(q<HTMLButtonElement>("#laTestRun").disabled).toBe(false);
    fail = false;
    await click("#laTestRun");
    expect(sent("POST", "/api/llm/agents/test/stream")[1].body).toMatchObject({ runId: 41 });
    expect(text("#laTestOut")).toContain("Whole answer.");
  });

  it("a dead conversation (409 with a code) locks the asks until New closes the run server-side", async () => {
    let dead = false;
    override = (c) => {
      if (c.method === "POST" && isStream(c)) {
        if (!dead) return [200, { __sse: [
          sseFrame("run", { runId: 41, agent: "distiller", continued: false }),
          sseFrame("final", { text: "First exchange done." }),
          sseFrame("done", { ok: true, runId: 41, ms: 50, turns: 2, tokensIn: 30, tokensOut: 4 }),
        ] }];
        return [409, { error: "run 41 is closed", code: "closed" }];
      }
      if (c.method === "POST" && c.path === "/api/llm/history/runs/41/close") return [200, { ok: true }];
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    await click("#laTestRun");
    dead = true;
    await click("#laTestRun");
    // The permanent refusal is said out loud and the asks are locked...
    expect(text("#laTestOut")).toContain("run 41 is closed");
    expect(q<HTMLButtonElement>("#laTestRun").disabled).toBe(true);
    // ...until New closes the run on the server and clears the column.
    await click("#laTestFresh");
    expect(sent("POST", "/api/llm/history/runs/41/close").length).toBe(1);
    expect(text("#laTestOut")).toBe("");
    expect(q<HTMLButtonElement>("#laTestRun").disabled).toBe(false);
    dead = false;
    await click("#laTestRun");
    expect(sent("POST", "/api/llm/agents/test/stream")[2].body).toEqual({ agent: "distiller" });
  });

  it("New on a run the server no longer has clears the column and says why", async () => {
    override = (c) => {
      if (c.method === "GET" && c.path.startsWith(OPEN_GET)) return [200, { runs: [OPEN_RUN], page: 0, pageSize: 1, more: false }];
      if (c.method === "GET" && c.path === "/api/llm/history/runs/41/dialogue") return [200, OPEN_DIALOGUE];
      if (c.method === "POST" && c.path === "/api/llm/history/runs/41/close") return [404, { error: "run 41 is not in the llm history" }];
      if (c.method === "POST" && isStream(c)) return [200, { __sse: [
        sseFrame("final", { text: "Fresh." }),
        sseFrame("done", { ok: true, runId: 44, ms: 4, turns: 1, tokensIn: 6, tokensOut: 1 }),
      ] }];
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    expect(text("#laTestOut")).toContain("Filed.");
    await click("#laTestFresh");
    expect(sent("POST", "/api/llm/history/runs/41/close").length).toBe(1);
    expect(text("#laTestOut")).toBe("");
    expect(document.body.textContent || "").toContain("run 41 is not in the llm history");
    await click("#laTestRun");
    expect(sent("POST", "/api/llm/agents/test/stream")[0].body).toEqual({ agent: "distiller" });
  });

  it("Stop cancels the live loop: the cancel route is called, the stream aborts, and the column never claims success", async () => {
    const live = liveBody();
    override = (c) => {
      if (c.method === "POST" && isStream(c)) return [200, { __sseLive: live.body }];
      if (c.method === "POST" && c.path === "/api/llm/agents/test/cancel") return [200, { ok: true, cancelled: true }];
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    await click("#laTestRun");
    live.push("run", { runId: 41, agent: "distiller", continued: false });
    live.push("delta", { kind: "thinking", text: "weighing " });
    await settle();
    // Live: Ask turns Queue (armed: words wait for the next step), Stop is armed.
    expect(text("#laTestRun")).toBe("Queue");
    expect(q<HTMLButtonElement>("#laTestRun").disabled).toBe(false);
    expect(q<HTMLButtonElement>("#laTestStop").disabled).toBe(false);
    await click("#laTestStop");
    expect(sent("POST", "/api/llm/agents/test/cancel").map((c) => c.body)).toEqual([{ runId: 41 }]);
    const streamSignal = signals.filter((s) => s.path === "/api/llm/agents/test/stream").pop();
    expect(streamSignal?.signal.aborted).toBe(true);
    // A cancelled loop says cancelled - the success meta (N rounds) never paints.
    expect(document.querySelector("[data-lacancelled]")).not.toBe(null);
    expect(document.querySelector("[data-lacancelled] .tc-who"), "the cancelled line carries no speaker mark").toBe(null);
    expect(text("#laTestOut")).not.toMatch(/\d+ rounds?\b/);
    // A late frame from the cancelled loop paints nothing.
    live.push("delta", { kind: "text", text: "ZOMBIE" });
    live.end();
    await settle();
    expect(text("#laTestOut")).not.toContain("ZOMBIE");
    expect(q<HTMLButtonElement>("#laTestRun").disabled).toBe(false);
    expect(text("#laTestRun")).toBe("Ask");
  });

  it("Stop lives only while the loop runs: a finished run is never cancelled, and New is dead while live", async () => {
    const live = liveBody();
    override = (c) => {
      if (c.method === "POST" && isStream(c)) return [200, { __sseLive: live.body }];
      if (c.method === "POST" && c.path === "/api/llm/history/runs/41/close") return [200, { ok: true }];
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    await click("#laTestRun");
    live.push("run", { runId: 41, agent: "distiller", continued: false });
    await settle();
    expect(q<HTMLButtonElement>("#laTestStop").disabled).toBe(false);
    // v1: New cannot abandon a live loop - it is dead until the loop is idle.
    expect(q<HTMLButtonElement>("#laTestFresh").disabled).toBe(true);
    live.push("final", { text: "Done answer." });
    live.push("done", { ok: true, runId: 41, ms: 60, turns: 2, tokensIn: 40, tokensOut: 6 });
    live.end();
    await settle();
    const stopAfter = document.querySelector<HTMLButtonElement>("#laTestStop");
    expect(stopAfter === null || stopAfter.disabled).toBe(true);
    expect(sent("POST", "/api/llm/agents/test/cancel").length).toBe(0);
    expect(text("#laTestOut")).toContain("2 rounds · 40 in / 6 out");
    // Idle: New closes the open run on the server before clearing the column.
    await click("#laTestFresh");
    expect(sent("POST", "/api/llm/history/runs/41/close").length).toBe(1);
    expect(text("#laTestOut")).toBe("");
  });

  const STEER = "/api/llm/agents/test/steer";
  const queuedLines = (): number => document.querySelectorAll("#laTestQueued .chat-queue-row").length;
  const queuedTexts = (): string[] => [...document.querySelectorAll("#laTestQueued .chat-queue-text")].map((t) => t.textContent ?? "");
  /** A queued row's button by its label: Edit, Delete or Withdraw, or steer-now. */
  const rowBtn = (words: string, label: string): HTMLButtonElement => {
    const row = [...document.querySelectorAll<HTMLElement>("#laTestQueued .chat-queue-row")].find((r) => r.querySelector(".chat-queue-text")?.textContent === words);
    return row!.querySelector<HTMLButtonElement>("button[aria-label='" + label + "']")!;
  };
  const STEER_NOW = "Steer now: the answer reads it before its next step";

  it("while a reply streams Ask is Queue: words wait in the queue, a steered one posts once the run is named, and a steer frame paints it as the reader's turn", async () => {
    const live = liveBody();
    override = (c) => {
      if (c.method === "POST" && isStream(c)) return [200, { __sseLive: live.body }];
      if (c.method === "POST" && c.path === STEER) return [200, { ok: true, queued: true }];
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    type("#laTestPrompt", "first question");
    await click("#laTestRun");
    expect(text("#laTestRun")).toBe("Queue");
    expect(q<HTMLButtonElement>("#laTestRun").disabled).toBe(false);
    expect(q("#laTestRun").title).toContain("next turn");
    expect(q<HTMLTextAreaElement>("#laTestPrompt").placeholder).toContain("Ctrl+Enter");
    // An empty Queue queues nothing (the default prompt is for an ask, not a queue).
    await click("#laTestRun");
    expect(queuedLines()).toBe(0);
    type("#laTestPrompt", "also list the ports");
    await click("#laTestRun");
    expect(q<HTMLTextAreaElement>("#laTestPrompt").value).toBe("");
    expect(queuedTexts()).toEqual(["also list the ports"]);
    expect(text("#laTestQueued")).toContain("1 queued message");
    // Queued words wait: nothing posts until the reader steers them.
    expect(sent("POST", STEER).length).toBe(0);
    // Steered before the run frame, it posts with that frame: the server seats the run first.
    await click(rowBtn("also list the ports", STEER_NOW));
    expect(sent("POST", STEER).length).toBe(0);
    live.push("run", { runId: 41, agent: "distiller", continued: false });
    await settle();
    const first = sent("POST", STEER).map((c) => c.body);
    expect(first.length).toBe(1);
    const id = String(first[0]?.id);
    expect(first[0]).toEqual({ runId: 41, id, text: "also list the ports" });
    // Enter queues too; Ctrl+Enter steers every waiting one now.
    type("#laTestPrompt", "and the pids");
    q("#laTestPrompt").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    await settle();
    expect(sent("POST", STEER).length).toBe(1);
    q("#laTestPrompt").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true, cancelable: true }));
    await settle();
    expect(sent("POST", STEER).map((c) => c.body?.text)).toEqual(["also list the ports", "and the pids"]);
    expect(sent("POST", "/api/llm/agents/test/stream").length).toBe(1);
    // The loop took the first after the tool answered: its row leaves and the column
    // carries it as the reader's turn; the work before it folds closed.
    live.push("tool", { name: "read", arguments: "{\"path\":\"a\"}" });
    live.push("toolResult", { name: "read", text: "ok", ok: true });
    live.push("steer", { id, text: "also list the ports" });
    await settle();
    expect(queuedTexts()).toEqual(["and the pids"]);
    const steered = q("[data-lasteered]");
    expect(steered.textContent).toContain("also list the ports");
    expect([...document.querySelectorAll<HTMLDetailsElement>("#laTestOut details")].every((d) => !d.open)).toBe(true);
    // The next step opens its own fold beneath the reader's words.
    live.push("tool", { name: "list", arguments: "{}" });
    await settle();
    expect(steered.nextElementSibling?.querySelector<HTMLDetailsElement>("details")?.open).toBe(true);
    live.end();
    await settle();
  });

  it("after a clean answer the first queued message is the next ask, by itself; Withdraw takes a steered one back, Edit rewrites one", async () => {
    const live = liveBody();
    let streams = 0;
    override = (c) => {
      if (c.method === "POST" && isStream(c)) {
        streams++;
        if (streams === 1) return [200, { __sseLive: live.body }];
        return [200, { __sse: [
          sseFrame("run", { runId: 41, agent: "distiller", continued: true }),
          sseFrame("final", { text: streams === 2 ? "Ports." : "Pids too." }),
          sseFrame("done", { ok: true, runId: 41, ms: 9, turns: 1, tokensIn: 9, tokensOut: 2 }),
        ] }];
      }
      if (c.method === "POST" && c.path === STEER) return [200, { ok: true, queued: true }];
      if (c.method === "POST" && c.path === STEER + "/withdraw") return [200, { ok: true, withdrawn: true }];
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    await click("#laTestRun");
    live.push("run", { runId: 41, agent: "distiller", continued: false });
    await settle();
    for (const words of ["list the ports", "drop this one", "and the pids"]) {
      type("#laTestPrompt", words);
      await click("#laTestRun");
    }
    expect(queuedTexts()).toEqual(["list the ports", "drop this one", "and the pids"]);
    expect(sent("POST", STEER).length).toBe(0);
    // ↑ puts one first and steers it; Withdraw takes it back and the inbox is told.
    await click(rowBtn("drop this one", STEER_NOW));
    expect(queuedTexts()).toEqual(["drop this one", "list the ports", "and the pids"]);
    const ids = sent("POST", STEER).map((c) => String(c.body?.id));
    expect(ids.length).toBe(1);
    await click(rowBtn("drop this one", "Withdraw"));
    expect(queuedTexts()).toEqual(["list the ports", "and the pids"]);
    expect(sent("POST", STEER + "/withdraw").map((c) => c.body)).toEqual([{ runId: 41, id: ids[0] }]);
    // Edit rewrites one in place; its Enter stays in the edit.
    await click(rowBtn("and the pids", "Edit"));
    const edit = q<HTMLTextAreaElement>("#laTestQueued textarea.chat-queue-edit");
    edit.value = "and the pids too";
    edit.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    await settle();
    expect(queuedTexts()).toEqual(["list the ports", "and the pids too"]);
    expect(sent("POST", STEER).length).toBe(1);
    // The reply ends: the first goes as the next ask on its own, continuing the run, and after
    // its clean answer the next one.
    live.push("final", { text: "First answer." });
    live.push("done", { ok: true, runId: 41, ms: 9, turns: 1, tokensIn: 9, tokensOut: 2 });
    live.end();
    for (let i = 0; i < 4; i++) await settle();
    expect(sent("POST", "/api/llm/agents/test/stream").slice(1).map((c) => c.body)).toEqual([
      { agent: "distiller", prompt: "list the ports", runId: 41 },
      { agent: "distiller", prompt: "and the pids too", runId: 41 },
    ]);
    expect(queuedLines()).toBe(0);
    expect(text("#laTestOut")).toContain("Ports.");
    expect(text("#laTestOut")).toContain("Pids too.");
    expect(text("#laTestRun")).toBe("Ask");
  });

  it("Stop or a failed reply keeps the queue for the reader, sending nothing; ↑ with nothing streaming sends one", async () => {
    let live = liveBody();
    override = (c) => {
      if (c.method === "POST" && isStream(c)) return [200, { __sseLive: live.body }];
      // A refused post stays local: what the loop never took is never lost.
      if (c.method === "POST" && c.path === STEER) return [409, { error: "that conversation is not streaming: send it as the next turn", code: "idle" }];
      if (c.method === "POST" && c.path === "/api/llm/agents/test/cancel") return [200, { ok: true, cancelled: true }];
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    await click("#laTestRun");
    live.push("run", { runId: 41, agent: "distiller", continued: false });
    await settle();
    type("#laTestPrompt", "queued words");
    await click("#laTestRun");
    await click(rowBtn("queued words", STEER_NOW));
    expect(sent("POST", STEER).length).toBe(1);
    expect(document.querySelectorAll("#laTestQueued [data-steered]").length).toBe(1);
    type("#laTestPrompt", "half typed");
    await click("#laTestStop");
    // The queue stays, its marks off; the prompt box is left alone.
    expect(q<HTMLTextAreaElement>("#laTestPrompt").value).toBe("half typed");
    expect(queuedTexts()).toEqual(["queued words"]);
    expect(document.querySelectorAll("#laTestQueued [data-steered]").length).toBe(0);
    expect(text("#laTestRun")).toBe("Ask");
    live.end();
    await settle();
    expect(sent("POST", "/api/llm/agents/test/stream").length).toBe(1);

    // ↑ with nothing streaming sends that one now, as the ask.
    live = liveBody();
    await click(rowBtn("queued words", STEER_NOW));
    expect(sent("POST", "/api/llm/agents/test/stream")[1].body).toEqual({ agent: "distiller", prompt: "queued words", runId: 41 });
    expect(queuedLines()).toBe(0);
    live.push("run", { runId: 41, agent: "distiller", continued: true });
    await settle();
    // A reply that fails keeps the conversation and the queue too.
    type("#laTestPrompt", "after the failure");
    await click("#laTestRun");
    live.push("done", { ok: false, runId: 41, error: "the provider answered 500", code: "provider" });
    live.end();
    await settle();
    await settle();
    expect(queuedTexts()).toEqual(["after the failure"]);
    expect(q<HTMLTextAreaElement>("#laTestPrompt").value).toBe("");
    expect(sent("POST", "/api/llm/agents/test/stream").length).toBe(2);
  });

  it("a stream that dies without a done frame is an error, never a quiet success", async () => {
    let whole = false;
    override = (c) => {
      if (c.method === "POST" && isStream(c)) {
        if (whole) return [200, { __sse: [
          sseFrame("run", { runId: 41, agent: "distiller", continued: true }),
          sseFrame("final", { text: "Whole." }),
          sseFrame("done", { ok: true, runId: 41, ms: 9, turns: 2, tokensIn: 20, tokensOut: 3 }),
        ] }];
        const dying = liveBody();
        dying.push("run", { runId: 41, agent: "distiller", continued: false });
        dying.push("delta", { kind: "text", text: "Half an " });
        dying.end();
        return [200, { __sseLive: dying.body }];
      }
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    await click("#laTestRun");
    expect(text("#laTestOut")).toContain("ended without an answer");
    expect(text("#laTestOut")).not.toMatch(/\d+ rounds?\b/);
    // The half-said conversation survives: Ask stays armed and still names the run.
    expect(q<HTMLButtonElement>("#laTestRun").disabled).toBe(false);
    whole = true;
    await click("#laTestRun");
    expect(sent("POST", "/api/llm/agents/test/stream")[1].body).toMatchObject({ runId: 41 });
  });

  it("a pre-stream busy refusal is recoverable: Ask stays armed and the retry names the same run", async () => {
    let busy = false;
    override = (c) => {
      if (c.method === "POST" && isStream(c)) {
        if (busy) return [409, { error: "another ask is already running for run 41", code: "busy" }];
        return [200, { __sse: [
          sseFrame("run", { runId: 41, agent: "distiller", continued: false }),
          sseFrame("final", { text: "First." }),
          sseFrame("done", { ok: true, runId: 41, ms: 10, turns: 2, tokensIn: 20, tokensOut: 3 }),
        ] }];
      }
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    await click("#laTestRun");
    busy = true;
    await click("#laTestRun");
    // Busy is the OTHER live class: said out loud, but the conversation is alive...
    expect(text("#laTestOut")).toContain("another ask is already running");
    expect(q<HTMLButtonElement>("#laTestRun").disabled).toBe(false);
    busy = false;
    await click("#laTestRun");
    expect(sent("POST", "/api/llm/agents/test/stream")[2].body).toMatchObject({ runId: 41 });
  });

  it("an in-stream cancelled done paints cancelled - not failure, not success - and keeps the conversation", async () => {
    let whole = false;
    override = (c) => {
      if (c.method === "POST" && isStream(c)) {
        if (whole) return [200, { __sse: [
          sseFrame("run", { runId: 41, agent: "distiller", continued: true }),
          sseFrame("final", { text: "Whole." }),
          sseFrame("done", { ok: true, runId: 41, ms: 8, turns: 2, tokensIn: 12, tokensOut: 2 }),
        ] }];
        return [200, { __sse: [
          sseFrame("run", { runId: 41, agent: "distiller", continued: false }),
          sseFrame("delta", { kind: "text", text: "Half " }),
          sseFrame("done", { ok: false, runId: 41, cancelled: true }),
        ] }];
      }
      return undefined;
    };
    await agents.mount();
    await click("[data-latest=distiller]");
    await click("#laTestRun");
    // The cancelled marker, and neither the failure word nor the success meta.
    expect(document.querySelector("[data-lacancelled]")).not.toBe(null);
    expect(document.querySelector("[data-lacancelled] .tc-who"), "the cancelled line carries no speaker mark").toBe(null);
    expect(text("#laTestOut")).not.toMatch(/\d+ rounds?\b/);
    expect(q<HTMLButtonElement>("#laTestRun").disabled).toBe(false);
    whole = true;
    await click("#laTestRun");
    expect(sent("POST", "/api/llm/agents/test/stream")[1].body).toMatchObject({ runId: 41 });
  });

  it("restore race: New closes the run discovery found while its dialogue is still loading", async () => {
    const dial = deferred();
    override = (c) => {
      if (c.method === "GET" && c.path.startsWith(OPEN_GET)) return [200, { runs: [OPEN_RUN], page: 0, pageSize: 1, more: false }];
      if (c.method === "GET" && c.path === "/api/llm/history/runs/41/dialogue") return [200, { __defer: dial.promise }];
      if (c.method === "POST" && c.path === "/api/llm/history/runs/41/close") return [200, { ok: true }];
      if (c.method === "POST" && isStream(c)) return [200, { __sse: [
        sseFrame("run", { runId: 42, agent: "distiller", continued: false }),
        sseFrame("final", { text: "M-A2-fresh." }),
        sseFrame("done", { ok: true, runId: 42, ms: 9, turns: 2, tokensIn: 9, tokensOut: 2 }),
      ] }];
      return undefined;
    };
    try {
      await agents.mount();
      await click("[data-latest=distiller]");
      await settle();
      // Discovery named run 41 and its dialogue is still loading; the reader hits New
      // anyway. The sheet KNOWS an open run exists - New must close THAT id.
      await click("#laTestFresh");
      expect(sent("POST", "/api/llm/history/runs/41/close").length).toBe(1);
      expect(text("#laTestOut")).not.toContain("file a proposal");
      // The late dialogue paints nothing and arms nothing.
      dial.resolve(OPEN_DIALOGUE);
      await settle();
      expect(text("#laTestOut")).not.toContain("file a proposal");
      await type("#laTestPrompt", "after new");
      await click("#laTestRun");
      await settle();
      expect(sent("POST", "/api/llm/agents/test/stream").at(-1)?.body).toEqual({ agent: "distiller", prompt: "after new" });
      expect(text("#laTestOut")).toContain("M-A2-fresh.");
    } finally {
      dial.resolve({});
    }
  });

  it("restore race: a restore that resolves after New never resurrects the closed conversation", async () => {
    const disc = deferred();
    const dial = deferred();
    let fresh = false;
    override = (c) => {
      if (c.method === "GET" && c.path.startsWith(OPEN_GET)) return [200, { __defer: disc.promise }];
      if (c.method === "GET" && c.path === "/api/llm/history/runs/41/dialogue") return [200, { __defer: dial.promise }];
      if (c.method === "POST" && c.path === "/api/llm/history/runs/41/close") return [200, { ok: true }];
      if (c.method === "POST" && isStream(c)) {
        if (fresh) return [200, { __sse: [
          sseFrame("run", { runId: 42, agent: "distiller", continued: false }),
          sseFrame("final", { text: "M-B2-fresh." }),
          sseFrame("done", { ok: true, runId: 42, ms: 9, turns: 2, tokensIn: 9, tokensOut: 2 }),
        ] }];
        return [200, { __sse: [
          sseFrame("run", { runId: 41, agent: "distiller", continued: false }),
          sseFrame("final", { text: "M-B1-first." }),
          sseFrame("done", { ok: true, runId: 41, ms: 9, turns: 2, tokensIn: 9, tokensOut: 2 }),
        ] }];
      }
      return undefined;
    };
    try {
      await agents.mount();
      await click("[data-latest=distiller]");
      // The first ask does not wait for discovery: the server reserves and names run 41
      // itself - the run frame is the real place the closure gets the id to close.
      await type("#laTestPrompt", "M-B1");
      await click("#laTestRun");
      await settle();
      expect(text("#laTestOut")).toContain("M-B1-first.");
      // The Ask already owns the run: late discovery is retired before a dialogue read.
      disc.resolve({ runs: [OPEN_RUN], page: 0, pageSize: 1, more: false });
      await settle();
      expect(calls.some((c) => c.method === "GET" && c.path === "/api/llm/history/runs/41/dialogue")).toBe(false);
      // New closes the run the ask armed - a positive precondition that holds today.
      await click("#laTestFresh");
      expect(sent("POST", "/api/llm/history/runs/41/close").length).toBe(1);
      expect(text("#laTestOut")).not.toContain("M-B1-first.");
      // The held dialogue resolves AFTER the close: it must paint nothing and arm nothing.
      dial.resolve(OPEN_DIALOGUE);
      await settle();
      expect(text("#laTestOut")).not.toContain("file a proposal");
      fresh = true;
      await type("#laTestPrompt", "M-B2");
      await click("#laTestRun");
      await settle();
      expect(sent("POST", "/api/llm/agents/test/stream").at(-1)?.body).toEqual({ agent: "distiller", prompt: "M-B2" });
      expect(text("#laTestOut")).toContain("M-B2-fresh.");
    } finally {
      disc.resolve({});
      dial.resolve({});
    }
  });

  it("Enter asks, Shift+Enter breaks a line, and a live loop swallows the second Enter", async () => {
    const live = liveBody();
    override = (c) => c.method === "POST" && isStream(c) ? [200, { __sseLive: live.body }] : undefined;
    await agents.mount();
    await click("[data-latest=distiller]");
    type("#laTestPrompt", "hello there");
    q("#laTestPrompt").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await settle();
    expect(sent("POST", "/api/llm/agents/test/stream").map((c) => c.body)).toEqual([{ agent: "distiller", prompt: "hello there" }]);
    // The loop is live: a second Enter - and Shift+Enter's line break - never double-asks.
    type("#laTestPrompt", "more");
    q("#laTestPrompt").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    q("#laTestPrompt").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", shiftKey: true, bubbles: true }));
    await settle();
    expect(sent("POST", "/api/llm/agents/test/stream").length).toBe(1);
    live.push("run", { runId: 45, agent: "distiller", continued: false });
    live.push("done", { ok: true, runId: 45, ms: 5, turns: 1, tokensIn: 5, tokensOut: 1 });
    live.end();
    await settle();
  });
});

describe("the LLM plugin - Reporting", () => {
  it("not set up: Set up opens the sheet, a preset fills cc-otel's listener, Save writes the block", async () => {
    await agents.mount();
    expect(text("#laTel")).toBe("Not set up - no run is reported.");
    expect(q<HTMLButtonElement>("#laTelTest").disabled).toBe(true);
    expect(text("#laTelEdit")).toBe("Set up");
    await click("#laTelEdit");
    expect(document.querySelector("#ltRemove")?.hasAttribute("hidden")).toBe(true);
    await click("#ltSave");
    expect(text("#ltErr")).toBe("The endpoint is required");
    change("#ltPreset", "http");
    expect(q<HTMLSelectElement>("#ltProtocol").value).toBe("http/protobuf");
    expect(q<HTMLInputElement>("#ltEndpoint").value).toBe("http://127.0.0.1:4318");
    expect(text("#ltEndpointWhy")).toBe("The collector's base URL; /v1/logs is added.");
    change("#ltPreset", "grpc");
    expect(text("#ltEndpointWhy")).toBe("scheme://host:port with no path - the gRPC service path is added.");

    // A credential-named header must carry a reference; a literal one stays in the browser.
    type("#ltHeaders", "authorization=Bearer abc123");
    await click("#ltSave");
    expect(text("#ltErr")).toBe("authorization names a credential: its value must be a ${secret://name} or ${ENV_VAR} reference.");
    type("#ltHeaders", "authorization=Bearer ${secret://otel}");
    await click("#ltSave");
    expect(puts()[0].telemetry).toEqual({ endpoint: "http://127.0.0.1:4317", protocol: "grpc", headers: { authorization: "Bearer ${secret://otel}" } });
    expect(text("#toast")).toBe("Reporting saved");
    expect(text("#laTel")).toContain("gRPC · http://127.0.0.1:4317");
    expect(text("#laTelEdit")).toBe("Edit");
    expect(text("[data-larow=distiller]")).toContain("reported");
  });

  it("the card counts the exports; Test sends one empty export and says how it went; Remove drops the block", async () => {
    row.telemetry = { endpoint: "http://127.0.0.1:4317", protocol: "grpc" };
    telemetry = { sent: 42, dropped: 3, lastExportMs: Date.now() - 5000 };
    let probe: Answer = [200, { ok: true, protocol: "grpc", endpoint: "http://127.0.0.1:4317", ms: 7 }];
    override = (c) => c.path === "/api/llm/telemetry/test" ? probe : undefined;
    await agents.mount();
    expect(text("#laTel")).toContain("42 sent");
    expect(text("#laTel .tag.warn")).toBe("3 dropped");
    await click("#laTelTest");
    expect(sent("POST", "/api/llm/telemetry/test").map((c) => c.body)).toEqual([{}]);
    expect(text("#toast")).toBe("http://127.0.0.1:4317 answered in 7 ms");
    probe = [200, { ok: false, error: "connection refused", ms: 2 }];
    await click("#laTelTest");
    expect(text("#toast")).toBe("The collector did not take the export: connection refused");

    await click("#laTelEdit");
    expect(document.querySelector("#ltPreset")).toBe(null);
    await click("#ltRemove");
    expect("telemetry" in puts()[0]).toBe(false);
    expect(text("#toast")).toBe("Reporting removed");
    expect(text("#laTel")).toBe("Not set up - no run is reported.");
  });
});
