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
import { fmtMs, transcriptTurn } from "../src/ui/index.js";
import { workSummary } from "../src/views/llm-history.js";
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

  it("heads each reply with the model's mark once: words between steps and a fold under leading words carry none", async () => {
    const run = async (frames: Array<[string, unknown]>): Promise<HTMLElement> => {
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
        for (const [name, payload] of frames) {
          wire.push(name, payload);
          await paint();
        }
        wire.end();
        await expect(ask).resolves.toEqual({});
      } finally {
        wire.end();
        globalThis.fetch = previous;
      }
      return column;
    };
    const marks = (column: HTMLElement): HTMLElement[] => Array.from(column.querySelectorAll<HTMLElement>(".tc-who.assistant"));

    // Work first: the fold's turn is the reply's head; the words said between two steps sit unmarked in it.
    const worked = await run([
      ["tool", { name: "run_sql", arguments: "{}" }],
      ["toolResult", { name: "run_sql", ok: true, text: "1 row" }],
      ["delta", { kind: "text", text: "Between the steps" }],
      ["tool", { name: "describe_table", arguments: "{}" }],
      ["toolResult", { name: "describe_table", ok: true, text: "id int" }],
      ["final", { text: "The answer" }],
      ["done", { runId: 1, ok: true }],
    ]);
    const between = Array.from(worked.querySelectorAll("details .tc-turn")).find((t) => t.textContent?.includes("Between the steps"));
    expect(between).toBeTruthy();
    expect(between?.querySelector(".tc-who")).toBe(null);
    expect(marks(worked)).toHaveLength(1);
    expect(marks(worked)[0].closest(".tc-turn")?.querySelector("details")).toBeTruthy();

    // Words first: they head the reply, the fold opening under them and the final carry no mark.
    // A steer starts the next reply, which is headed again.
    const led = await run([
      ["delta", { kind: "text", text: "Let me look" }],
      ["tool", { name: "run_sql", arguments: "{}" }],
      ["toolResult", { name: "run_sql", ok: true, text: "1 row" }],
      ["steer", { id: "q1", text: "also count them" }],
      ["tool", { name: "run_sql", arguments: "{}" }],
      ["toolResult", { name: "run_sql", ok: true, text: "2 rows" }],
      ["final", { text: "Counted" }],
      ["done", { runId: 1, ok: true }],
    ]);
    const heads = marks(led).map((m) => m.closest(".tc-turn"));
    expect(heads).toHaveLength(2);
    expect(heads[0]?.textContent).toContain("Let me look");
    expect(heads[0]?.querySelector("details")).toBe(null);
    expect(heads[1]?.querySelector("details")).toBeTruthy();
  });

  it("sums the work under its fold in the line a restored ask reads, its chevron kept; a steer folds what came before on its own line", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const wire = streaming();
    const column = document.createElement("div");
    const previous = globalThis.fetch;
    globalThis.fetch = vi.fn(async () => new Response(wire.body));
    const summaries = (): string[] => Array.from(column.querySelectorAll(".tc-code > details > summary")).map((s) => s.textContent || "");
    try {
      const ask = streamAsk("/stream", {}, { alive: () => true, onRunId: () => undefined }, (t) => {
        const node = transcriptTurn(t);
        column.appendChild(node);
        return node;
      }, new AbortController().signal);
      wire.push("run", { runId: 3 });
      wire.push("delta", { kind: "thinking", text: "Inspect the schema" });
      wire.push("usage", { tokensIn: 1_200, tokensOut: 30 });
      wire.push("tool", { name: "run_sql", arguments: "{}" });
      wire.push("toolResult", { name: "run_sql", ok: true, text: "1 row" });
      await paint();
      vi.advanceTimersByTime(4_000);
      wire.push("steer", { id: "q1", text: "only the active ones" });
      await paint();
      expect(summaries()).toEqual([workSummary({ rounds: 1, tools: 1, tokensIn: 1_200, tokensOut: 30, ms: 4_000 })]);
      expect(summaries()[0]).toBe("1 step · 1 tool call · 1.2K in / 30 out · " + fmtMs(4_000));
      wire.push("delta", { kind: "thinking", text: "Filter them" });
      wire.push("usage", { tokensIn: 1_300, tokensOut: 40 });
      wire.push("tool", { name: "run_sql", arguments: "{}" });
      wire.push("toolResult", { name: "run_sql", ok: true, text: "1 row" });
      wire.push("usage", { tokensIn: 1_400, tokensOut: 50 });
      await paint();
      vi.advanceTimersByTime(2_000);
      wire.push("final", { text: "Two are active." });
      wire.push("done", { runId: 3, ok: true, ms: 6_000, turns: 3, tokensIn: 3_900, tokensOut: 120 });
      wire.end();
      await expect(ask).resolves.toEqual({});
      expect(summaries(), "the fold after the steer counts its own rounds and time").toEqual([
        workSummary({ rounds: 1, tools: 1, tokensIn: 1_200, tokensOut: 30, ms: 4_000 }),
        workSummary({ rounds: 2, tools: 1, tokensIn: 2_700, tokensOut: 90, ms: 2_000 }),
      ]);
      for (const s of column.querySelectorAll(".tc-code > details > summary")) expect(s.querySelector(".fold-chev")).not.toBeNull();
    } finally {
      wire.end();
      globalThis.fetch = previous;
      vi.useRealTimers();
    }
  });

  it("without usage frames the run's totals say the work: in the fold when there was one, else as the answer's fact line", async () => {
    for (const worked of [true, false]) {
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
        if (worked) {
          wire.push("tool", { name: "run_sql", arguments: "{}" });
          wire.push("toolResult", { name: "run_sql", ok: true, text: "1 row" });
        }
        wire.push("final", { text: "Done." });
        wire.push("done", { runId: 4, ok: true, ms: 900, turns: 3, tokensIn: 100, tokensOut: 20 });
        wire.end();
        await expect(ask).resolves.toEqual({});
        const said = workSummary({ rounds: 3, tools: worked ? 1 : 0, tokensIn: 100, tokensOut: 20, ms: 900 });
        if (worked) {
          expect(column.querySelector(".tc-code > details > summary")?.textContent).toBe(said);
          expect(said).toBe("3 steps · 1 tool call · 100 in / 20 out · " + fmtMs(900));
        } else {
          expect(column.querySelector("details")).toBeNull();
          expect(column.lastElementChild?.textContent).toBe(said);
        }
      } finally {
        wire.end();
        globalThis.fetch = previous;
      }
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
      expect(line()?.textContent).toContain("Waiting for the model · 20 s · Stop cancels this step");
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
