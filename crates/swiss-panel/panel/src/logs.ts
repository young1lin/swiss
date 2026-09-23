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

import type { ApiMcpCallRow, ApiMcpItem, ApiMcpRow } from "./types/api.js";
import type { PhantomMcpRow } from "./types/dom.js";
import type { KindPageState, McpDetail, McpKind } from "./types/state.js";
import { toast } from "./util.js";
import { frag, h } from "./h.js";
import type { HChild } from "./h.js";
import { rowOf } from "./sidebar.js";
import { mcpDetail } from "./mcp-state.js";
import { tr } from "./i18n.js";
import { JV_LINES, decodeStrings, formattedCopyText, hasDecoded, jsonCodeNode, splitJsonBlock, textNode, valueBlock } from "./ui/json-view.js";
import {
  btn, card, collapseRuns, emptyNode, failNote, filterInput, fmtMs, iconBtn, moreBtn, note, pager, row, section, spinner,
  sw, timeLabel, timeline, timelineMeta, timelineToggle,
} from "./ui/index.js";
import type { TimelineItem } from "./ui/index.js";

/* --- Logs: what was called, with what, and what came back ------------------------------------- */
function fmtChars(n: number): string {
  return n < 1000 ? tr("logs.nChars", { n }) : tr("logs.nKChars", { n: (n / 1000).toFixed(1) });
}

/**
 * Format a tool result for reading.
 *
 * A result travels compact — indentation is dead weight in a model's context — but nobody can read a
 * 2 KB single-line object, so the panel expands it here. A trailing "[showing the first N of M …]"
 * note from the gateway is kept out of the parse and re-appended. Anything that is not JSON (a plain
 * string, an error message, a clipped log entry) is shown exactly as it arrived.
 */
function fmtJson(text: string | null | undefined): string {
  if (!text) return "";
  let body = text, tail = "";
  const cut = text.lastIndexOf("\n\n[");
  if (cut > 0 && text.charAt(text.length - 1) === "]") { body = text.slice(0, cut); tail = text.slice(cut); }
  const trimmed = body.trim();
  if (trimmed.charAt(0) !== "{" && trimmed.charAt(0) !== "[") return text;
  try {
    return JSON.stringify(JSON.parse(trimmed), null, 2) + tail;
  } catch (e) {
    return text; // truncated or not actually JSON — better raw than pretended
  }
}

/** docs/33 C1: the house clipboard idiom (connect.js copyText, data-csv dbCopyText) for the log
 *  blocks: async clipboard first, the textarea fallback second, one quiet toast either way. */
function legacyCopy(text: string): void {
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try { document.execCommand("copy"); } catch (e) { /* nothing else to try */ }
  document.body.removeChild(ta);
}
async function copyLogText(text: string): Promise<void> {
  try {
    if (typeof navigator !== "undefined" && navigator.clipboard && navigator.clipboard.writeText) {
      await navigator.clipboard.writeText(text);
      toast(tr("logs.copied"));
      return;
    }
  } catch (e) { /* fall through to the legacy path */ }
  try { legacyCopy(text); toast(tr("logs.copied")); }
  catch (e) { toast(tr("logs.copyFailed"), true); }
}

/* --- docs/33 C3: one block of a call — the formatted JSON view -------------------------------
   ui/json-view.ts owns parsing, decoding, the code block and the labelled block around it; this is
   what Logs puts in it: the caption, what the body turned out to be, one visible Copy with Copy raw
   behind the block's ⋯ (docs/46 §3.2, revising C3's two standing buttons), the Show all tail. The
   block carries data-blk="<kind>:<seq>" so a full reply landing, or Show all, repaints this one
   block in place (repaintCallBlock) instead of the whole list. */
type BlockKind = "args" | "out";

/** The text a block shows: the arguments as stored, or the reply — whole once fetched, else the
 *  page's preview. Copy and Copy raw read the same text, so they always match the screen. */
function blockText(d: McpDetail, c: ApiMcpCallRow, kind: BlockKind): string {
  if (kind === "args") return c.args || "";
  const full = d.callsFull[c.seq];
  return (full != null ? full : c.output) || "";
}

function callBlockNode(d: McpDetail, c: ApiMcpCallRow, kind: BlockKind): HTMLElement {
  const key = kind + ":" + c.seq;
  const raw = blockText(d, c, kind);
  const error = kind === "out" && !c.ok;
  const all = !!(d.callsAll || {})[key];
  const notes: string[] = [];
  let body: HChild[];
  let lines: number;
  // An error keeps its red text as it came: it is read as a message, not inspected as data.
  const parsed = error ? null : splitJsonBlock(raw);
  if (parsed) {
    const value = decodeStrings(parsed.value);
    // A short value sits on one line (docs/46 §3.2) - unless prose follows it, which reads as a
    // paragraph under a block, not after a one-liner.
    const code = jsonCodeNode(value, all, { oneLine: !parsed.tail });
    if (hasDecoded(value)) notes.push(tr("logs.kindDecoded"));
    if (parsed.tail) notes.push(tr("logs.kindTail"));
    body = [code.node, parsed.tail ? textNode(parsed.tail, true, "logs jv-tail").node : null];
    lines = code.lines;
  } else {
    const text = textNode(raw || (kind === "args" ? tr("logs.none") : tr("logs.empty")), all, "logs" + (error ? " err" : ""));
    body = [text.node];
    lines = text.lines;
  }
  const label = kind === "args" ? tr("logs.arguments2") : c.ok ? tr("logs.result") : tr("logs.error");
  // The glyph is the whole visible button; its name says which block, so the two in one call differ.
  const copyName = kind === "args" ? tr("logs.copyArguments") : tr("logs.copyResult");
  const moreName = kind === "args" ? tr("logs.moreArguments") : tr("logs.moreResult");
  return valueBlock({
    label, notes, data: { blk: key },
    tools: [
      iconBtn("copy", copyName, { ghost: true, data: { copy: key } }),
      moreBtn(moreName, { data: { blkmore: key } }),
    ],
  }, ...body,
  !all && lines > JV_LINES ? btn(tr("logs.showAllLines", { n: lines }), { data: { showall: key } }) : null);
}

/** Repaint one block of one call in place — after its full reply lands or its Show all is pressed.
 *  A key whose row is not on the page (a poll moved the list, the row is closed) is simply
 *  nothing to do. */
function repaintCallBlock(d: McpDetail, key: string): void {
  if (typeof document === "undefined") return;
  const [kind, seqText] = key.split(":");
  const c = (d.calls || []).find((r) => { return r.seq === Number(seqText); });
  const node = document.querySelector('#tabbody [data-blk="' + key + '"]');
  if (!c || !node || (kind !== "args" && kind !== "out")) return;
  node.replaceWith(callBlockNode(d, c, kind));
}

/** What the block's Copy (formatted) or Copy raw puts on the clipboard, by its data key. */
function callBlockCopyText(d: McpDetail, key: string, raw: boolean): string | null {
  const [kind, seqText] = key.split(":");
  const c = (d.calls || []).find((r) => { return r.seq === Number(seqText); });
  if (!c || (kind !== "args" && kind !== "out")) return null;
  const text = blockText(d, c, kind);
  return raw ? text : formattedCopyText(text);
}

/** docs/33 C3: the line under a preview whose full reply was pruned. One builder for the painted
 *  body and for showFullResult's in-place swap, so the two cannot drift. */
function goneNode(seq: number): HTMLElement {
  return note(tr("detail.fullReplyLongerStored"), { data: { gone: seq } });
}

/** A value to read, the way Logs shows one (docs/46 §3.2): JSON as the panel's code block - on
 *  one line when it is short - and anything else exactly as it arrived; an error stays red text.
 *  Past JV_LINES the whole reply is plain formatted text instead: neither Run nor a hover has a
 *  Show all, and a 3,000-line reply painted token by token is DOM nobody reads. */
function readableBody(text: string, err: boolean): HChild[] {
  const parsed = err ? null : splitJsonBlock(text);
  if (parsed) {
    const code = jsonCodeNode(decodeStrings(parsed.value), false, { oneLine: !parsed.tail });
    if (code.lines <= JV_LINES) return [code.node, parsed.tail ? textNode(parsed.tail, true, "logs jv-tail").node : null];
  }
  return [textNode(err ? text : fmtJson(text), true, "logs" + (err ? " err" : "")).node];
}

/* --- docs/46 §3.2: the log is an event list ----------------------------------------------------
   One call is one timeline row: the time (the date is the day heading above it), the tool, its
   arguments, a failure as a red tag, the duration. What every row said the same - the transport,
   the client, the size - moved into the expanded body, and consecutive identical calls (same tool,
   same arguments, same outcome, same reply: a client's poll loop) fold into one row with ×N. */

/** A stored call as a timeline item. `callseq` addresses the row for the delegated listener. */
function callItem(c: ApiMcpCallRow): TimelineItem {
  return {
    id: String(c.seq),
    at: Date.parse(c.at),
    title: c.tool,
    arg: c.args || tr("logs.arguments"),
    who: c.client || undefined,
    ms: c.ms,
    status: c.ok ? undefined : { text: tr("logs.failed"), tone: "bad" },
    same: [c.tool, c.args, c.ok ? "ok" : "failed", c.chars, c.output].join("\u0000"),
    data: { callseq: c.seq },
  };
}

/** The calls a row stands for, newest first: the row's own call and, for ×N, every identical
 *  one folded under it. Null when no row on this page starts at `seq`. */
function callRunOf(d: McpDetail, seq: number): ApiMcpCallRow[] | null {
  const calls = d.calls || [];
  const bySeq = new Map(calls.map((c) => { return [String(c.seq), c] as const; }));
  for (const run of collapseRuns(calls.map(callItem))) {
    if (run.some((it) => { return it.id === String(seq); })) {
      return run.map((it) => { return bySeq.get(it.id) as ApiMcpCallRow; });
    }
  }
  return null;
}

/** The expanded body: the meta the row left out, then the arguments and the reply of the row's
 *  own (newest) call. A ×N row adds the run's span and every call in it, so a folded run hides
 *  nothing the unfolded rows said. */
function callBodyNode(d: McpDetail, run: ApiMcpCallRow[]): HChild[] {
  const c = run[0];
  const lines: HChild[] = [timelineMeta([
    tr("logs.metaVia", { via: c.via }),
    c.client ? tr("logs.metaClient", { client: c.client }) : null,
    tr("logs.metaChars", { chars: fmtChars(c.chars) }),
  ])];
  if (run.length > 1) {
    const ms = run.map((r) => { return r.ms; }).sort((a, b) => { return a - b; });
    lines.push(timelineMeta([
      tr("logs.runSame", { n: run.length }),
      tr("logs.runSpan", { from: timeLabel(Date.parse(run[run.length - 1].at)), to: timeLabel(Date.parse(c.at)) }),
      tr("logs.runMs", { min: fmtMs(ms[0]), max: fmtMs(ms[ms.length - 1]) }),
    ]));
    lines.push(timelineMeta(run.map((r) => {
      return tr(r.client && r.client !== c.client ? "logs.runEachClient" : "logs.runEach",
        { time: timeLabel(Date.parse(r.at)), ms: fmtMs(r.ms), client: r.client || "" });
    })));
  }
  // A page ships only the head of each reply; opening the row fetches the rest (docs/33 C3). The
  // button stays for the moment before that lands and as the retry when it failed. A reply whose
  // body was pruned says so in place — the preview above it is all that is left.
  const more = !c.preview || d.callsFull[c.seq] != null
    ? null
    : (d.callsGone || {})[c.seq]
      ? goneNode(c.seq)
      : btn(tr("logs.showFullResultChars", { chars: fmtChars(c.chars) }), { data: { full: c.seq } });
  return [...lines, callBlockNode(d, c, "args"), callBlockNode(d, c, "out"), more];
}

/** The log as an event list. A folded row is open when any call in it was opened, so a new
 *  identical call arriving at the top of a run does not close the row being read. */
function callsTimeline(d: McpDetail): HTMLElement {
  const calls = d.calls || [];
  const bySeq = new Map(calls.map((c) => { return [String(c.seq), c] as const; }));
  const items = calls.map(callItem);
  const open = new Set<string>();
  for (const run of collapseRuns(items)) {
    if (run.some((it) => { return !!d.callsOpen[it.id]; })) open.add(run[0].id);
  }
  return timeline(items, {
    open,
    body: (_it, run) => { return callBodyNode(d, run.map((it) => { return bySeq.get(it.id) as ApiMcpCallRow; })); },
  });
}

/** The pager's status cell: the committed page number, with the pending suffix while a switch
 *  is in flight. The number NEVER shows the target — pending must not pretend to be committed
 *  (docs/32 B1). One builder for the full paint and the in-place chrome patch, so they cannot drift. */
function callsStatusNode(d: McpDetail): HChild {
  const page = d.callsPage + 1;
  return d.callsPendingPage != null
    ? [tr("logs.pageN", { n: page }), spinner(), tr("logs.loading")]
    : tr("logs.pageN2", { n: page });
}

/** A failed foreground load, in place: the sentence and the way out, nothing else. The HTTP
 *  status is the only secondary text — a response body never lands in the panel (docs/32 B1). */
function callsErrNode(d: McpDetail): HTMLElement {
  // The sentence is set once, in the state field (detail.js callsLoadFailed) — the node renders
  // the field, so the copy cannot drift between the two.
  return failNote({
    id: "clErr",
    text: d.callsError || tr("logs.couldLoadCalls"),
    why: d.callsErrStatus || null,
    action: btn(tr("logs.retry"), { id: "clRetry" }),
  });
}

/** What an empty page says: a page past the end, a needle that matched nothing, or a log that
 *  has never recorded a call - only the last is the empty state; the others are one line. */
function callsEmptyNode(d: McpDetail, q: string): HTMLElement {
  if (d.callsPage) return note(tr("logs.nothingPage"));
  if (q) return note(tr("logs.callsMatchingQ", { q }));
  return emptyNode({ icon: "history", title: tr("logs.noCallsYet"), hint: tr("logs.noCallsHint") });
}

function logsBodyNode(d: McpDetail): HChild {
  if (d.calls == null && !d.callsError) {
    /* First open, before anything is there to keep in place (docs/32 B1). */
    return note(tr("logs.loadingCalls"), { busy: true });
  }
  /* docs/31: server-side search over the stored calls. The input re-renders with the page, but
   * renderCallsOnly swaps the LIVE node back in, so focus and caret survive a result repaint.
   * docs/32 B4: Clear lives behind the ⋯, never as a standing button beside the filter. */
  const q = d.callsQ || "";
  const tools = [
    filterInput({ id: "callsQ", placeholder: tr("logs.searchCalls"), label: tr("logs.searchToolCalls"), value: q }),
    moreBtn(tr("logs.moreLogActions"), { id: "clMenu" }),
  ];
  const busy = d.callsPendingPage != null;
  let body: HChild;
  let pages: HChild = null;
  if (d.calls == null) {
    body = callsErrNode(d); // the very first load failed — the error replaces the shell, not the rows
  } else {
    body = d.calls.length ? callsTimeline(d) : callsEmptyNode(d, q);
    /* Both directions go quiet while a switch is pending; the number stays the committed page. */
    pages = (d.callsPage > 0 || d.callsMore)
      ? pager({
          id: "clPager", label: tr("logs.callLogPages"), statusId: "clStatus", status: callsStatusNode(d),
          prev: btn(tr("logs.newer"), { id: "clPrev", disabled: busy || d.callsPage <= 0 }),
          next: btn(tr("logs.older"), { id: "clNext", disabled: busy || !d.callsMore }),
        })
      : null;
  }
  const errAgain = d.callsError && d.calls != null ? callsErrNode(d) : null;
  /* One region for rows/pager/error lets a pending switch mark itself busy in place, without
   * touching the search box above it or the stderr section below it (docs/32 B1). */
  const region = h("div", { id: "callsRegion", aria: { busy: busy ? "true" : "false" } }, body, pages, errAgain);
  let err: HChild = null;
  if (d.stderr) {
    err = section({ cap: tr("logs.childProcessStderr") }, card(textNode(d.stderr, true, "logs").node));
  } else if ((d.config && d.config.type) === "proc") {
    /* An empty stderr on a proc answers a different question depending on whether a child exists
       yet. Blank space here reads as "logs went missing" — say which of the three blanks it is. */
    const st = (rowOf(d.name) || {} as ApiMcpRow).lifecycle;
    const why = st === "idle"
      ? tr("logs.childProcessLazyProc")
      : st === "error"
        ? tr("logs.childPrintedNothingBefore")
        : tr("logs.childRunningButPrinted");
    err = section({ cap: tr("logs.childProcessStderr") }, note(why));
  }
  return frag(section({ cap: tr("logs.toolCallsNewestFirst"), tools }, region), err);
}

/** Expand/collapse one call without re-rendering: a poll must not close what you just opened.
 *  The state is per call; closing a folded row forgets every call in it. The body is painted on
 *  open only (docs/46 §2.4) - a closed row costs no code block. Answers whether the row is now
 *  open, so the caller can fetch a clipped reply in full. */
function toggleCall(seq: number): boolean {
  const d = mcpDetail();
  if (!d) return false;
  const run = callRunOf(d, seq) || [];
  const open = !run.some((c) => { return !!d.callsOpen[c.seq]; }) && !d.callsOpen[seq];
  run.forEach((c) => { d.callsOpen[c.seq] = false; });
  d.callsOpen[seq] = open;
  const root = typeof document === "undefined" ? null : document.querySelector<HTMLElement>("#tabbody .tl");
  if (root && typeof root.querySelectorAll === "function") {
    timelineToggle(root, String(seq), open && run.length ? callBodyNode(d, run) : undefined);
  }
  return open;
}

/* --- docs/46 §3.2: Tools, Resources, Prompts ---------------------------------------------------
   One library row per item: the name (mono - a tool name is a value you type), one line of
   description, and the row's controls - Try and the client switch on a tool, Read on a resource.
   A tool or a prompt opens in place to its whole record: the full description, then its
   arguments as the code block every JSON value in the panel is (the same block Logs draws). */

/** The record behind a tool or a prompt row: what the one-line row cuts off. */
function itemRecordNode(it: ApiMcpItem, kind: McpKind): HChild[] {
  const args: unknown = kind === "tools" ? it.inputSchema : it.arguments && it.arguments.length ? it.arguments : null;
  return [
    valueBlock({ label: tr("logs.fullDescription"), text: it.description || tr("logs.descriptionProvided") }),
    // A tool is described by its input schema, a prompt by its argument list: each says so
    // under its own name, and says it has none rather than leaving the block out.
    kind === "tools"
      ? valueBlock({ label: tr("logs.inputSchema"), text: args == null ? tr("logs.inputSchemaProvided") : undefined },
          args != null ? jsonCodeNode(args, true).node : null)
      : valueBlock({ label: tr("logs.arguments2"), text: args == null ? tr("logs.arguments") : undefined },
          args != null ? jsonCodeNode(args, true).node : null),
  ];
}

/** An item list's pages. Tools, resources and prompts page by cursor, so the status says the
 *  page number, "of M" only when the MCP reported a total. */
function kindPager(kd: KindPageState): HTMLElement | null {
  const page = kd.cursors.length;
  if (page <= 1 && !kd.nextCursor) return null;
  const words = kd.total != null
    ? tr("logs.pageNM", { n: page, m: Math.max(1, Math.ceil(kd.total / kd.pageSize)) })
    : tr("logs.pageN2", { n: page });
  return pager({
    label: tr("logs.itemPages"), status: kd.loading ? [words, spinner()] : words,
    prev: btn(tr("logs.previous"), { id: "pgPrev", disabled: page <= 1 }),
    next: btn(tr("logs.next"), { id: "pgNext", disabled: !kd.nextCursor }),
  });
}

function kindBodyNode(d: McpDetail, kind: McpKind, m: ApiMcpRow | PhantomMcpRow): HChild {
  const kd = d[kind];
  if (m.lifecycle !== "started") return note(tr("logs.startedStartListKind", { kind }));
  if (kd.loading && !kd.loaded) return note(tr("logs.loadingKind", { kind }), { busy: true });
  if (kd.error) {
    // Keep the pager: pageNext/pagePrev push the cursor optimistically, so a failed page used to
    // leave the tab with no way back except the global Refresh (which resets to page one).
    return frag(note(kd.error, { err: true }), kd.cursors.length > 1 ? kindPager(kd) : null);
  }
  // Resources get a master on/off at the top: off empties the list (the capability stays, so the
  // notify stays valid) and tells connected clients to re-list. Symmetric with the per-tool switch.
  let resToggle: HTMLElement | null = null;
  if (kind === "resources" && kd.loaded) {
    const on = kd.resourceEnabled !== false;
    resToggle = card(row({
      name: tr("logs.exposeResources"),
      sub: on ? tr("logs.visibleClients") : tr("logs.hiddenClientsSeeResources"),
      toggle: sw(on, tr("logs.exposeResources"), {
        data: { restog: on ? "1" : "0" },
        title: on ? tr("logs.clickHideAllResources") : tr("logs.clickExposeResources"),
      }),
    }));
  }
  let list: HChild;
  if (!kd.items.length && !(kind === "tools" && kd.disabled && kd.disabled.length)) {
    list = note(kind === "resources" && kd.resourceEnabled === false
      ? tr("logs.resourcesHiddenTurnThem")
      : tr("logs.kind", { kind }));
  } else if (kd.items.length) {
    list = card(kd.items.map((it) => {
      if (kind === "resources") {
        // Read mirrors the tools' Try button: a resource is only believable once you have seen its
        // contents, and its URI is the whole input, so no form is needed.
        return row({
          name: h("code", null, it.uri),
          sub: [it.name, it.description].filter(Boolean).join(" · ") || null,
          primary: btn(tr("logs.read"), { data: { read: it.uri }, title: tr("logs.readResource") }),
        });
      }
      // Tools get Try (it opens Run with this tool already selected) and the client switch: off
      // drops the tool out of tools/list and tells clients to re-list (list_changed).
      return row({
        name: h("code", null, it.name),
        sub: it.description || null,
        // The one line is cut; hovering the row reads the whole of it.
        title: it.description || undefined,
        detail: itemRecordNode(it, kind),
        primary: kind === "tools" ? btn(tr("logs.try"), { data: { try: it.name }, title: tr("logs.tryTool") }) : undefined,
        toggle: kind === "tools"
          ? sw(true, tr("logs.visibleClients2"), { data: { toggle: it.name, on: "1" }, title: tr("logs.visibleClientsClickHide") })
          : undefined,
      });
    }));
  } else {
    list = null;
  }
  // Disabled tools are absent from the live list, so they get their own section - muted, with the
  // switch the other way, so a tool is switched back on from the same place it was switched off.
  const hidden = kind === "tools" && kd.disabled && kd.disabled.length
    ? section({ cap: tr("logs.disabledShownClients") }, card(kd.disabled.map((name) => {
        return row({
          name: h("code", null, name), sub: tr("logs.hiddenToolsListUntil"), muted: true,
          toggle: sw(false, tr("logs.visibleClients2"), { data: { toggle: name, on: "0" }, title: tr("logs.turnTool") }),
        });
      })))
    : null;
  // One section for the live list, so the hidden tools' section sits a full step below it.
  return frag(section({}, resToggle, list, kindPager(kd)), hidden);
}

export { readableBody };
export { callBlockCopyText, callBlockNode, callBodyNode, callItem, callRunOf, callsErrNode, callsStatusNode, copyLogText, fmtChars, fmtJson, goneNode, itemRecordNode, kindBodyNode, logsBodyNode, repaintCallBlock, toggleCall };