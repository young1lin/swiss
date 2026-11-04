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
   The streamed ask (SPEC §llm.api): one reader for every surface that talks to a builtin agent
   over server-sent events - the Agents Test and the Data page's agent. It paints the frames it
   knows (run, delta, tool, toolResult, steer, final, done) on the caller's column and hands any
   other frame to the caller's onFrame, so a surface can act on its own (the Data agent's db
   frame puts a statement and its rows on the page).
   ================================================================================================ */
import { targetEl } from "../util.js";
import { fill, h } from "../h.js";
import type { HChild } from "../h.js";
import { tr } from "../i18n.js";
import { factLine, flow, flowIO, flowToggle, fmtMs, formFold, note, scrollBox, speaker, splitJsonBlock, transcriptText, transcriptTurn } from "../ui/index.js";
import type { TranscriptTurn } from "../ui/index.js";
import { firstLine, jsonBlock, stepLine, thinkLine, toolIcon } from "./llm-history.js";

/** One streamed ask (SPEC §llm.api): POST the body to a stream route, read the answer as
 *  server-sent events, paint every happening on the dialog's column as it arrives.
 *  Delta events append to the line they belong to (reasoning to its Think line in the
 *  work, answer text to a plain turn) so the column grows exactly as the wire speaks:
 *  once work began, a round's words stand inside it between its thinking and its tool
 *  asks, in the order said; tool asks land as flow lines, results in them; the final swaps the
 *  streamed text for its markdown, below the work, and done closes with the run's
 *  numbers.
 *
 *  The conversation's shape: the early `run` frame names the run (the caller keeps the
 *  id; a continued ask says so on the column). A refusal the server sends BEFORE any
 *  frame carries a code - `busy` is recoverable, any other code locks the asks until
 *  New (the caller decides from `locked`). A body that ends without `done` is the
 *  caller's "ended without an answer" - never a quiet success. */
/** The model's silence shows once it outlasts a normal first token (4-5 s on GLM), so a
 *  quick round never flashes the line; past WAIT_HINT_MS the line says Stop is the way out. */
const WAIT_SHOW_MS = 3000;
const WAIT_HINT_MS = 20000;

export interface StreamHooks {
  /** False once the caller stopped or cleared this loop: its frames paint nothing. */
  alive(): boolean;
  /** The run this ask speaks in was named (again on `done`, for wires without the
   *  early frame). */
  onRunId(id: number, continued: boolean): void;
  /** The loop took words the reader queued (their id) and the model reads them next. */
  onSteer?(id: string, text: string): void;
  /** A frame this reader does not paint itself: a surface's own (the Data agent's `db`). */
  onFrame?(name: string, payload: Record<string, unknown>): void;
  /** A tool answered (the frame, its `ms` how long the tool took), after the column shows it. */
  onToolResult?(payload: Record<string, unknown>): void;
  /** A frame changed the transcript's layout, including a delta or a closing disclosure. */
  onPaint?(): void;
}

export interface StreamOutcome { error?: string; cancelled?: boolean; locked?: boolean; ended?: boolean }

export async function streamAsk(
  url: string,
  body: Record<string, unknown>,
  hooks: StreamHooks,
  addTurn: (t: TranscriptTurn) => HTMLElement,
  signal: AbortSignal,
): Promise<StreamOutcome> {
  let answerEl: HTMLElement | null = null;
  let answerText = "";
  let doneSeen = false;
  let cancelled = false;
  let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  const pending: Array<{ name: string; result: (text: string, ok: boolean) => void }> = [];
  let process: HTMLDetailsElement | null = null;
  let processBody: HTMLElement | null = null;
  const addProcess = (t: TranscriptTurn): HTMLElement => {
    if (!processBody) {
      processBody = h("div", null);
      process = formFold({ summary: tr("llm.test.working") }, processBody) as HTMLDetailsElement;
      process.open = true;
      addTurn({ role: speaker("assistant"), code: [process] });
    }
    const node = transcriptTurn(t);
    processBody.appendChild(node);
    return node;
  };
  /** A round moving on to its tool asks: what it said stays where it was said, now
   *  the markdown it is (as History renders a round's prose in place). */
  const closeWords = (): void => {
    if (answerEl && answerText.trim()) {
      answerEl.replaceWith(transcriptTurn({ role: speaker("assistant"), text: answerText, markdown: true }));
      // (words between steps: their code blocks copy, the turn has no Copy of its own)
    }
    answerEl = null;
    answerText = "";
  };
  /** The thinking in flight as its Think line in the work (thinkLine, as History shows a
   *  round that reasoned): live while the model thinks, its span once anything else
   *  arrives; thinking after that opens a new line, in the order it was said. */
  let thought: { holder: HTMLElement; text: string; shown: string; at: number } | null = null;
  const paintThought = (t: { holder: HTMLElement; text: string; shown: string }, ms?: number): void => {
    const open = !!t.holder.querySelector(".fl-step.open");
    t.shown = firstLine(t.text.trim());
    fill(t.holder, flow([{ id: "think", ...thinkLine(t.text), live: ms == null, meta: ms == null ? undefined : fmtMs(ms) }], {
      eager: true, open: open ? new Set(["think"]) : undefined, body: () => scrollBox(transcriptText(t.text, { dim: true })),
    }));
  };
  const think = (text: string): void => {
    const t = thought;
    if (t) {
      t.text += text;
      const holder = t.holder.querySelector(".fl-body .tc-text");
      // Only the first line rides the row: past it, the opened body takes the words.
      if (holder && firstLine(t.text.trim()) === t.shown) holder.textContent = t.text;
      else paintThought(t);
      return;
    }
    const holder = h("div", { data: { lathink: "" } });
    holder.onclick = (e): void => {
      if (targetEl(e)?.closest(".fl-sum")) { flowToggle(holder, "think"); e.stopPropagation(); }
    };
    const opened = { holder, text, shown: "", at: Date.now() };
    paintThought(opened);
    addProcess({ role: null, code: [holder] });
    thought = opened;
  };
  const closeThought = (): void => {
    if (thought) paintThought(thought, Date.now() - thought.at);
    thought = null;
  };
  const foldProcess = (label = tr("llm.test.worked")): void => {
    if (!process) return;
    process.open = false;
    const summary = process.querySelector("summary");
    if (summary) summary.textContent = label;
  };
  /* The model's silence, counted (2026-11-04: one GLM round sat 127 s before its first token
     and the column showed nothing, so the pane read as frozen). From the moment the model is
     asked - the stream opened, a round's last tool answered, a steer taken - until its next
     frame, a quiet line at the column's end counts the wait; any frame of the model's own,
     Stop or the stream's end takes it away. */
  let wait: { since: number; timer: ReturnType<typeof setInterval>; turn: HTMLElement | null; words: HTMLElement } | null = null;
  const stopWait = (): void => {
    const w = wait;
    if (!w) return;
    clearInterval(w.timer);
    w.turn?.remove();
    wait = null;
  };
  const tickWait = (): void => {
    const w = wait;
    if (!w) return;
    if (!hooks.alive()) { stopWait(); return; }
    const ms = Date.now() - w.since;
    if (ms < WAIT_SHOW_MS) return;
    const elapsed = tr("ui.sec", { n: String(Math.floor(ms / 1000)) });
    w.words.textContent = tr(ms >= WAIT_HINT_MS ? "llm.test.waitingStop" : "llm.test.waiting", { elapsed });
    if (!w.turn) {
      w.turn = addTurn({ role: null, code: [note(w.words, { busy: true, data: { lawait: "" } })] });
      hooks.onPaint?.();
    }
  };
  const startWait = (): void => {
    stopWait();
    wait = { since: Date.now(), timer: setInterval(tickWait, 1000), turn: null, words: h("span") };
  };
  const settleInterrupted = (): void => {
    closeThought();
    // Keep partial words readable after Stop even when the caller has retired this send.
    // Reparent its own turn rather than asking that caller to append to a newer generation.
    if (answerEl && process?.contains(answerEl)) {
      process.closest(".tc-turn")?.after(answerEl);
      answerEl = null;
      answerText = "";
    }
    foldProcess(tr(signal.aborted ? "llm.test.cancelled" : "llm.history.failed"));
    process?.querySelectorAll(".fl-step.live").forEach((step) => step.classList.remove("live"));
    process?.querySelectorAll(".dot.starting").forEach((pulse) => pulse.remove());
    hooks.onPaint?.();
  };
  const abort = (): void => {
    stopWait();
    if (!doneSeen) settleInterrupted();
    // A custom transport may not reject its pending read on fetch's AbortSignal.
    void reader?.cancel().catch(() => { /* the fetch may already have aborted */ });
  };
  signal.addEventListener("abort", abort, { once: true });
  try {
    const response = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
      signal,
    });
    if (!response.ok || !response.body) {
      // The curated JSON first ({error, code}); a body that is not JSON falls back to
      // its text, then the bare status - a real API error is never papered over.
      let why = "";
      let code: string | undefined;
      try {
        const parsed = await response.json() as { error?: unknown; code?: unknown };
        if (typeof parsed.error === "string") why = parsed.error;
        if (typeof parsed.code === "string") code = parsed.code;
      } catch { /* not JSON: try the plain text */ }
      if (!why) {
        try { why = await response.text(); } catch { /* a body that is neither */ }
      }
      return {
        error: why || (response.status + " " + response.statusText),
        locked: code !== undefined && code !== "busy",
      };
    }
    reader = response.body.getReader();
    startWait();
    const decoder = new TextDecoder();
    let buffer = "";
    let error: string | undefined;
    const onFrame = (name: string, data: string): void => {
      if (!hooks.alive()) return; // a stopped loop's frame is not this dialog's
      let payload: Record<string, unknown> = {};
      try { payload = JSON.parse(data) as Record<string, unknown>; } catch { payload = {}; }
      // The model spoke (or the run closed): its silence is over.
      if (name === "delta" || name === "tool" || name === "final" || name === "done") stopWait();
      if (name === "run") {
        const id = payload.runId;
        if (typeof id === "number") hooks.onRunId(id, payload.continued === true);
      } else if (name === "delta") {
        const text = typeof payload.text === "string" ? payload.text : "";
        // The first delta builds its turn carrying that piece (an empty turn
        // would have nowhere to land); the rest append to the same block.
        if (payload.kind === "thinking") {
          think(text);
          return;
        }
        closeThought();
        if (!answerEl) {
          // Words said once the work began stand in it, in order: after the step
          // before them, before the tool asks they lead to. Words before any work
          // open the column; the final lifts the closing round's words out below.
          const turn: TranscriptTurn = { role: speaker("assistant"), text };
          answerEl = processBody ? addProcess(turn) : addTurn(turn);
          answerText = text;
        } else {
          const holder = answerEl.querySelector(".tc-text");
          if (holder) holder.textContent += text;
          answerText += text;
        }
      } else if (name === "tool") {
        closeThought();
        closeWords();
        const tool = String(payload.name || "");
        const args = String(payload.arguments || "");
        const holder = h("div", { data: { latool: tool } });
        // A call and its answer are JSON most of the time (a SQL result, a file's rows), and
        // the reader must see it highlighted here exactly as the history page shows it: the
        // plain-text block this used to paint was the regression. Anything that is not JSON
        // still reads as text, verbatim.
        const shown = (raw: string): HChild => {
          const parsed = splitJsonBlock(raw);
          return parsed && !parsed.tail ? jsonBlock(parsed.value) : transcriptText(raw, { mono: true });
        };
        const paint = (text: string | null, ok = true): void => {
          const open = !!holder.querySelector(".fl-step.open");
          fill(holder, flow([{ id: "tool", title: tool, lead: toolIcon(tool), mono: true, ...stepLine(args),
            live: text == null, status: !ok ? { text: tr("llm.history.toolError"), tone: "bad" } : undefined,
          }], { eager: true, open: open ? new Set(["tool"]) : undefined, body: () => flowIO([
            { label: tr("llm.history.arguments"), body: shown(args), copy: args, copyLabel: tr("logs.copyArguments") },
            {
              label: tr("llm.history.result"),
              body: text == null ? note(tr("llm.agents.testing"), { busy: true }) : shown(text),
              copy: text == null ? undefined : text,
              copyLabel: tr("logs.copyResult"),
            },
          ]) }));
        };
        holder.onclick = (e): void => {
          if (targetEl(e)?.closest(".fl-sum")) { flowToggle(holder, "tool"); e.stopPropagation(); }
        };
        paint(null);
        pending.push({ name: tool, result: paint });
        addProcess({ role: null, code: [holder] });
      } else if (name === "toolResult") {
        const index = pending.findIndex((t) => t.name === payload.name);
        if (index >= 0) pending.splice(index, 1)[0].result(String(payload.text || ""), payload.ok !== false);
        else addTurn({ role: tr("llm.history.role.tool"), code: [formFold({ summary: String(payload.name || tr("llm.history.result")) }, transcriptText(String(payload.text || ""), { mono: true }))] });
        hooks.onToolResult?.(payload);
        // The round's last tool answered: the model is asked again.
        if (!pending.length) startWait();
      } else if (name === "steer") {
        // The reader's words went in between two steps: the work so far folds, the
        // words land as the reader's turn, and the next step opens a fold beneath them.
        if (process) {
          process.open = false;
          const summary = process.querySelector("summary");
          if (summary) summary.textContent = tr("llm.test.worked");
        }
        process = null;
        processBody = null;
        closeThought();
        closeWords();
        hooks.onSteer?.(String(payload.id || ""), String(payload.text || ""));
        startWait(); // the model reads the words next; the count restarts under them
      } else if (name === "final") {
        // The closing answer renders as the markdown it is, whole, its Copy closing it.
        closeThought();
        if (answerEl) answerEl.remove();
        foldProcess();
        const text = String(payload.text || "");
        addTurn({ role: process ? null : speaker("assistant"), text, markdown: true, copy: text.trim() ? text : undefined });
      } else if (name === "done") {
        doneSeen = true;
        if (process) {
          process.open = false;
          const summary = process.querySelector("summary");
          if (summary) summary.textContent = tr("llm.test.worked");
        }
        const id = payload.runId;
        if (typeof id === "number") hooks.onRunId(id, false);
        if (payload.ok === false) {
          if (payload.cancelled === true) cancelled = true;
          else error = typeof payload.error === "string" ? payload.error : "the run failed";
        } else {
          const meta = tr("llm.test.doneMeta", {
            turns: String(payload.turns ?? 1),
            in: String(payload.tokensIn ?? 0),
            out: String(payload.tokensOut ?? 0),
            ms: String(payload.ms ?? 0),
          });
          const summary = process?.querySelector("summary");
          if (summary) summary.textContent = tr("llm.test.worked") + " · " + meta;
          else addTurn({ role: null, code: [factLine([meta])] });
        }
        closeThought();
        answerEl = null;
      } else hooks.onFrame?.(name, payload);
    };
    for (;;) {
      const chunk = await reader.read();
      if (chunk.done) break;
      buffer += decoder.decode(chunk.value, { stream: true });
      for (;;) {
        const cut = buffer.indexOf("\n\n");
        if (cut < 0) break;
        const frame = buffer.slice(0, cut);
        buffer = buffer.slice(cut + 2);
        let eventName = "message";
        let data = "";
        for (const line of frame.split("\n")) {
          if (line.startsWith("event:")) eventName = line.slice(6).trim();
          else if (line.startsWith("data:")) data += line.slice(5).trim();
        }
        if (data) { onFrame(eventName, data); hooks.onPaint?.(); }
      }
    }
    if (signal.aborted || cancelled) return { cancelled: true };
    if (error !== undefined) return { error };
    return doneSeen ? {} : { ended: true };
  } catch (err) {
    if (signal.aborted) return { cancelled: true };
    return { error: err instanceof Error ? err.message : String(err) };
  } finally {
    stopWait();
    signal.removeEventListener("abort", abort);
    // Event callbacks assigned the disclosure while the stream was being read.
    const work = process as HTMLDetailsElement | null;
    if (work && !doneSeen) {
      settleInterrupted();
    }
  }
}
