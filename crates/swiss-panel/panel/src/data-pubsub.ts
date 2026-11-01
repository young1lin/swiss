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

import type { ApiDbPubsubChannel, ApiDbPubsubChannels, ApiDbPubsubEvent, ApiDbPubsubMessage } from "./types/api.js";
import type { DbPubsubLine, DbPubsubTab } from "./types/state.js";
import type { HChild } from "./h.js";
import { api, apiJson, dbReqGuard, el, toast } from "./util.js";
import { h } from "./h.js";
import { dbConn, dbIsMounted, dbTab } from "./db-state.js";
import { tk, tr, trn } from "./i18n.js";
import { readableBody } from "./logs.js";
import { dbCopyText } from "./data-csv.js";
import { btn, iconBtn } from "./ui/button.js";
import { actionBtn } from "./ui/action.js";
import { valueBlock } from "./ui/json-view.js";
import { filterInput, inlineForm, note, section, split } from "./ui/page.js";
import { dot, tag } from "./ui/status.js";
import type { DotState } from "./ui/status.js";
import { collapseRuns, timeline, timelineMeta, timelineToggle } from "./ui/timeline.js";
import type { TimelineItem } from "./ui/timeline.js";

/* --- pub/sub (SPEC §data.pubsub) ------------------------------------------------------------------------ */
/* A redis connection's channels are not keys: SCAN never sees one, so the key list cannot show
   them and the console cannot SUBSCRIBE (it would take the shared connection out of service).
   This page is one open object per connection, like the Activity monitor (SPEC §data.tabs):
   the channels that have a subscriber right now, polled every 5s WHILE THE TAB IS ACTIVE; a
   live log of what the watched channels and patterns carry, read from an NDJSON feed the
   server holds on a connection of its own; and a publish row. A backgrounded, closed or
   unmounted page aborts its feed - the server gives the subscription back the moment the
   request goes - and coming back subscribes again, the log noting the stretch nobody heard.
   Drawn with the library alone (SPEC §panel.ui): split() for the two columns, section() for
   their heads, the event list for the log, inlineForm() for the rows you type in. */

const DB_PUBSUB_POLL_MS = 5000;
/** The log keeps this many lines; older ones fall off the end. */
const DB_PUBSUB_LOG_MAX = 500;
/** A feed that ended on its own while the page is in front is opened again after this long. */
const DB_PUBSUB_RETRY_MS = 5000;
/** Messages arrive up to 200 a second; the log repaints at most this often. */
const DB_PUBSUB_PAINT_MS = 250;
/** A watch-set change waits this long for the next one before it resubscribes. */
const DB_PUBSUB_RESUB_MS = 300;
/** The server's cap on one side of a watch set (swiss-host PUBSUB_NAMES_MAX). */
const DB_PUBSUB_NAMES_MAX = 32;
/** The names this browser watched, per connection: { [conn]: names, newest first }. A
 *  convenience like the console's history - lost with the browser's storage, never state. */
const DB_PUBSUB_RECENT_KEY = "swiss.pubsubRecent";
/** Recent names kept per connection. */
const DB_PUBSUB_RECENT_MAX = 20;

interface PubsubFeed { ctl: AbortController; tab: DbPubsubTab }
let psFeed: PubsubFeed | null = null;
let psPoll: ReturnType<typeof setInterval> | null = null;
let psRetry: ReturnType<typeof setTimeout> | null = null;
let psPaint: ReturnType<typeof setTimeout> | null = null;
let psResub: ReturnType<typeof setTimeout> | null = null;
let psSeq = 0;
/** The log rows a reader opened, by line seq: a repaint keeps them open. */
const psOpen = new Set<string>();
const psGuard = dbReqGuard();

/* --- pure ---------------------------------------------------------------------------------------- */

/** Split an NDJSON body as it arrives: `rest` is the unfinished line the last chunk ended in,
 *  `chunk` the text just read. Complete lines parse into events; a blank or unparsable line is
 *  skipped rather than ending the feed. Pure. */
function dbPubsubSplit(rest: string, chunk: string): { events: ApiDbPubsubEvent[]; rest: string } {
  const parts = (rest + chunk).split("\n");
  const tail = parts.pop() ?? "";
  const events: ApiDbPubsubEvent[] = [];
  parts.forEach((line: string): void => {
    if (!line.trim()) return;
    try {
      const ev: unknown = JSON.parse(line);
      if (ev && typeof ev === "object" && "event" in ev) events.push(ev as ApiDbPubsubEvent);
    } catch {
      // A line the server never writes; the next one is still a line.
    }
  });
  return { events, rest: tail };
}

/** Append a line to the log, letting the oldest fall off past `max`. Pure over the array. */
function dbPubsubPush(log: DbPubsubLine[], line: DbPubsubLine, max: number): void {
  log.push(line);
  if (log.length > max) log.splice(0, log.length - max);
}

/** A name with a glob character in it (* ? [) is a pattern - PSUBSCRIBE - and anything else a
 *  channel; redis reads `\*` as a literal star inside a pattern, so a channel that really holds
 *  one is still reachable. Pure. */
function dbPubsubIsPattern(name: string): boolean {
  return /[*?[]/.test(name);
}

/** Add a typed name to the watch set, on the side its shape says. Returns the message key that
 *  says why it was not added, or null once it is in. Pure over the two lists. */
function dbPubsubAddName(t: Pick<DbPubsubTab, "watchChannels" | "watchPatterns">, raw: string): string | null {
  const name = raw.trim();
  if (!name) return tk("dataPubsub.nameAChannelOrPattern");
  const side = dbPubsubIsPattern(name) ? t.watchPatterns : t.watchChannels;
  if (side.includes(name)) return tk("dataPubsub.alreadyWatched");
  if (side.length >= DB_PUBSUB_NAMES_MAX) return tk("dataPubsub.tooManyNames");
  side.push(name);
  return null;
}

/** Whether a PSUBSCRIBE pattern matches a channel name, read the way redis's stringmatch reads
 *  it: `*` any run, `?` one character, `[...]` a class (`^` negates, `a-z` a range), `\` takes
 *  the next character literally. For the page's hint only - the server does the matching. Pure. */
function dbPubsubGlob(pattern: string, name: string): boolean {
  const lit = (c: string): string => { return c.replace(/[\\^$.*+?()[\]{}|-]/g, "\\$&"); };
  let re = "";
  for (let i = 0; i < pattern.length; i++) {
    const c = pattern[i];
    if (c === "\\" && i + 1 < pattern.length) {
      re += lit(pattern[++i]);
    } else if (c === "*") {
      re += "[\\s\\S]*";
    } else if (c === "?") {
      re += "[\\s\\S]";
    } else if (c === "[") {
      let j = i + 1;
      const neg = pattern[j] === "^";
      if (neg) j++;
      let cls = "";
      for (; j < pattern.length && pattern[j] !== "]"; j++) {
        if (pattern[j] === "\\" && j + 1 < pattern.length) cls += lit(pattern[++j]);
        else if (pattern[j] === "-" && cls !== "" && j + 1 < pattern.length && pattern[j + 1] !== "]") cls += "-";
        else cls += lit(pattern[j]);
      }
      // An empty class matches nothing, its negation any one character.
      if (cls) re += "[" + (neg ? "^" : "") + cls + "]";
      else re += neg ? "[\\s\\S]" : "(?!)";
      i = j;
    } else {
      re += lit(c);
    }
  }
  try {
    return new RegExp("^" + re + "$").test(name);
  } catch (e) {
    return false;
  }
}

/** A watched channel that a watched pattern also matches - redis delivers each of its messages
 *  twice, once to each subscription - as [channel, pattern], the first pair found. Pure. */
function dbPubsubOverlap(t: Pick<DbPubsubTab, "watchChannels" | "watchPatterns">): [string, string] | null {
  for (const c of t.watchChannels) {
    const p = t.watchPatterns.find((x: string): boolean => { return dbPubsubGlob(x, c); });
    if (p != null) return [c, p];
  }
  return null;
}

/** A name just watched goes to the front of the recent list - a repeat moves up rather than
 *  appearing twice - and the oldest fall off past `max`. Pure: answers a new list. */
function dbPubsubRemember(recent: string[], name: string, max: number): string[] {
  return [name].concat(recent.filter((n: string): boolean => { return n !== name; })).slice(0, max);
}

/** HH:MM:SS.mmm in local time - a gap note's clock, where a second is too coarse. Pure. */
function dbPubsubTime(at: number): string {
  const d = new Date(at);
  const p = (n: number, w: number): string => { return String(n).padStart(w, "0"); };
  return p(d.getHours(), 2) + ":" + p(d.getMinutes(), 2) + ":" + p(d.getSeconds(), 2) + "." + p(d.getMilliseconds(), 3);
}

/** A byte count as the log prints it. Pure. */
function dbPubsubBytes(n: number): string {
  if (n < 1024) return n + " B";
  if (n < 1024 * 1024) return (n / 1024).toFixed(1).replace(/\.0$/, "") + " KiB";
  return (n / 1024 / 1024).toFixed(1).replace(/\.0$/, "") + " MiB";
}

/** The log as event-list items, newest first: a message is its channel and payload - the
 *  pattern that matched it is the who column, shown only when the rows disagree - and identical
 *  consecutive messages fold into ×N; a note is its word and its sentence, amber-tagged when
 *  messages went unseen. Pure. */
function dbPubsubItems(log: DbPubsubLine[]): TimelineItem[] {
  const items: TimelineItem[] = [];
  for (let i = log.length - 1; i >= 0; i--) {
    const l = log[i];
    if (l.line === "note") {
      items.push({
        id: String(l.seq), at: l.at, title: l.label, arg: l.text,
        status: l.bad ? { text: tr("dataPubsub.missed"), tone: "warn" } : undefined,
      });
    } else {
      items.push({
        id: String(l.seq), at: l.at, title: l.channel, arg: l.payload,
        who: l.pattern ? tr("dataPubsub.via", { pattern: l.pattern }) : undefined,
        status: l.truncated ? { text: tr("dataPubsub.cut", { size: dbPubsubBytes(l.size) }), tone: "warn" } : undefined,
        same: [l.channel, l.pattern || "", l.binary ? 1 : 0, l.payload].join("\u0001"),
      });
    }
  }
  return items;
}

/* --- the tab's lifecycle ------------------------------------------------------------------------- */

/** The page came to the front: the listing now and every 5s, and the feed if anything is
 *  watched. */
function dbPubsubStart(): void {
  void dbPubsubLoad();
  dbPubsubPollStart();
  // A repaint of the page already in front (the view refreshing) keeps the feed it has.
  if (!psFeed || psFeed.tab !== dbTab()) void dbPubsubOpen();
}

/** The page left the front - backgrounded, closed, the connection switched, the view left:
 *  no poll, no feed. The feed that was live remembers when it stopped, for the gap note. */
function dbPubsubStop(): void {
  if (psPoll) { clearInterval(psPoll); psPoll = null; }
  if (psPaint) { clearTimeout(psPaint); psPaint = null; }
  dbPubsubClose(true);
}

function dbPubsubPollStart(): void {
  if (psPoll) clearInterval(psPoll);
  psPoll = setInterval((): void => {
    // Self-guarding like the Activity poll: a page no longer in front stops everything.
    if (!dbIsMounted() || dbTab().kind !== "pubsub") { dbPubsubStop(); return; }
    if (document.hidden) return;
    void dbPubsubLoad();
  }, DB_PUBSUB_POLL_MS);
}

/** GET /pubsub under the typed pattern. A failure is the listing's own line, not a toast -
 *  the poll would raise one every five seconds. */
async function dbPubsubLoad(): Promise<void> {
  const conn = dbConn().conn;
  const t = dbTab();
  if (!conn || t.kind !== "pubsub") return;
  const token = psGuard.issue();
  const pattern = t.pattern.trim();
  const path = "/api/db/" + encodeURIComponent(conn) + "/pubsub" + (pattern ? "?pattern=" + encodeURIComponent(pattern) : "");
  let ok = false;
  let body: unknown = null;
  let status = 0;
  try {
    const r = await api(path);
    ok = r.ok;
    status = r.status;
    body = await r.json().catch((): unknown => { return null; });
  } catch {
    status = -1;
  }
  if (!psGuard.accepts(token) || dbTab() !== t) return;
  if (ok && body && typeof body === "object") {
    t.channels = body as ApiDbPubsubChannels;
    t.channelsError = null;
  } else {
    t.channelsError = status === -1 ? tr("util.requestFailedGatewayRunning") : dbPubsubErrorOf(body) || tr("util.httpN", { n: status });
  }
  dbPubsubPaintChannels(t);
}

/** The API's { error } envelope, narrowed. */
function dbPubsubErrorOf(body: unknown): string {
  return body && typeof body === "object" && "error" in body && typeof body.error === "string" ? body.error : "";
}

/** End the feed on purpose. `gap` (the page left the front) keeps when a live feed stopped,
 *  so the next one can say which stretch went unheard; a resubscribe has no gap to report. */
function dbPubsubClose(gap: boolean): void {
  if (psRetry) { clearTimeout(psRetry); psRetry = null; }
  if (psResub) { clearTimeout(psResub); psResub = null; }
  const f = psFeed;
  if (!f) return;
  psFeed = null;
  f.ctl.abort();
  const t = f.tab;
  t.offAt = gap && t.live === "live" ? Date.now() : null;
  t.live = "off";
}

/** Open the feed for the tab in front: POST its watch set, then read NDJSON until the server
 *  ends it or the page aborts it. A refusal before the first event (a bad name, the four-feed
 *  cap, a server that refuses SUBSCRIBE) is the page's error and is not retried; a feed that
 *  ended on its own is opened again in five seconds if the page is still in front. */
async function dbPubsubOpen(): Promise<void> {
  const conn = dbConn().conn;
  const t = dbTab();
  if (!conn || t.kind !== "pubsub" || !dbIsMounted()) return;
  dbPubsubClose(false);
  if (t.stopped || (!t.watchChannels.length && !t.watchPatterns.length)) {
    t.live = "off";
    dbPubsubPaintState(t);
    return;
  }
  const f: PubsubFeed = { ctl: new AbortController(), tab: t };
  psFeed = f;
  t.live = "connecting";
  t.liveNote = null;
  dbPubsubPaintState(t);
  let r: Response;
  try {
    r = await api("/api/db/" + encodeURIComponent(conn) + "/pubsub/subscribe", {
      method: "POST",
      body: JSON.stringify({ channels: t.watchChannels, patterns: t.watchPatterns }),
      signal: f.ctl.signal,
    });
  } catch {
    if (psFeed !== f) return;
    psFeed = null;
    t.live = "error";
    t.liveNote = tr("util.requestFailedGatewayRunning");
    dbPubsubPaintState(t);
    dbPubsubRetryLater(t);
    return;
  }
  if (psFeed !== f) { f.ctl.abort(); return; }
  if (!r.ok || !r.body) {
    const j: unknown = await r.json().catch((): unknown => { return null; });
    if (psFeed !== f) return;
    psFeed = null;
    t.live = "error";
    t.liveNote = dbPubsubErrorOf(j) || tr("util.httpN", { n: r.status });
    dbPubsubPaintState(t);
    return;
  }
  const reader = r.body.getReader();
  const dec = new TextDecoder();
  let rest = "";
  let closed = false;
  try {
    for (;;) {
      const chunk = await reader.read();
      if (chunk.done || psFeed !== f) break;
      const out = dbPubsubSplit(rest, dec.decode(chunk.value, { stream: true }));
      rest = out.rest;
      for (const ev of out.events) {
        if (ev.event === "closed") closed = true;
        dbPubsubEvent(t, ev);
      }
    }
  } catch {
    // The abort, or the connection to the panel dropping; which one is the psFeed check below.
  }
  // Closed on purpose: dbPubsubClose already said so.
  if (psFeed !== f) return;
  psFeed = null;
  if (!closed) {
    // No closed line came: the response itself was cut (the panel restarting, a proxy).
    dbPubsubNote(t, tr("dataPubsub.evCut"), tr("dataPubsub.feedCut"), false);
    t.liveNote = tr("dataPubsub.feedCut");
    dbPubsubSchedulePaint(t);
  }
  t.live = "off";
  t.offAt = Date.now();
  dbPubsubPaintState(t);
  dbPubsubRetryLater(t);
}

/** Open the feed again in five seconds, if the same page is still in front and still wants it. */
function dbPubsubRetryLater(t: DbPubsubTab): void {
  if (psRetry) clearTimeout(psRetry);
  psRetry = setTimeout((): void => {
    psRetry = null;
    if (dbIsMounted() && dbTab() === t && !t.stopped && !psFeed) void dbPubsubOpen();
  }, DB_PUBSUB_RETRY_MS);
}

/** One feed event into the tab's state. A tick only proves the feed is alive: nothing to show. */
function dbPubsubEvent(t: DbPubsubTab, ev: ApiDbPubsubEvent): void {
  if (ev.event === "tick") return;
  if (ev.event === "subscribed") {
    t.live = "live";
    t.liveNote = null;
    if (t.offAt) {
      dbPubsubNote(t, tr("dataPubsub.evGap"), tr("dataPubsub.gap", { from: dbPubsubTime(t.offAt), to: dbPubsubTime(Date.now()) }), true);
      t.offAt = null;
    } else {
      dbPubsubNote(t, tr("dataPubsub.evListening"), ev.channels.concat(ev.patterns).join(", "), false);
    }
    dbPubsubPaintState(t);
  } else if (ev.event === "message") {
    dbPubsubPush(t.log, Object.assign({ line: "message" as const, seq: ++psSeq }, ev), DB_PUBSUB_LOG_MAX);
  } else if (ev.event === "dropped") {
    t.dropped += ev.count;
    dbPubsubNote(t, tr("dataPubsub.evDropped"), trn(ev.count, "dataPubsub.dropped.one", "dataPubsub.dropped.other"), true);
  } else {
    t.live = "off";
    t.liveNote = ev.reason;
    dbPubsubNote(t, tr("dataPubsub.evClosed"), ev.reason, false);
    dbPubsubPaintState(t);
  }
  dbPubsubSchedulePaint(t);
}

function dbPubsubNote(t: DbPubsubTab, label: string, text: string, bad: boolean): void {
  dbPubsubPush(t.log, { line: "note", seq: ++psSeq, label, text, at: Date.now(), bad: bad || undefined }, DB_PUBSUB_LOG_MAX);
}

/** The paint waits while the reader is busy in the log - only the count moves - and tries again
 *  next round; the round loop ends with the page (dbPubsubStop clears it). */
function dbPubsubSchedulePaint(t: DbPubsubTab): void {
  if (psPaint) return;
  psPaint = setTimeout((): void => {
    psPaint = null;
    if (dbTab() !== t) return;
    const box = document.getElementById("dbPsLog");
    if (box && dbPubsubLogBusy(box)) { dbPubsubPaintCount(t); dbPubsubSchedulePaint(t); return; }
    dbPubsubPaintLog(t);
  }, DB_PUBSUB_PAINT_MS);
}

/** A button held down in the log, or text selected in it: a repaint now would replace the nodes
 *  under the pointer or the selection, eating the click or the text being copied. */
function dbPubsubLogBusy(box: HTMLElement): boolean {
  try {
    if (box.matches(":active")) return true;
  } catch (e) {
    /* a DOM that does not know :active - the selection still counts */
  }
  const sel = typeof window.getSelection === "function" ? window.getSelection() : null;
  return !!sel && !sel.isCollapsed && !!sel.anchorNode && box.contains(sel.anchorNode);
}

/* --- the page ------------------------------------------------------------------------------------ */

/** A box the page keeps what is typed in on the tab (data-psin names it for the delegated input). */
function dbPubsubBox(which: string, value: string, placeholder: string, label: string): HTMLInputElement {
  return h("input", {
    type: "text", autocomplete: "off", spellcheck: false, placeholder, aria: { label }, value, data: { psin: which },
  }) as HTMLInputElement;
}

/** The whole page into the pane's grid slot (renderDbGrid's pubsub arm): the live column - its
 *  state, the watch set, the log - beside the channel listing and the publish row. The parts the
 *  poll and the feed repaint have ids of their own, so a repaint never takes the caret out of a box. */
function dbPubsubRender(): void {
  const t = dbTab();
  const wrap = document.getElementById("dbGridWrap");
  if (!wrap || t.kind !== "pubsub") return;
  wrap.textContent = "";
  const pattern = filterInput({ placeholder: tr("dataPubsub.patternPlaceholder"), label: tr("dataPubsub.patternLabel"), value: t.pattern });
  pattern.dataset.psin = "pattern";
  const live = section({
    cap: tr("dataPubsub.messages"),
    tools: [h("span", { id: "dbPsState" }), h("span", { id: "dbPsCount" }), h("span", { id: "dbPsToggle" })],
  },
  h("div", { id: "dbPsNote" }),
  h("div", { id: "dbPsWatch" }),
  h("div", { id: "dbPsLog", role: "log", aria: { live: "off" } }));
  const channels = section({ cap: tr("dataPubsub.channels"), tools: [pattern] }, h("div", { id: "dbPsChannels" }));
  const publish = section({ cap: tr("dataPubsub.publishCap") }, inlineForm(
    dbPubsubBox("pubch", t.pubChannel, tr("dataPubsub.channel"), tr("dataPubsub.publishChannelLabel")),
    dbPubsubBox("pubmsg", t.pubMessage, tr("dataPubsub.message"), tr("dataPubsub.publishMessageLabel")),
    btn(tr("dataPubsub.publish"), { data: { ps: "publish" }, title: tr("dataPubsub.publishTip") })));
  wrap.appendChild(split(live, [channels, publish]));
  dbPubsubPaintChannels(t);
  dbPubsubPaintState(t);
  dbPubsubPaintWatch(t);
  dbPubsubPaintLog(t);
}

/** The channel listing: name, NUMSUB count, Watch. */
function dbPubsubPaintChannels(t: DbPubsubTab): void {
  const box = document.getElementById("dbPsChannels");
  if (!box || dbTab() !== t) return;
  box.textContent = "";
  if (t.channelsError) {
    box.appendChild(note(t.channelsError, { err: true }));
    return;
  }
  const j = t.channels;
  if (!j) {
    box.appendChild(note(tr("dataPubsub.loadingChannels"), { busy: true }));
    return;
  }
  if (!j.channels.length) {
    const none = j.pattern === "*" ? tr("dataPubsub.noChannels") : tr("dataPubsub.noChannelsMatching", { pattern: j.pattern });
    box.appendChild(note(none));
  } else {
    const tbl = el("table", "db-grid");
    const hr = el("tr");
    [tr("dataPubsub.channel"), tr("dataPubsub.subscribers"), ""].forEach((label: string, i: number): void => {
      hr.appendChild(el("th", i === 1 ? "db-nosort tnum" : "db-nosort", label));
    });
    tbl.appendChild(h("thead", null, hr));
    const tbody = el("tbody");
    j.channels.forEach((c: ApiDbPubsubChannel): void => {
      const watched = !c.binary && t.watchChannels.includes(c.name);
      const name = el("td", "db-cell mono", c.name);
      name.title = c.name;
      if (c.binary) name.append(" ", tag(tr("dataPubsub.binary"), { title: tr("dataPubsub.binaryChannelTip") }));
      const act = el("td", "db-rowctl");
      act.appendChild(btn(watched ? tr("dataPubsub.watching") : tr("dataPubsub.watch"), {
        kind: "ghost", disabled: watched || !!c.binary,
        title: c.binary ? tr("dataPubsub.binaryChannelTip") : tr("dataPubsub.watchChannelTip", { channel: c.name }),
        data: { ps: "watch", psname: c.name },
      }));
      tbody.appendChild(h("tr", null, name, el("td", "tnum", String(c.subscribers)), act));
    });
    tbl.appendChild(tbody);
    box.appendChild(tbl);
  }
  if (j.truncated) box.appendChild(note(tr("dataPubsub.truncated", { shown: j.channels.length, total: j.total })));
  // A channel heard only through a pattern has no NUMSUB subscriber, so the listing cannot show
  // it; the count of pattern subscriptions is the one honest trace of them.
  if (j.patterns > 0) box.appendChild(note(trn(j.patterns, "dataPubsub.numpat.one", "dataPubsub.numpat.other")));
}

/** The live column's head - the feed's state, the count, Listen/Stop - and why a feed is off. */
function dbPubsubPaintState(t: DbPubsubTab): void {
  const state = document.getElementById("dbPsState");
  const toggle = document.getElementById("dbPsToggle");
  const why = document.getElementById("dbPsNote");
  if (!state || !toggle || !why || dbTab() !== t) return;
  state.textContent = "";
  toggle.textContent = "";
  why.textContent = "";
  const watching = t.watchChannels.length + t.watchPatterns.length;
  const shown: [DotState, string] = t.live === "live" ? ["up", tr("dataPubsub.live")]
    : t.live === "connecting" ? ["starting", tr("dataPubsub.connecting")]
    : t.live === "error" ? ["error", tr("dataPubsub.refused")]
    : t.stopped ? ["off", tr("dataPubsub.stopped")]
    : ["off", watching ? tr("dataPubsub.notListening") : tr("dataPubsub.nothingWatched")];
  state.append(dot(shown[0], shown[1]), " ", shown[1]);
  if (watching) {
    const on = t.live === "live" || t.live === "connecting";
    toggle.appendChild(btn(on ? tr("dataPubsub.stop") : tr("dataPubsub.listen"), {
      kind: "ghost", data: { ps: on ? "stop" : "listen" },
      title: on ? tr("dataPubsub.stopTip") : tr("dataPubsub.listenTip"),
    }));
  }
  if (t.liveNote && t.live !== "live") why.appendChild(note(t.liveNote, { err: t.live === "error" }));
  dbPubsubPaintCount(t);
}

/** The watch set - one button per name, a click lets it go - then the box that adds one, and
 *  under it the names this browser watched here before and is not watching now, a click away.
 *  Painted when the set changes, not on every feed event, so typing in the box is never
 *  interrupted. */
function dbPubsubPaintWatch(t: DbPubsubTab): void {
  const box = document.getElementById("dbPsWatch");
  if (!box || dbTab() !== t) return;
  box.textContent = "";
  const chip = (name: string, sideTip: string, side: string): HTMLElement => {
    return btn(dbPubsubShown(name), {
      kind: "ghost", icon: "x", title: tr("dataPubsub.unwatch", { name }) + " · " + sideTip,
      data: { ps: "unwatch", psname: name, psside: side },
    });
  };
  const chips: HChild[] = [];
  t.watchChannels.forEach((n: string): void => { chips.push(chip(n, tr("dataPubsub.channelChipTip"), "channel")); });
  t.watchPatterns.forEach((n: string): void => { chips.push(chip(n, tr("dataPubsub.patternChipTip"), "pattern")); });
  box.appendChild(inlineForm(chips.length ? h("span", null, tr("dataPubsub.watching")) : null, ...chips,
    dbPubsubBox("watch", t.watchText, tr("dataPubsub.watchPlaceholder"), tr("dataPubsub.watchLabel")),
    btn(tr("dataPubsub.watch"), { data: { ps: "add" }, title: tr("dataPubsub.watchTip") }),
    chips.length ? null : btn(tr("dataPubsub.watchEverything"), { kind: "ghost", data: { ps: "all" }, title: tr("dataPubsub.watchEverythingTip") })));
  const twice = dbPubsubOverlap(t);
  if (twice) {
    box.appendChild(note(tr("dataPubsub.twice", { channel: twice[0], pattern: dbPubsubShown(twice[1]) }), { data: { pstwice: twice[0] } }));
  }
  const conn = dbConn().conn;
  const recent = (conn ? dbPubsubRecent(conn) : []).filter((n: string): boolean => {
    return !(dbPubsubIsPattern(n) ? t.watchPatterns : t.watchChannels).includes(n);
  });
  if (!recent.length) return;
  box.appendChild(inlineForm(h("span", null, tr("dataPubsub.recent")),
    ...recent.map((n: string): HTMLElement => {
      return btn(dbPubsubShown(n), { kind: "ghost", icon: "history", title: tr("dataPubsub.recentTip", { name: n }), data: { ps: "recent", psname: n } });
    })));
}

/** A watched name as its button reads: the bare `*` says what it means. */
function dbPubsubShown(name: string): string {
  return name === "*" ? tr("dataPubsub.everything") : name;
}

/** "12 messages · 3 dropped" - what the log holds and what the server could not relay. */
function dbPubsubPaintCount(t: DbPubsubTab): void {
  const box = document.getElementById("dbPsCount");
  if (!box) return;
  const n = t.log.reduce((k: number, l: DbPubsubLine): number => { return l.line === "message" ? k + 1 : k; }, 0);
  const s = trn(n, "dataPubsub.nMessages.one", "dataPubsub.nMessages.other");
  box.textContent = t.dropped ? s + " · " + trn(t.dropped, "dataPubsub.nDropped.one", "dataPubsub.nDropped.other") : s;
}

/** The log as the panel's event list, newest on top; the rows a reader opened stay open. */
function dbPubsubPaintLog(t: DbPubsubTab): void {
  const box = document.getElementById("dbPsLog");
  if (!box || dbTab() !== t) return;
  box.textContent = "";
  const items = dbPubsubItems(t.log);
  const ids = new Set(items.map((it: TimelineItem): string => { return it.id; }));
  psOpen.forEach((id: string): void => { if (!ids.has(id)) psOpen.delete(id); });
  if (!items.length) {
    const watching = t.watchChannels.length + t.watchPatterns.length;
    box.appendChild(note(watching ? tr("dataPubsub.logEmpty") : tr("dataPubsub.watchHint")));
  } else {
    box.appendChild(timeline(items, {
      open: psOpen, dayHeads: false,
      body: (_it: TimelineItem, run: TimelineItem[]): HChild => { return dbPubsubBody(t, run); },
    }));
  }
  dbPubsubPaintCount(t);
}

/** An open row: the message's channel - text to select, which the summary button is not - its
 *  size, pattern and caveats on the meta line; Copy channel and Watch only this channel, the way
 *  from a feed watching everything to the one channel worth following; then the payload the
 *  way Logs shows a value, JSON as the code block, with its own Copy. A note is its sentence. */
function dbPubsubBody(t: DbPubsubTab, run: TimelineItem[]): HChild {
  const l = t.log.find((x: DbPubsubLine): boolean => { return String(x.seq) === run[0].id; });
  if (!l) return null;
  if (l.line === "note") return note(l.text);
  const seq = String(l.seq);
  const alone = !t.watchPatterns.length && t.watchChannels.length === 1 && t.watchChannels[0] === l.channel;
  return [
    timelineMeta([
      l.channel,
      dbPubsubBytes(l.size),
      l.pattern ? tr("dataPubsub.via", { pattern: l.pattern }) : null,
      run.length > 1 ? tr("dataPubsub.nSame", { n: run.length }) : null,
      l.binary ? tr("dataPubsub.binaryPayloadTip") : null,
      l.truncated ? tr("dataPubsub.cutTip") : null,
    ]),
    inlineForm(
      actionBtn("copy", tr("dataPubsub.copyChannel"), { ghost: true, data: { ps: "copych", psseq: seq } }),
      btn(tr("dataPubsub.watchOnly"), {
        kind: "ghost", disabled: alone || !!l.channelBinary,
        title: l.channelBinary ? tr("dataPubsub.binaryChannelTip") : tr("dataPubsub.watchOnlyTip", { name: l.channel }),
        data: { ps: "only", psseq: seq },
      })),
    valueBlock({
      label: tr("dataPubsub.payload"),
      tools: [iconBtn("copy", tr("dataPubsub.copyPayload"), { ghost: true, data: { ps: "copymsg", psseq: seq } })],
    }, ...readableBody(l.payload, false)),
  ];
}

/** The message a row's control names by its data-psseq, while the ring still holds it. */
function dbPubsubMessageAt(t: DbPubsubTab, seq: string | undefined): (DbPubsubLine & { line: "message" }) | null {
  const l = t.log.find((x: DbPubsubLine): boolean => { return String(x.seq) === seq; });
  return l && l.line === "message" ? l : null;
}

/* --- acts ---------------------------------------------------------------------------------------- */

/** A watch-set change: Listen again, repaint the set and the listing's Watch buttons, and
 *  resubscribe once the operator stops clicking. */
function dbPubsubWatchChanged(t: DbPubsubTab): void {
  t.stopped = false;
  dbPubsubPaintWatch(t);
  dbPubsubPaintState(t);
  dbPubsubPaintChannels(t);
  if (!t.log.length) dbPubsubPaintLog(t);
  if (psResub) clearTimeout(psResub);
  psResub = setTimeout((): void => {
    psResub = null;
    if (dbIsMounted() && dbTab() === t) void dbPubsubOpen();
  }, DB_PUBSUB_RESUB_MS);
}

function dbPubsubAdd(t: DbPubsubTab, name: string): boolean {
  const why = dbPubsubAddName(t, name);
  if (why) { toast(tr(why, { max: DB_PUBSUB_NAMES_MAX }), true); return false; }
  dbPubsubRecord(name.trim());
  t.watchText = "";
  dbPubsubWatchChanged(t);
  return true;
}

/** Watch one channel alone: the watch set becomes that name, patterns and all let go. */
function dbPubsubWatchOnly(t: DbPubsubTab, name: string): void {
  t.watchChannels = [name];
  t.watchPatterns = [];
  dbPubsubRecord(name);
  dbPubsubWatchChanged(t);
}

/** A name just put in the watch set goes to the front of the connection's recent list. */
function dbPubsubRecord(name: string): void {
  const conn = dbConn().conn;
  if (conn) dbPubsubRecentSave(conn, dbPubsubRemember(dbPubsubRecent(conn), name, DB_PUBSUB_RECENT_MAX));
}

/** The names this browser watched on a connection, newest first. */
function dbPubsubRecent(conn: string): string[] {
  try {
    const raw = localStorage.getItem(DB_PUBSUB_RECENT_KEY);
    const all: unknown = raw ? JSON.parse(raw) : {};
    const list: unknown = typeof all === "object" && all !== null ? (all as Record<string, unknown>)[conn] : null;
    return Array.isArray(list) ? list.filter((n: unknown): n is string => { return typeof n === "string" && n !== ""; }) : [];
  } catch (e) {
    return [];
  }
}

/** Keep a connection's recent names, or drop them all when the list is empty. */
function dbPubsubRecentSave(conn: string, list: string[]): void {
  try {
    const raw = localStorage.getItem(DB_PUBSUB_RECENT_KEY);
    const v: unknown = raw ? JSON.parse(raw) : {};
    const all = typeof v === "object" && v !== null && !Array.isArray(v) ? v as Record<string, string[]> : {};
    if (list.length) all[conn] = list; else delete all[conn];
    localStorage.setItem(DB_PUBSUB_RECENT_KEY, JSON.stringify(all));
  } catch (e) {
    /* full or blocked - the recent list is a convenience, not state */
  }
}

/** Forget what this browser watched on the tab's connection (the ⋯ menu's Clear recent). */
function dbPubsubForget(): void {
  const t = dbTab();
  const conn = dbConn().conn;
  if (t.kind !== "pubsub" || !conn) return;
  dbPubsubRecentSave(conn, []);
  dbPubsubPaintWatch(t);
}

/** PUBLISH through the guarded pipeline, which binds the message as one argument - spaces,
 *  quotes and all. The reply is how many subscribers received it, this page's feed included. */
async function dbPubsubPublish(t: DbPubsubTab): Promise<void> {
  const conn = dbConn().conn;
  if (!conn) return;
  const channel = t.pubChannel;
  if (!channel) { toast(tr("dataPubsub.nameAChannel"), true); return; }
  const j = await apiJson<{ replies?: unknown[] }>("/api/db/" + encodeURIComponent(conn) + "/redis-pipeline", {
    method: "POST",
    body: JSON.stringify({ commands: [["PUBLISH", channel, t.pubMessage]] }),
  });
  if (!j) return;
  const n = Number(j.replies && j.replies.length ? j.replies[0] : 0) || 0;
  toast(trn(n, "dataPubsub.published.one", "dataPubsub.published.other", { channel }));
}

/** Open or close one log row in place - only its summary toggles it; a click inside an open body
 *  is someone reading or selecting the payload. */
function dbPubsubToggleRow(t: DbPubsubTab, sum: Element): boolean {
  const item = sum.closest<HTMLElement>(".tl-item");
  const root = item ? item.closest<HTMLElement>(".tl") : null;
  const id = item ? item.dataset.tlId : undefined;
  if (!root || !id || !root.closest("#dbPsLog")) return false;
  const open = !psOpen.has(id);
  if (open) psOpen.add(id); else psOpen.delete(id);
  const run = collapseRuns(dbPubsubItems(t.log)).find((r: TimelineItem[]): boolean => { return r[0].id === id; }) || [];
  timelineToggle(root, id, open && run.length ? dbPubsubBody(t, run) : undefined);
  return true;
}

/** #pane's delegated click for the pub/sub page (SPEC §panel.toolchain): every control carries
 *  data-ps, a log row is the event list's summary button, and the tab is re-read at event time. */
function dbPubsubClick(target: Element, ev: MouseEvent): boolean {
  const t = dbTab();
  if (t.kind !== "pubsub") return false;
  const sum = target.closest(".tl-sum");
  if (sum) return dbPubsubToggleRow(t, sum);
  const ctl = target.closest<HTMLElement>("[data-ps]");
  if (!ctl) return false;
  const act = ctl.dataset.ps;
  ev.stopPropagation();
  if ((act === "watch" || act === "recent") && ctl.dataset.psname != null) {
    dbPubsubAdd(t, ctl.dataset.psname);
  } else if (act === "add") {
    dbPubsubAdd(t, t.watchText);
  } else if (act === "all") {
    dbPubsubAdd(t, "*");
  } else if (act === "unwatch" && ctl.dataset.psname != null) {
    const name = ctl.dataset.psname;
    const side = ctl.dataset.psside === "pattern" ? t.watchPatterns : t.watchChannels;
    const i = side.indexOf(name);
    if (i >= 0) side.splice(i, 1);
    dbPubsubWatchChanged(t);
  } else if (act === "copych" || act === "copymsg") {
    const l = dbPubsubMessageAt(t, ctl.dataset.psseq);
    if (l) dbCopyText(act === "copych" ? l.channel : l.payload);
  } else if (act === "only") {
    const l = dbPubsubMessageAt(t, ctl.dataset.psseq);
    if (l && !l.channelBinary) dbPubsubWatchOnly(t, l.channel);
  } else if (act === "stop") {
    t.stopped = true;
    dbPubsubClose(true);
    dbPubsubPaintState(t);
  } else if (act === "listen") {
    t.stopped = false;
    void dbPubsubOpen();
  } else if (act === "publish") {
    void dbPubsubPublish(t);
  } else if (act === "refresh") {
    void dbPubsubLoad();
  }
  return true;
}

/** The boxes keep what is typed on the tab, so a repaint or a trip to another tab keeps it. */
let psPatternTimer: ReturnType<typeof setTimeout> | null = null;
function dbPubsubInput(target: Element): boolean {
  const box = target.closest<HTMLInputElement>("[data-psin]");
  const t = dbTab();
  if (!box || t.kind !== "pubsub") return false;
  const which = box.dataset.psin;
  if (which === "pattern") {
    t.pattern = box.value;
    if (psPatternTimer) clearTimeout(psPatternTimer);
    psPatternTimer = setTimeout((): void => { psPatternTimer = null; void dbPubsubLoad(); }, DB_PUBSUB_RESUB_MS);
  } else if (which === "watch") {
    t.watchText = box.value;
  } else if (which === "pubch") {
    t.pubChannel = box.value;
  } else if (which === "pubmsg") {
    t.pubMessage = box.value;
  }
  return true;
}

/** Enter adds the typed name - the caret stays in the box for the next one - or publishes from
 *  either publish box. */
function dbPubsubKeydown(target: Element, ev: KeyboardEvent): boolean {
  const box = target.closest<HTMLInputElement>("[data-psin]");
  const t = dbTab();
  if (!box || t.kind !== "pubsub") return false;
  if (ev.key !== "Enter" || ev.isComposing) return true;
  ev.preventDefault();
  const which = box.dataset.psin;
  if (which === "watch") {
    if (dbPubsubAdd(t, box.value)) {
      const again = document.querySelector<HTMLInputElement>("#dbPsWatch [data-psin='watch']");
      if (again) again.focus();
    }
  } else if (which === "pubch" || which === "pubmsg") {
    void dbPubsubPublish(t);
  } else if (which === "pattern") {
    void dbPubsubLoad();
  }
  return true;
}

/** Empty the log and the dropped count; the feed keeps running. */
/* --- the Data agent's reads (SPEC §data.agent) ----------------------------------------------------- */

/** What the Data agent read of pub/sub, shown on this page once the agent pane opened it: a
 *  listing puts the agent's pattern in the box and its listing in the aside, the poll carrying
 *  on under that pattern; a listen puts the names it subscribed to into the watch set - the
 *  page's own feed then listens on them live - and what it heard into the log under an Agent
 *  line. The reader wants what the agent reads made visible (2026-10-29): a prompt naming
 *  pub/sub ended on a run_redis result, the channels the agent read never shown. */
function dbPubsubAgentRead(frame: Record<string, unknown>): void {
  const t = dbTab();
  if (t.kind !== "pubsub") return; // the strip had no room for the page
  if (frame.kind === "channels") {
    const listing = frame.listing as ApiDbPubsubChannels | undefined;
    if (!listing || !Array.isArray(listing.channels)) return;
    t.pattern = listing.pattern === "*" ? "" : listing.pattern;
    t.channels = listing;
    t.channelsError = null;
    dbPubsubRender();
    // Under the agent's pattern from here: this load supersedes the one the open asked for.
    void dbPubsubLoad();
    return;
  }
  const names = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x: unknown): x is string => typeof x === "string") : []);
  const watched = names(frame.channels).concat(names(frame.patterns));
  const heard = (Array.isArray(frame.messages) ? frame.messages as ApiDbPubsubEvent[] : [])
    .filter((m: ApiDbPubsubEvent): m is ApiDbPubsubMessage => { return !!m && m.event === "message"; });
  for (const m of heard) dbPubsubPush(t.log, Object.assign({ line: "message" as const, seq: ++psSeq }, m), DB_PUBSUB_LOG_MAX);
  const dropped = typeof frame.dropped === "number" ? frame.dropped : 0;
  t.dropped += dropped;
  const seconds = typeof frame.seconds === "number" ? frame.seconds : 0;
  const said = [trn(heard.length, "dataPubsub.agentHeard.one", "dataPubsub.agentHeard.other", { seconds: seconds.toFixed(1), names: watched.join(", ") })];
  if (dropped) said.push(trn(dropped, "dataPubsub.dropped.one", "dataPubsub.dropped.other"));
  if (typeof frame.ended === "string" && frame.ended) said.push(frame.ended);
  // After the messages, so the newest-first log reads the Agent line above what it heard.
  dbPubsubNote(t, tr("dataPubsub.evAgent"), said.join(" · "), false);
  let added = false;
  for (const name of watched) {
    if (dbPubsubAddName(t, name) === null) { dbPubsubRecord(name); added = true; }
  }
  // A name new to the watch set resubscribes the page's feed; names already watched leave it be.
  if (added) dbPubsubWatchChanged(t);
  dbPubsubPaintLog(t);
}

/** The agent stopped the page's live feed (the stop_listen tool): that feed belongs to the page
 *  and outlives the conversation that started nothing of it, so a tool can only ask it to close -
 *  the reader's own Stop, with a line in the log saying who ended it. False when the page has no
 *  Pub/Sub tab in front, which is the only place a feed exists. */
function dbPubsubAgentStop(): boolean {
  const t = dbTab();
  if (t.kind !== "pubsub") return false;
  const was = t.live !== "off";
  dbPubsubClose(false);
  t.live = "off";
  if (was) dbPubsubNote(t, tr("dataPubsub.evAgent"), tr("dataPubsub.agentStopped"), false);
  dbPubsubRender();
  return true;
}

function dbPubsubClear(): void {
  const t = dbTab();
  if (t.kind !== "pubsub") return;
  t.log = [];
  t.dropped = 0;
  dbPubsubPaintLog(t);
}

export {
  DB_PUBSUB_LOG_MAX, dbPubsubAddName, dbPubsubAgentRead, dbPubsubAgentStop, dbPubsubBytes, dbPubsubClear, dbPubsubClick, dbPubsubForget, dbPubsubGlob, dbPubsubInput,
  dbPubsubIsPattern, dbPubsubItems, dbPubsubKeydown, dbPubsubLoad, dbPubsubLogBusy, dbPubsubOverlap, dbPubsubPush, dbPubsubRemember,
  dbPubsubRender, dbPubsubSplit, dbPubsubStart, dbPubsubStop, dbPubsubTime,
};
