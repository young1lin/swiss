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
import type { JtBox, PhantomMcpRow } from "./types/dom.js";
import type { McpDetail, McpKind } from "./types/state.js";
import { iconNode, toast } from "./util.js";
import { frag, h } from "./h.js";
import type { HChild } from "./h.js";
import { rowOf } from "./sidebar.js";
import { mcpDetail } from "./mcp-state.js";
import { locale, tr } from "./i18n.js";

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

/** docs/33 C2: a block is tree material only when it is a JSON object or array. Plain strings,
 *  error text and a truncated payload that no longer parses keep the C1 pre. */
function parseJsonBlock(text: string): unknown {
  if (!text) return null;
  const trimmed = text.trim();
  if (trimmed.charAt(0) !== "{" && trimmed.charAt(0) !== "[") return null;
  try { return JSON.parse(trimmed); } catch (e) { return null; }
}

/* --- docs/33 C2: the collapsible tree ----------------------------------------------------------
   A pure DOM builder: children render lazily on first expand, so the built DOM stays proportional
   to what the operator actually opened. Expansion lives in the caller's `open` map (path -> bool)
   — the 6-second repaint rebuilds the tree from it and the open nodes survive. Containers shallower
   than depth 2 record an initial `true` once, so defaults and explicit toggles share one truth. */
function jtDiv(cls: string): HTMLDivElement { const n = document.createElement("div"); n.className = cls; return n; }
function jtSpan(cls: string, text: string): HTMLSpanElement { const n = document.createElement("span"); n.className = cls; n.textContent = text; return n; }
function jtBtn(cls: string, title: string): HTMLButtonElement {
  const b = document.createElement("button");
  b.type = "button"; b.className = "btn icon " + cls; b.title = title;
  b.setAttribute("aria-label", title);
  return b;
}
function jtSummary(v: unknown): string {
  if (Array.isArray(v)) return v.length ? "[" + v.length + "]" : "[]";
  const n = Object.keys(v as Record<string, unknown>).length;
  return n ? "{" + n + "}" : "{}";
}
function jtCopyText(v: unknown): string {
  if (v !== null && typeof v === "object") return JSON.stringify(v, null, 2);
  if (typeof v === "string") return v; // the scalar text, unquoted
  return String(v);
}

function jtNode(parentEl: HTMLElement, key: string | number, val: JtBox, path: string, open: Record<string, boolean>): void {
  const isObj = val !== null && typeof val === "object";
  if (isObj && open[path] === undefined) open[path] = false; // top-level shape first; details on demand
  const row = jtDiv("jt-row" + (isObj && open[path] ? " open" : ""));
  // Keys identify fields, so they use the text face and omit JSON's punctuation-heavy quotes.
  // The block copy action still returns exact, valid JSON.
  const keySpan = jtSpan("jt-key", String(key));
  const colSpan = jtSpan("jt-col", ":");
  const cp = jtBtn("jt-copy", tr("logs.copyValue"));
  cp.dataset.copy = "1";
  cp.appendChild(iconNode("copy"));
  cp.onclick = () => { void copyLogText(jtCopyText(val)); };
  if (isObj) {
    const chev = jtBtn("jt-chev", tr("logs.toggle"));
    chev.appendChild(iconNode("chevron-right"));
    const kids = jtDiv("jt-kids");
    const renderKids = () => {
      while (kids.firstChild) kids.removeChild(kids.firstChild);
      if (!open[path]) return;
      const entries: [string | number, unknown][] = Array.isArray(val)
        ? val.map((v: JtBox, i: number): [number, unknown] => { return [i, v]; })
        : Object.keys(val).map((k: string): [string, unknown] => { return [k, val[k]]; });
      entries.forEach((e) => { jtNode(kids, e[0], e[1] as JtBox, path + e[0] + "/", open); });
    };
    const setChevronLabel = () => {
      const label = open[path] ? tr("logs.collapseKey", { key: String(key) }) : tr("logs.expandKey", { key: String(key) });
      chev.title = label;
      chev.setAttribute("aria-label", label);
      chev.setAttribute("aria-expanded", open[path] ? "true" : "false");
    };
    setChevronLabel();
    chev.onclick = () => {
      open[path] = !open[path];
      row.className = "jt-row" + (open[path] ? " open" : "");
      setChevronLabel();
      renderKids();
    };
    row.appendChild(chev);
    row.appendChild(keySpan);
    row.appendChild(colSpan);
    row.appendChild(jtSpan("jt-sum", jtSummary(val)));
    row.appendChild(cp);
    if (open[path]) renderKids();
    const wrap = jtDiv("jt-node");
    wrap.appendChild(row);
    wrap.appendChild(kids);
    parentEl.appendChild(wrap);
  } else {
    row.appendChild(jtDiv("jt-spc"));
    row.appendChild(keySpan);
    row.appendChild(colSpan);
    const text = typeof val === "string" ? JSON.stringify(val) : String(val);
    // Strings use the neutral base style; numbers and literals keep semantic hooks for alignment.
    const cls = typeof val === "number" ? " n" : typeof val === "string" ? "" : " b";
    row.appendChild(jtSpan("jt-val" + cls, text.length > 200 ? text.slice(0, 200) + "…" : text));
    row.appendChild(cp);
    parentEl.appendChild(row);
  }
}

/** docs/33 C2: one block's body — a tree slot when the value is an object/array, a plain
 *  escaped pre for everything else (plain strings, error text, a payload truncated mid-JSON).
 *  C1's pre highlighter is gone with the tree here: a pre that reached hlJson could never have
 *  parsed, so hlJson could only ever return its input — dead code, deleted, not kept.
 *
 *  A NODE since docs/37 R5: the pre's payload is a text node, and the tree slot is the empty
 *  div buildJsonTree fills — mountBlock finds it by its data-jtree attribute either way. */
function blockNode(raw: string | null | undefined, kind: string, seq: number, ok: boolean): HTMLElement {
  const pretty = fmtJson(raw);
  if (parseJsonBlock(pretty) != null) return h("div", { class: "jtree", data: { jtree: kind + ":" + seq } });
  return h("pre", { class: "logs" + (ok ? "" : " err"), data: kind === "out" ? { out: seq } : {} },
    pretty || (kind === "args" ? tr("logs.none") : tr("logs.empty")));
}

/** Build the tree into `host` (a .jtree slot). Rebuildable any number of times.
 *  The root is the block itself, so painting a second synthetic root row only wastes a line. Show
 *  its immediate fields and keep every nested container folded until the operator asks for it. */
function buildJsonTree(host: HTMLElement, value: JtBox, open?: Record<string, boolean>): void {
  open = open || {};
  // Keep BOTH classes: jtree is the container chrome (ground, radius, max-height), jtree-in marks
  // the built state. Assigning only one would strip the other (found live on 19998).
  host.className = "jtree jtree-in";
  while (host.firstChild) host.removeChild(host.firstChild);
  const entries: [string | number, unknown][] = Array.isArray(value)
    ? value.map((v: JtBox, i: number): [number, unknown] => { return [i, v]; })
    : Object.keys(value).map((k: string): [string, unknown] => { return [k, value[k]]; });
  if (!entries.length) {
    host.appendChild(jtSpan("jt-empty", Array.isArray(value) ? "[]" : "{}"));
    return;
  }
  entries.forEach((e) => { jtNode(host, e[0], e[1] as JtBox, "/" + e[0] + "/", open); });
}

/** docs/33 C2: mount every open row's tree blocks. Called after a repaint (the poll rebuilds
 *  the DOM every 6s on page 0), when a row first opens, and when a full reply lands. Expansion
 *  comes back from d.callsTree, so the operator's open nodes survive all three. */
function mountJsonTrees(d: McpDetail, onlySeq?: number | string | null): void {
  if (typeof document === "undefined" || !document.querySelectorAll) return;
  // Callers hand a DATASET value in ("4" from data-callseq / data-full), the log stores numbers —
  // normalize once here or `seq !== onlySeq` silently skips every row (found live on 19998).
  if (onlySeq != null) onlySeq = Number(onlySeq);
  document.querySelectorAll("#tabbody .call.open").forEach((rowEl) => {
    const seq = Number(rowEl.getAttribute("data-seq"));
    if (!seq || (onlySeq != null && seq !== onlySeq)) return;
    const c = (d.calls || []).find((r) => { return r.seq === seq; });
    if (!c) return;
    mountBlock(rowEl, d, c, "args", c.args);
    mountBlock(rowEl, d, c, "out", d.callsFull[seq] != null ? d.callsFull[seq] : c.output);
  });
}

/** One block: build the tree when the value parses. A preview that was truncated mid-JSON painted
 *  a pre; once the full reply parses, the out block's pre is upgraded to a tree in place. */
function mountBlock(rowEl: Element, d: McpDetail, c: ApiMcpCallRow, kind: string, raw: string | null): void {
  const value = parseJsonBlock(fmtJson(raw || ""));
  if (value == null) return;
  let slot = rowEl.querySelector<HTMLElement>('[data-jtree="' + kind + ":" + c.seq + '"]');
  if (!slot) {
    if (kind !== "out") return; // args are never truncated — no pre to upgrade
    const pre = rowEl.querySelector("pre[data-out]");
    if (!pre) return;
    slot = document.createElement("div");
    slot.className = "jtree";
    slot.setAttribute("data-jtree", "out:" + c.seq);
    pre.replaceWith(slot);
  }
  if (!d.callsTree) d.callsTree = {};
  if (!d.callsTree[c.seq]) d.callsTree[c.seq] = {};
  buildJsonTree(slot, value as JtBox, d.callsTree[c.seq]);
}

/** docs/33 C1: a block label row — the caption plus the copy affordance for that block. */
function lblNode(text: string, kind: string, seq: number): HTMLElement {
  const label = kind === "args" ? tr("logs.copyArguments") : tr("logs.copyResult");
  return h("div", { class: "call-lbl" },
    text,
    h("button", { class: "btn icon", data: { copy: kind + ":" + seq }, aria: { label: label }, title: label },
      iconNode("copy")));
}

function callNode(d: McpDetail, c: ApiMcpCallRow): HTMLElement {
  const when = new Date(c.at).toLocaleString(locale());
  const meta = c.client
    ? tr("logs.whenClientMsMs", { when, via: c.via, client: c.client, ms: c.ms, chars: fmtChars(c.chars) })
    : tr("logs.whenMsMsChars", { when, via: c.via, ms: c.ms, chars: fmtChars(c.chars) });
  // A page ships only the head of each reply. Offer the rest instead of leaving a dangling tail.
  const full = d.callsFull[c.seq];
  const body = full != null ? full : c.output;
  const more = c.preview && full == null
    ? h("div", { class: "form-actions" },
        h("button", { class: "btn", data: { full: c.seq } }, tr("logs.showFullResultChars", { chars: fmtChars(c.chars) })))
    : null;
  return h("div", { class: "call" + (d.callsOpen[c.seq] ? " open" : ""), data: { seq: c.seq } },
    h("div", { class: "call-sum", data: { callseq: c.seq }, role: "button", tabIndex: 0 },
      h("span", { class: "chev", aria: { hidden: "true" } }, iconNode("chevron-right")),
      h("span", { class: "dot " + (c.ok ? "up" : "down") }),
      h("span", { class: "call-tool" }, c.tool),
      h("span", { class: "call-arg" }, c.args || tr("logs.arguments")),
      h("span", { class: "call-meta" }, meta)),
    h("div", { class: "call-body" },
      lblNode(tr("logs.arguments2"), "args", c.seq),
      blockNode(c.args, "args", c.seq, true),
      lblNode(c.ok ? tr("logs.result") : tr("logs.error"), "out", c.seq),
      blockNode(body, "out", c.seq, c.ok),
      more));
}

/** The pager's status cell: the committed page number, with the pending suffix while a switch
 *  is in flight. The number NEVER shows the target — pending must not pretend to be committed
 *  (docs/32 B1). One builder for the full paint and the in-place chrome patch, so they cannot drift. */
function callsStatusNode(d: McpDetail): HChild {
  const page = d.callsPage + 1;
  return d.callsPendingPage != null
    ? [tr("logs.pageN", { n: page }), h("span", { class: "spin" }), tr("logs.loading")]
    : tr("logs.pageN2", { n: page });
}

/** A failed foreground load, in place: the sentence and the way out, nothing else. The HTTP
 *  status is the only secondary text — a response body never lands in the panel (docs/32 B1). */
function callsErrNode(d: McpDetail): HTMLElement {
  // The sentence is set once, in the state field (detail.js callsLoadFailed) — the node renders
  // the field, so the copy cannot drift between the two.
  return h("div", { class: "calls-err", id: "clErr", role: "status" },
    h("span", d.callsError || tr("logs.couldLoadCalls")),
    d.callsErrStatus ? h("span", { class: "calls-err-why" }, d.callsErrStatus) : null,
    h("button", { class: "btn", id: "clRetry" }, tr("logs.retry")));
}

function logsBodyNode(d: McpDetail): HChild {
  if (d.calls == null && !d.callsError) {
    /* First open, before anything is there to keep in place (docs/32 B1). */
    return h("div", { class: "note" }, h("span", { class: "spin" }), " ", tr("logs.loadingCalls"));
  }
  /* docs/31: server-side search over the stored calls. The input re-renders with the page, but
   * renderCallsOnly swaps the LIVE node back in, so focus and caret survive a result repaint. */
  const q = d.callsQ || "";
  const head = h("div", { class: "sec-head" },
    h("span", { class: "sec-cap" }, tr("logs.toolCallsNewestFirst")),
    h("input", { id: "callsQ", type: "search", placeholder: tr("logs.searchCalls"), aria: { label: tr("logs.searchToolCalls") }, value: q }),
    h("button", { class: "btn icon", id: "clMenu", aria: { label: tr("logs.moreLogActions") }, title: tr("logs.moreLogActions") }, iconNode("ellipsis")));
  const busy = d.callsPendingPage != null;
  let body: HChild;
  let pager: HChild = null;
  if (d.calls == null) {
    body = callsErrNode(d); // the very first load failed — the error replaces the shell, not the rows
  } else {
    body = d.calls.length
      ? h("div", { class: "group" }, d.calls.map((c) => { return callNode(d, c); }))
      : h("div", { class: "group" },
          h("div", { class: "row" },
            h("span", { class: "rowmsg" },
              d.callsPage
                ? tr("logs.nothingPage")
                : q
                  ? tr("logs.callsMatchingQ", { q })
                  : tr("logs.callsEveryToolInvocation"))));
    /* Both directions go quiet while a switch is pending; the number stays the committed page. */
    pager = (d.callsPage > 0 || d.callsMore)
      ? h("div", { class: "pager", id: "clPager", role: "navigation", aria: { label: tr("logs.callLogPages") } },
          h("button", { class: "btn", id: "clPrev", disabled: busy || d.callsPage <= 0 }, tr("logs.newer")),
          h("span", { class: "calls-status", id: "clStatus", aria: { live: "polite" } }, callsStatusNode(d)),
          h("button", { class: "btn", id: "clNext", disabled: busy || !d.callsMore }, tr("logs.older")))
      : null;
  }
  const errAgain = d.callsError && d.calls != null ? callsErrNode(d) : null;
  /* One region for rows/pager/error lets a pending switch mark itself busy in place, without
   * touching the search box above it or the stderr section below it (docs/32 B1). */
  const region = h("div", { class: "calls", id: "callsRegion", aria: { busy: busy ? "true" : "false" } },
    body, pager, errAgain);
  let err: HChild = null;
  if (d.stderr) {
    err = frag(
      h("div", { class: "sec-head", style: "padding-top:var(--s5)" }, h("span", { class: "sec-cap" }, tr("logs.childProcessStderr"))),
      h("div", { class: "group" }, h("pre", { class: "logs" }, d.stderr)));
  } else if ((d.config && d.config.type) === "proc") {
    /* An empty stderr on a proc answers a different question depending on whether a child exists
       yet. Blank space here reads as "logs went missing" — say which of the three blanks it is. */
    const st = (rowOf(d.name) || {} as ApiMcpRow).lifecycle;
    const why = st === "idle"
      ? tr("logs.childProcessLazyProc")
      : st === "error"
        ? tr("logs.childPrintedNothingBefore")
        : tr("logs.childRunningButPrinted");
    err = frag(
      h("div", { class: "sec-head", style: "padding-top:var(--s5)" }, h("span", { class: "sec-cap" }, tr("logs.childProcessStderr"))),
      h("div", { class: "group" }, h("div", { class: "row" }, h("span", { class: "rowmsg" }, why))));
  }
  return frag(head, region, err);
}

/** Expand/collapse one call without re-rendering: a poll must not close what you just opened. */
function toggleCall(seq: number): void {
  const d = mcpDetail();
  if (!d) return;
  d.callsOpen[seq] = !d.callsOpen[seq];
  const node = document.querySelector('#tabbody .call[data-seq="' + seq + '"]');
  if (node) node.className = "call" + (d.callsOpen[seq] ? " open" : "");
  if (d.callsOpen[seq]) mountJsonTrees(d, seq); // docs/33 C2: trees build when the row opens
}

/** One truncated line of argument names, required ones in bold. `args` is [{ name, req }].
 *  The title carries the full list, because the line is clipped rather than wrapped and a tool with
 *  eight arguments would otherwise lose the last four with no way to see them from here.
 *  A NODE since docs/37 R5: the bold distinction is an element, not markup glued around a name. */
function argLineNode(args: { name: string; req?: boolean }[]): HTMLElement | null {
  if (!args.length) return null;
  const plain = args.map((a) => { return a.name; }).join(", ");
  return h("div", { class: "args", title: plain },
    args.map((a) => { return a.req ? h("b", null, a.name) : a.name; }).reduce<HChild[]>(function (out, piece, i) {
      if (i) out.push(", ");
      out.push(piece);
      return out;
    }, []));
}

/** A tool stays one quiet row until the user asks for its complete MCP metadata. */
function toolDetailNode(it: ApiMcpItem, args: HChild): HTMLElement {
  const description = it.description || tr("logs.descriptionProvided");
  const schema = it.inputSchema ? JSON.stringify(it.inputSchema, null, 2) : tr("logs.inputSchemaProvided");
  return h("details", { class: "item-detail" },
    h("summary", { title: description },
      h("span", { class: "item-chev" }, iconNode("chevron-right")),
      h("span", { class: "item-summary" },
        h("span", { class: "name" }, it.name),
        h("span", { class: "desc item-teaser" }, description),
        args)),
    h("div", { class: "item-full" },
      h("div", { class: "item-full-label" }, tr("logs.fullDescription")),
      h("div", { class: "item-full-text" }, description),
      h("div", { class: "item-full-label" }, tr("logs.inputSchema")),
      h("pre", { class: "item-schema" }, schema)));
}

function kindBodyNode(d: McpDetail, kind: McpKind, m: ApiMcpRow | PhantomMcpRow): HChild {
  const kd = d[kind];
  // Resources get a master on/off at the top: off empties the list (the capability stays, so the
  // notify stays valid) and tells connected clients to re-list. Symmetric with the per-tool toggle.
  let resToggle: HTMLElement | null = null;
  if (kind === "resources" && kd.loaded) {
    const on = kd.resourceEnabled !== false;
    resToggle = h("div", { class: "row row-act" },
      h("div", { class: "row-main" },
        h("div", { class: "name" }, tr("logs.exposeResources")),
        h("div", { class: "desc" }, on ? tr("logs.visibleClients") : tr("logs.hiddenClientsSeeResources"))),
      h("button", {
        class: "sw", role: "switch",
        aria: { checked: on ? "true" : "false", label: tr("logs.exposeResources") },
        data: { restog: on ? "1" : "0" },
        title: on ? tr("logs.clickHideAllResources") : tr("logs.clickExposeResources"),
      }));
  }
  if (m.lifecycle !== "started") {
    return h("div", { class: "group" },
      h("div", { class: "row" }, h("span", { class: "rowmsg" }, tr("logs.startedStartListKind", { kind }))));
  }
  if (kd.loading && !kd.loaded) {
    return h("div", { class: "note" }, h("span", { class: "spin" }), " ", tr("logs.loadingKind", { kind }));
  }
  if (kd.error) {
    // Keep the pager: pageNext/pagePrev push the cursor optimistically, so a failed page used to
    // leave the tab with no way back except the global Refresh (which resets to page one).
    const errPager = kd.cursors.length > 1
      ? h("div", { class: "pager" },
          h("button", { class: "btn", id: "pgPrev" }, tr("logs.previous")),
          h("span", null, tr("logs.pageN2", { n: kd.cursors.length })),
          h("button", { class: "btn", id: "pgNext", disabled: !kd.nextCursor }, tr("logs.next")))
      : null;
    return frag(
      h("div", { class: "group" }, h("div", { class: "row" }, h("span", { class: "rowmsg warn" }, kd.error))),
      errPager);
  }
  let rows: HChild;
  if (!kd.items.length && !(kind === "tools" && kd.disabled && kd.disabled.length)) {
    // An empty list still needs the toggle bar above it (resources can be hidden, not just absent),
    // so this is composed into `rows` and the resToggle header is added by the normal return below
    // rather than short-circuiting out of the function.
    const emptyMsg = kind === "resources" && kd.resourceEnabled === false
      ? tr("logs.resourcesHiddenTurnThem")
      : tr("logs.kind", { kind });
    rows = h("div", { class: "row" }, h("span", { class: "rowmsg" }, emptyMsg));
  } else {
    rows = kd.items.map((it) => {
      if (kind === "resources") {
        // Read mirrors the tools' Try button: a resource is only believable once you have seen its
        // contents, and its URI is the whole input, so no form is needed.
        return h("div", { class: "row row-act" },
          h("div", { class: "row-main" },
            h("div", { class: "name" }, it.uri),
            it.name ? h("div", { class: "desc" }, it.name) : null,
            it.description ? h("div", { class: "desc" }, it.description) : null),
          h("button", { class: "btn", data: { read: it.uri }, title: tr("logs.readResource") }, tr("logs.read")));
      }
      let args: HChild = null;
      if (kind === "prompts" && it.arguments && it.arguments.length) {
        args = argLineNode(it.arguments.map((a) => { return { name: a.name, req: !!a.required }; }));
      }
      if (kind === "tools" && it.inputSchema && it.inputSchema.properties) {
        const req = it.inputSchema.required || [];
        args = argLineNode(Object.keys(it.inputSchema.properties).map((k) => {
          return { name: k, req: req.includes(k) };
        }));
      }
      const main = h("div", { class: "row-main" },
        h("div", { class: "name" }, it.name),
        it.description ? h("div", { class: "desc" }, it.description) : null,
        args);
      // Tools get a Try button: it opens Run with this tool already selected, so a tool can be
      // exercised from the list it was found in. A toggle turns the tool off for clients — it drops
      // out of tools/list and clients are told to re-list (notifications/tools/list_changed).
      if (kind === "tools") {
        return h("div", { class: "row row-act item-row" },
          toolDetailNode(it, args),
          h("button", { class: "btn", data: { try: it.name }, title: tr("logs.tryTool") }, tr("logs.try")),
          h("button", {
            class: "sw", role: "switch",
            aria: { checked: "true", label: tr("logs.visibleClients2") },
            data: { toggle: it.name, on: "1" },
            title: tr("logs.visibleClientsClickHide"),
          }));
      }
      return h("div", { class: "row row-b" }, main);
    });
  }

  // Disabled tools are absent from the live list, so they get their own rows here — greyed, with the
  // toggle the other way, so a tool can be switched back on from the same place it was switched off.
  const disabledRows: HChild[] = [];
  if (kind === "tools" && kd.disabled && kd.disabled.length) {
    disabledRows.push(h("div", { class: "group-cap" }, tr("logs.disabledShownClients")));
    kd.disabled.forEach((name) => {
      disabledRows.push(h("div", { class: "row row-act muted" },
        h("div", { class: "row-main" },
          h("div", { class: "name" }, name),
          h("div", { class: "desc" }, tr("logs.hiddenToolsListUntil"))),
        h("button", {
          class: "sw", role: "switch",
          aria: { checked: "false", label: tr("logs.visibleClients2") },
          data: { toggle: name, on: "0" },
          title: tr("logs.turnTool"),
        })));
    });
  }

  const page = kd.cursors.length;
  const pages = kd.total != null ? tr("logs.pageNM", { n: page, m: Math.max(1, Math.ceil(kd.total / kd.pageSize)) }) : tr("logs.pageN2", { n: page });
  const pager = (page > 1 || kd.nextCursor)
    ? h("div", { class: "pager" },
        h("button", { class: "btn", id: "pgPrev", disabled: page <= 1 }, tr("logs.previous")),
        h("span", null, pages + " ", kd.loading ? h("span", { class: "spin" }) : null),
        h("button", { class: "btn", id: "pgNext", disabled: !kd.nextCursor }, tr("logs.next")))
    : null;
  return frag(
    resToggle ? h("div", { class: "group" }, resToggle) : null,
    h("div", { class: "group" }, rows, disabledRows),
    pager);
}

export { argLineNode, blockNode, buildJsonTree, callNode, callsErrNode, callsStatusNode, copyLogText, fmtChars, fmtJson, kindBodyNode, lblNode, logsBodyNode, mountJsonTrees, parseJsonBlock, toggleCall, toolDetailNode };