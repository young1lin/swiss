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

import { describe, expect, it, vi } from "vitest";
import { transcriptTurn } from "../src/ui/index.js";
import { streamAsk } from "../src/views/llm-stream.js";

/** Frames arrive independently of cancellation, like a transport still closing its socket. */
function streaming() {
  let controller: ReadableStreamDefaultController<Uint8Array>;
  let ended = false;
  const body = new ReadableStream<Uint8Array>({ start(c) { controller = c; }, cancel() { ended = true; } });
  const encoder = new TextEncoder();
  const push = (name: string, payload: unknown): void => {
    controller.enqueue(encoder.encode("event: " + name + "\ndata: " + JSON.stringify(payload) + "\n\n"));
  };
  return { body, push, end: (): void => { if (!ended) { ended = true; controller.close(); } } };
}

const paint = async (): Promise<void> => {
  for (let i = 0; i < 8; i++) await Promise.resolve();
};

describe("a streaming answer's process", () => {
  it("keeps the complete live tool arguments and result, including their tails", async () => {
    const wire = streaming();
    const column = document.createElement("div");
    const previous = globalThis.fetch;
    const args = JSON.stringify({ sql: "select " + "column_name, ".repeat(100) + "last_column" });
    const result = "result " + "row data ".repeat(200) + "RESULT_END";
    globalThis.fetch = vi.fn(async () => new Response(wire.body));
    try {
      const ask = streamAsk("/stream", {}, { alive: () => true, onRunId: () => undefined }, (turn) => {
        const node = transcriptTurn(turn);
        column.appendChild(node);
        return node;
      }, new AbortController().signal);
      wire.push("tool", { name: "run_sql", arguments: args });
      wire.push("toolResult", { name: "run_sql", ok: true, text: result });
      await paint();
      expect(column.textContent).toContain("last_column");
      expect(column.textContent).toContain("RESULT_END");
      wire.push("done", { runId: 1, ok: true });
      wire.end();
      await expect(ask).resolves.toEqual({});
    } finally {
      wire.end();
      globalThis.fetch = previous;
    }
  });

  it("folds when the final answer arrives, before the transport's done frame", async () => {
    const wire = streaming();
    const column = document.createElement("div");
    const previous = globalThis.fetch;
    globalThis.fetch = vi.fn(async () => new Response(wire.body));
    try {
      const ask = streamAsk("/stream", {}, { alive: () => true, onRunId: () => undefined }, (t) => {
        const node = transcriptTurn(t);
        column.appendChild(node);
        return node;
      }, new AbortController().signal);
      wire.push("delta", { kind: "thinking", text: "Inspect the schema" });
      wire.push("delta", { kind: "text", text: "The latest answer" });
      await paint();
      const work = column.querySelector("details")!;
      expect(work.open).toBe(true);
      wire.push("final", { text: "The latest answer" });
      await paint();
      expect(work.open).toBe(false);
      expect(column.lastElementChild?.textContent).toContain("The latest answer");
      wire.push("done", { runId: 1, ok: true });
      wire.end();
      await expect(ask).resolves.toEqual({});
    } finally {
      wire.end();
      globalThis.fetch = previous;
    }
  });

  it("counts the model's silence at the column's end, says Stop past 20 s, and clears on its next frame", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
    const wire = streaming();
    const column = document.createElement("div");
    const previous = globalThis.fetch;
    globalThis.fetch = vi.fn(async () => new Response(wire.body));
    const line = (): HTMLElement | null => column.querySelector<HTMLElement>("[data-lawait]");
    try {
      const ask = streamAsk("/stream", {}, { alive: () => true, onRunId: () => undefined }, (t) => {
        const node = transcriptTurn(t);
        column.appendChild(node);
        return node;
      }, new AbortController().signal);
      wire.push("run", { runId: 7 });
      await paint();
      vi.advanceTimersByTime(2000);
      expect(line(), "a normal first token never flashes the line").toBeNull();
      vi.advanceTimersByTime(1000);
      expect(line()?.textContent).toContain("Waiting for the model · 3 s");
      expect(line()?.querySelector(".spin"), "it leads with the spinner").toBeTruthy();
      vi.advanceTimersByTime(17000);
      expect(line()?.textContent).toContain("Waiting for the model · 20 s · Stop cancels this round");
      wire.push("delta", { kind: "thinking", text: "Inspect the schema" });
      await paint();
      expect(line(), "the model spoke").toBeNull();
      wire.push("tool", { name: "run_sql", arguments: "{}" });
      wire.push("toolResult", { name: "run_sql", ok: true, text: "1 row" });
      await paint();
      vi.advanceTimersByTime(3000);
      expect(line(), "the last tool answered: the model is asked again").not.toBeNull();
      expect(column.lastElementChild?.contains(line())).toBe(true);
      wire.push("done", { runId: 7, ok: true });
      wire.end();
      await expect(ask).resolves.toEqual({});
      expect(line()).toBeNull();
    } finally {
      wire.end();
      globalThis.fetch = previous;
      vi.useRealTimers();
    }
  });

  it("Stop takes the waiting line away", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
    const wire = streaming();
    const column = document.createElement("div");
    const stop = new AbortController();
    const previous = globalThis.fetch;
    globalThis.fetch = vi.fn(async () => new Response(wire.body));
    try {
      const ask = streamAsk("/stream", {}, { alive: () => true, onRunId: () => undefined }, (t) => {
        const node = transcriptTurn(t);
        column.appendChild(node);
        return node;
      }, stop.signal);
      await paint();
      vi.advanceTimersByTime(5000);
      expect(column.querySelector("[data-lawait]")).not.toBeNull();
      stop.abort();
      expect(column.querySelector("[data-lawait]")).toBeNull();
      await expect(ask).resolves.toEqual({ cancelled: true });
    } finally {
      wire.end();
      globalThis.fetch = previous;
      vi.useRealTimers();
    }
  });

  it("Stop immediately folds the work, retains partial words and releases a pending read", async () => {
    const wire = streaming();
    const column = document.createElement("div");
    const stop = new AbortController();
    const previous = globalThis.fetch;
    let alive = true;
    globalThis.fetch = vi.fn(async () => new Response(wire.body));
    try {
      const ask = streamAsk("/stream", {}, { alive: () => alive, onRunId: () => undefined }, (t) => {
        const node = transcriptTurn(t);
        if (alive) column.appendChild(node);
        return node;
      }, stop.signal);
      wire.push("delta", { kind: "thinking", text: "Inspect the schema" });
      wire.push("delta", { kind: "text", text: "Partial answer remains visible" });
      await paint();
      const work = column.querySelector("details")!;
      expect(work.open).toBe(true);
      alive = false;
      stop.abort();
      expect(work.open).toBe(false);
      expect(column.lastElementChild?.textContent).toContain("Partial answer remains visible");
      expect(column.lastElementChild?.contains(work)).toBe(false);
      await expect(ask).resolves.toEqual({ cancelled: true });
      expect(column.querySelector(".fl-step.live")).toBeNull();
    } finally {
      wire.end();
      globalThis.fetch = previous;
    }
  });
});
