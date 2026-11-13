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

/* The Lane page (SPEC §lane.panel): who holds the machine-wide lane, who waits, and what
 * finished. The lane changes when a command enters, starts or ends - rarely - so the page
 * repaints whole on each LANE.QUEUE notice and has no timer; clock() rewords the ages from
 * the timestamps it already holds. Release is the one action: the lane stops counting a stuck
 * holder, whose command runs on. */
import { $, api, targetEl, toast } from "../util.js";
import { fill, h } from "../h.js";
import { servedError, tr, trn } from "../i18n.js";
import { btn } from "../ui/button.js";
import { dot, tag } from "../ui/status.js";
import { card, note, paneBody, paneHead, section } from "../ui/page.js";
import { row } from "../ui/row.js";
import { durShort, relTime } from "../ui/timeline.js";
import { busOn } from "../bus/bus.js";
import { coalescedRead } from "../bus/live.js";
import { Module, NoticeSchema } from "../bus/pb/swiss/bus/v1/bus_pb.js";
import { LaneQueueId, LaneSub } from "../bus/pb/swiss/bus/v1/lane_pb.js";
                                                                     

                   
                              
                      
                                              
                     
 

let active                 = null;
let lane                         = null;
const releasing = new Set        ();

function current(s         )          { return active === s && !s.controller.signal.aborted; }

/* The age a running or waiting row shows, from the moment it carries in data-laneat. */
function ageText(kind        , at        , now        )         {
  const d = durShort(Math.max(0, now - at) / 1000);
  return tr(kind === "running" ? "lane.runningFor" : "lane.waitingFor", { d });
}

function capsText(env                        )         {
  const caps = Object.entries(env).map(([k, v]) => k + "=" + v);
  return caps.length ? tr("lane.caps", { caps: caps.join("  ") }) : tr("lane.capsNone");
}

function headSub(l                 )         {
  return trn(l.capacity, "lane.slotsOne", "lane.slotsOther", { n: String(l.capacity) }) + " · " + capsText(l.env);
}

function liveRow(e              , kind                       , now        )              {
  const at = kind === "running" ? e.startedAt ?? e.enteredAt : e.enteredAt;
  const busy = releasing.has(e.id);
  return row({
    lead: dot(kind === "running" ? "up" : "idle", tr(kind === "running" ? "lane.state.running" : "lane.state.waiting")),
    name: h("code", null, e.cmd),
    sub: e.cwd || undefined,
    title: e.cmd,
    meta: [
      h("span", null, "#" + e.id),
      h("span", null, e.by),
      h("span", { data: { laneat: String(at), lanekind: kind } }, ageText(kind, at, now)),
      e.pid != null ? h("code", null, tr("lane.pid", { pid: String(e.pid) })) : null,
      e.resumed ? tag(tr("lane.resumed"), { title: tr("lane.resumedHint") }) : null,
    ],
    primary: btn(busy ? tr("lane.releasing") : tr("lane.release"), {
      kind: "ghost", disabled: busy, title: tr("lane.releaseHint"), data: { lanerelease: String(e.id) },
    }),
    data: { lanerow: String(e.id) },
  });
}

function outcomeTag(e              )              {
  if (e.outcome === "exit") {
    if (e.exit === 0) return tag(tr("lane.outcome.exit", { code: "0" }), { mono: true });
    if (e.exit == null) return tag(tr("lane.outcome.killed"), { tone: "bad" });
    return tag(tr("lane.outcome.exit", { code: String(e.exit) }), { tone: "bad", mono: true });
  }
  if (e.outcome === "released") return tag(tr("lane.outcome.released"), { tone: "warn", title: tr("lane.outcome.releasedHint") });
  return tag(tr("lane.outcome.gone"), { tone: "warn", title: tr("lane.outcome.goneHint") });
}

function recentRow(e              , now        )              {
  const took = e.startedAt != null && e.endedAt != null ? durShort(Math.max(0, e.endedAt - e.startedAt) / 1000) : null;
  return row({
    name: h("code", null, e.cmd),
    title: e.cwd ? e.cmd + "\n" + e.cwd : e.cmd,
    meta: [
      h("span", null, "#" + e.id),
      h("span", null, e.by),
      took ? h("span", null, tr("lane.took", { d: took })) : null,
      e.endedAt != null ? h("span", { data: { laneended: String(e.endedAt) }, title: new Date(e.endedAt).toLocaleString() }, relTime(e.endedAt, now)) : null,
    ],
    cols: [outcomeTag(e)],
    data: { lanedone: String(e.id) },
  });
}

function paint()       {
  const l = lane;
  if (!l) return;
  const now = Date.now();
  const running = l.running.length
    ? card(...l.running.map((e) => liveRow(e, "running", now)))
    : note(tr("lane.idle"));
  fill($("pane"), paneBody({},
    paneHead({ desc: tr("lane.desc"), sub: h("span", { id: "laneHead" }, headSub(l)) }),
    section({ cap: tr("lane.runningCap") }, running),
    l.waiting.length ? section({ cap: tr("lane.waitingCap"), note: tr("lane.waitingNote") }, card(...l.waiting.map((e) => liveRow(e, "waiting", now)))) : null,
    section({ cap: tr("lane.recentCap") }, l.recent.length ? card(...l.recent.map((e) => recentRow(e, now))) : note(tr("lane.recentNone"))),
    note(h("span", null, tr("lane.howTo"), " ", h("code", null, "swiss lane run -- cargo test -j 2")))));
  $("countChip").textContent = countText();
}

async function load(s         )                {
  if (!current(s)) return;
  try {
    const response = await api("/api/lane", { signal: s.controller.signal });
    const body          = await response.json().catch(() => ({}));
    if (!current(s)) return;
    if (!response.ok) { toast(servedError(body, tr("util.httpN", { n: response.status })), true); return; }
    lane = body                   ;
    paint();
  } catch {
    if (current(s)) toast(tr("util.requestFailedGatewayRunning"), true);
  }
}

async function release(s         , id        )                {
  if (releasing.has(id)) return;
  if (!confirm(tr("lane.confirmRelease", { id: String(id) }))) return;
  releasing.add(id);
  paint();
  try {
    const response = await api("/api/lane/" + id + "/release", { method: "POST", body: "{}", signal: s.controller.signal });
    const body          = await response.json().catch(() => ({}));
    if (!current(s)) return;
    // A 404 is lane.unknown: the command left on its own meanwhile, which is what Release wanted.
    if (!response.ok) toast(servedError(body, tr("util.httpN", { n: response.status })), response.status !== 404);
  } catch {
    if (current(s)) toast(tr("util.requestFailedGatewayRunning"), true);
  } finally {
    releasing.delete(id);
  }
  // The notice re-reads too; this read covers a bus that is down.
  await load(s);
}

export async function mount(options                           = {})                {
  unmount();
  if (options.signal?.aborted) return;
  const s          = { controller: new AbortController(), stopBus: () => {}, read: null, detach: () => {} };
  active = s;
  const abort = ()       => { if (active === s) unmount(); };
  options.signal?.addEventListener("abort", abort, { once: true });
  s.detach = () => { options.signal?.removeEventListener("abort", abort); };
  const read = coalescedRead(() => load(s));
  s.read = read;
  s.stopBus = busOn({ module: Module.LANE, sub: LaneSub.QUEUE, key: "" }, LaneQueueId.LANE_QUEUE_CHANGED, NoticeSchema, () => { read.push(); });
  $("pane").onclick = (event            )       => {
    const t = targetEl(event);
    if (!t || !current(s)) return;
    const button = t.closest                   ("[data-lanerelease]");
    if (button && !button.disabled) void release(s, Number(button.dataset.lanerelease));
  };
  await load(s);
}

export async function refresh()                {
  if (active) await load(active);
}

/** The ages, reworded from the timestamps already held. No request (SPEC §lane.panel). */
export function clock()       {
  const now = Date.now();
  for (const node of Array.from(document.querySelectorAll             ("#pane [data-laneat]"))) {
    const text = ageText(node.dataset.lanekind || "", Number(node.dataset.laneat), now);
    if (node.textContent !== text) node.textContent = text;
  }
  for (const node of Array.from(document.querySelectorAll             ("#pane [data-laneended]"))) {
    const text = relTime(Number(node.dataset.laneended), now);
    if (node.textContent !== text) node.textContent = text;
  }
}

export function countText()         {
  if (!lane) return "";
  return tr("lane.count", { running: String(lane.running.length), waiting: String(lane.waiting.length) });
}

export function unmount()       {
  const s = active;
  active = null;
  if (s) {
    s.controller.abort();
    s.detach();
    s.stopBus();
    s.read?.stop();
  }
  $("pane").onclick = null;
  lane = null;
  releasing.clear();
}
