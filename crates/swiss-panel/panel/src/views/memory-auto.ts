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
   Memory - the auto-update sheet (SPEC §memory.panel), opened from the project head and from
   the Review tab: what keeps a project's memory up with its code without a click, in one place.

   The daily review (SPEC §memory.settings autoReview) hands every pending note in Review,
   including human notes, to the project's workers. Its switch, time of day and runs a day, and the
   workers, are the project's own settings, saved by one PATCH. The sheet says what waits (the
   review inbox's notes, grouped by origin), when the clock looks next and
   how the last review went, and queues one now.

   Auto sync (SPEC §memory.sync Triggers) - a commit or a finished Claude Code session nudging a
   sync run - is the plugin row's `sync` block, shared by every project: its auto switch and
   minimum interval are edited here (the CAS PUT the sync sheet uses), the rest of the block
   left as it was. The hooks that send the nudges are installed from the CLI; the sheet names
   the commands.
   ================================================================================================ */
import { toast } from "../util.js";
import type { HChild } from "../h.js";
import { fill, h } from "../h.js";
import { tr } from "../i18n.js";
import { actionBtn, btn, card, checkField, closeSheet, dot, field, formCap, hint, inlineForm, kvRow, pair, setActionBusy, sheet, showSheet, tag } from "../ui/index.js";
import type { MemProjectSettings, MemSummary, MemSyncBlock, MemSyncOverview, MemSyncRun } from "../types/memory.js";
import type { LlmAgentRow } from "../types/llm.js";
import { ageText, errMessage, req } from "./memory-core.js";
import { loadStatus, taskAgents } from "./llm-core.js";
import { STATE_KEY, STATE_TONE, agentNode, agentsPick, configReq, fromWord, phaseText, recheckNode } from "./memory-sync.js";

/** The runs a day the clock may take when the settings leave it out (SPEC §memory.settings). */
const RUNS_DEFAULT = 3;

/** How often the open sheet re-reads while a review waits or runs. */
const FOLLOW_MS = 3000;

/** Every pending inbox note, grouped by origin; the full review processes both groups. */
export function reviewCounts(items: MemSummary[]): { model: number; person: number } {
  const model = items.filter((x) => x.origin === "agent").length;
  return { model, person: items.length - model };
}

/** What the daily review's clock does next, judged as the gateway does (SPEC §memory.settings
 *  The daily review) from the project's runs, newest first: a review waits or runs; today's
 *  time is still ahead; it came with no review since, or the newest left questions for another
 *  while the day has runs left - the clock queues one within its minute; else tomorrow. The
 *  time of day is read on this clock, the gateway's on the machine the panel is served from. */
export type NextReview = { kind: "running" } | { kind: "today" | "tomorrow"; at: number } | { kind: "soon" | "more" };

export function nextReview(slot: string, runs: MemSyncRun[], cap: number, now: number): NextReview {
  const reviews = runs.filter((r) => r.trigger === "review" && !r.dryRun);
  if (reviews.some((r) => r.state === "queued" || r.state === "running")) return { kind: "running" };
  const [hour, minute] = slot.split(":").map(Number);
  const due = new Date(now);
  due.setHours(hour, minute, 0, 0);
  if (now < due.getTime()) return { kind: "today", at: due.getTime() };
  const since = (ms: number | undefined | null): boolean => ms != null && ms >= due.getTime();
  const today = reviews.filter((r) => since(r.queuedMs) || since(r.startedMs) || since(r.finishedMs));
  if (!today.length) return { kind: "soon" };
  const newest = today[0];
  const more = newest.state === "ok" && !!(newest.scope && newest.scope.recheck && newest.scope.recheck.more);
  if (more && today.length < cap) return { kind: "more" };
  const next = new Date(due.getTime());
  next.setDate(next.getDate() + 1);
  next.setHours(hour, minute, 0, 0);
  return { kind: "tomorrow", at: next.getTime() };
}

function nextText(n: NextReview, slot: string): string {
  switch (n.kind) {
    case "running": return tr("memory.auto.next.running");
    case "today": return tr("memory.auto.next.today", { time: slot });
    case "tomorrow": return tr("memory.auto.next.tomorrow", { time: slot });
    case "soon": return tr("memory.auto.next.soon");
    case "more": return tr("memory.auto.next.more");
  }
}

/** The newest review of the changed notes: its state, when, its phase while it runs, and what
 *  it came to - the sync sheet's own line for it. */
function lastNode(runs: MemSyncRun[]): HChild {
  const r = runs.find((x) => x.trigger === "review" && !x.dryRun && x.scope && x.scope.changed);
  if (!r) return tr("memory.auto.last.none");
  const line = recheckNode(r);
  if (line && r.summary) line.title = r.summary;
  return h("div", { data: { mautorun: r.id } },
    tag(tr(STATE_KEY[r.state] || STATE_KEY.failed), { tone: STATE_TONE[r.state] || undefined }),
    phaseText(r), " · ", ageText(r.finishedMs || r.startedMs || r.queuedMs),
    line, r.error ? hint(r.error, { bad: true }) : null);
}

/** The review that waits or runs now - Review now's, the clock's, or a dry one left out. */
function liveReview(runs: MemSyncRun[]): MemSyncRun | undefined {
  return runs.find((r) => r.trigger === "review" && !r.dryRun && (r.state === "queued" || r.state === "running"));
}

/** The workers a review would ask now and the layer that chose them - none said aloud. */
function workersNode(o: MemSyncOverview): HChild {
  const from = o.chosen ? [" · ", fromWord(o.chosen.agents.from)] : null;
  const ws = o.workers || (o.agent ? [o.agent] : []);
  if (!ws.length) return [dot("off", tr("memory.sync.agent.none")), " ", tr("memory.sync.agent.none"), from];
  return [...ws.flatMap((w, i) => [...(i ? [" + "] : []), agentNode(w)]), from];
}

/** The auto-update sheet of one project (never the global one: it has no checkout). `onSaved`
 *  hands the saved settings back, so the page's head and Review bar say them at once. */
export async function openAuto(projectId: string, name: string, onSaved: (s: MemProjectSettings) => void, openSync: () => void): Promise<void> {
  const enc = encodeURIComponent(projectId);
  const [got, inbox, llm] = await Promise.all([
    req<MemSyncOverview>("GET", "/sync?project=" + enc),
    req<{ items: MemSummary[] }>("GET", "/review?project=" + enc),
    loadStatus(),
  ]);
  if (!got.ok || !got.data) { toast(errMessage(got.err), true); return; }
  let over = got.data;
  const agents: LlmAgentRow[] = taskAgents(llm.data);
  const waits = inbox.ok && inbox.data ? reviewCounts(inbox.data.items) : null;

  const err = hint("", { id: "memAutoErr", bad: true, hidden: true, live: true });
  const say = (text: string): void => { err.textContent = text; err.hidden = !text; };

  // The daily review: a switch, then its time and runs a day while it is on.
  const s = over.settings || { agents: null, lead: null, embedding: null };
  const on = h("input", { id: "memAutoReviewOn", type: "checkbox", checked: !!s.autoReview });
  const at = h("input", { id: "memAutoReviewAt", type: "time", value: s.autoReview || "" });
  // Empty takes the default, shown as the placeholder.
  const runsInput = h("input", {
    id: "memAutoReviewRuns", type: "number", min: "1", max: "10", step: "1", placeholder: String(RUNS_DEFAULT),
    value: s.autoReviewRuns == null ? "" : String(s.autoReviewRuns),
  });
  const when = pair(
    field({ label: tr("memory.auto.daily.time"), control: at, hint: tr("memory.auto.daily.timeWhy") }),
    field({ label: tr("memory.auto.daily.runs"), control: runsInput, hint: tr("memory.sync.settings.autoReviewRunsWhy") }));
  const paintWhen = (): void => { when.hidden = !on.checked; };
  on.addEventListener("change", paintWhen);
  paintWhen();
  const pick = agentsPick({ id: "memAutoAgents", value: s.agents, agents, namedLabel: tr("memory.sync.pick.named") });

  const status = h("div", { id: "memAutoStatus" });
  const reviewNow = btn(tr("memory.auto.reviewNow"), { id: "memAutoReviewNow" });
  // Stop (SPEC §memory.sync Cancel) beside the button that started it: shown while a review
  // waits or runs, "stopping…" once told to.
  const stopReview = actionBtn("stop", tr("memory.auto.stopReview"), { id: "memAutoStopReview", ghost: true, title: tr("memory.auto.stopReviewWhy") });
  // The words a Stop wears when it is not waiting on anything (setActionBusy restores them).
  const stopIdle = (): { label: string; title: string } => {
    return { label: tr("memory.auto.stopReview"), title: tr("memory.auto.stopReviewWhy") };
  };
  let cancelPending = "";
  const cancelAccepted = new Set<string>();
  const paintButtons = (): void => {
    const live = liveReview(over.runs);
    const stopping = !!live && (cancelPending === live.id || cancelAccepted.has(live.id) || (over.stopping || []).includes(live.id));
    reviewNow.disabled = !!live;
    stopReview.hidden = !live;
    setActionBusy(stopReview, "stop", stopIdle(), stopping ? tr("memory.sync.run.stopping") : null);
  };
  const paintStatus = (): void => {
    const saved = over.settings && over.settings.autoReview;
    const cap = (over.settings && over.settings.autoReviewRuns) || RUNS_DEFAULT;
    // A review in flight is said whether or not the clock is on: Review now runs one either way.
    const live = liveReview(over.runs);
    const next: HChild = live ? [tr("memory.auto.next.running"), phaseText(live)]
      : saved ? nextText(nextReview(saved, over.runs, cap, Date.now()), saved) : tr("memory.auto.next.off");
    fill(status, card([
      kvRow(tr("memory.auto.workers"), workersNode(over)),
      waits ? kvRow(tr("memory.auto.waiting"), tr("memory.auto.waitingN", waits)) : null,
      kvRow(tr("memory.auto.next"), next),
      kvRow(tr("memory.auto.last"), lastNode(over.runs)),
    ]));
    paintButtons();
  };
  paintStatus();

  // Auto sync: the plugin row's block, shared by every project. No block yet is the sync
  // sheet's to set up (its conversations are a privacy opt-in), so here it is only said.
  const block: MemSyncBlock | null = over.config || null;
  const syncOn = h("input", { id: "memAutoSyncOn", type: "checkbox", checked: !!block && block.auto !== false });
  const interval = h("input", { id: "memAutoSyncInterval", placeholder: "600", value: block && block.minIntervalSec != null ? String(block.minIntervalSec) : "" });
  // A nudge never falls back to the newest agent (SPEC §memory.settings): said while auto is
  // on and neither the project nor the block names one - workers picked here name them.
  const named = !!over.chosen && (over.chosen.agents.from === "project" || over.chosen.agents.from === "plugin");
  const unnamed = hint(tr("memory.auto.sync.unnamed"), { bad: true, id: "memAutoSyncUnnamed" });
  const paintUnnamed = (): void => { unnamed.hidden = named || !syncOn.checked || pick.read().kind === "named"; };
  syncOn.addEventListener("change", paintUnnamed);
  pick.node.addEventListener("change", paintUnnamed);
  paintUnnamed();
  const syncPart: HChild = block && block.agent !== "off"
    ? [
        pair(
          checkField({ label: tr("memory.auto.sync.on"), control: syncOn, hint: tr("memory.auto.sync.onWhy") }),
          field({ label: tr("memory.sync.block.interval"), control: interval, hint: tr("memory.sync.block.intervalWhy") })),
        unnamed,
      ]
    : hint(tr("memory.auto.sync.noBlock"), { id: "memAutoSyncNone" });
  const hooksLine = hint([
    tr("memory.auto.sync.hooksGit"), " ", h("code", null, "swiss memory sync hooks install"), " · ",
    tr("memory.auto.sync.hooksClaude"), " ", h("code", null, "swiss memory hook print claude-code"),
  ], { id: "memAutoHooks" });

  const save = btn(tr("memory.auto.save"), { id: "memAutoSave", kind: "primary" });
  const close = btn(tr("memory.close"));
  const toSync = btn(tr("memory.auto.syncSettings"), { id: "memAutoSyncOpen", kind: "ghost" });

  showSheet(sheet({
    title: tr("memory.auto.title"),
    sub: name,
    body: [
      hint(tr("memory.auto.intro")),
      formCap(tr("memory.auto.daily")),
      checkField({ label: tr("memory.auto.daily.on"), control: on, hint: tr("memory.auto.daily.onWhy") }),
      when,
      field({ label: tr("memory.auto.daily.agents"), control: pick.node, hint: tr("memory.auto.daily.agentsWhy") }),
      status,
      inlineForm(reviewNow, stopReview),
      formCap(tr("memory.auto.sync")),
      syncPart,
      hooksLine,
      err,
    ],
    foot: [toSync, h("span", { class: "grow" }), close, save],
  }));

  let refreshSeq = 0;
  const refresh = async (): Promise<void> => {
    const seq = ++refreshSeq;
    const r = await req<MemSyncOverview>("GET", "/sync?project=" + enc);
    if (seq !== refreshSeq) return;
    if (r.ok && r.data) {
      over = r.data;
      cancelAccepted.forEach((id) => {
        if (!over.runs.some((run) => run.id === id && (run.state === "queued" || run.state === "running"))) cancelAccepted.delete(id);
      });
      paintStatus();
    }
  };
  // A review that waits or runs is followed while the sheet stays open.
  let following = false;
  const follow = (): void => {
    if (following || !over.runs.some((r) => r.trigger === "review" && (r.state === "queued" || r.state === "running"))) return;
    following = true;
    setTimeout(() => {
      following = false;
      if (!status.isConnected) return;
      void refresh().then(follow);
    }, FOLLOW_MS);
  };
  follow();

  close.onclick = closeSheet;
  toSync.onclick = (): void => { closeSheet(); openSync(); };

  reviewNow.onclick = (): void => {
    void (async (): Promise<void> => {
      say("");
      reviewNow.disabled = true;
      try {
        const r = await req<{ run: MemSyncRun; merged: boolean }>("POST", "/sync", { project: projectId, review: true });
        if (!r.ok || !r.data) { say(errMessage(r.err)); return; }
        toast(r.data.merged ? tr("memory.sync.review.merged") : tr("memory.sync.review.queued", { id: r.data.run.id }));
        await refresh();
        follow();
      } finally {
        paintButtons();
      }
    })();
  };
  stopReview.onclick = (): void => {
    const live = liveReview(over.runs);
    if (!live || cancelPending || cancelAccepted.has(live.id)) return;
    const hadFocus = document.activeElement === stopReview;
    void (async (): Promise<void> => {
      say("");
      // In flight: no second request, and the words say the wait. paintButtons restores the verb
      // when the answer comes back (a refused Stop is a Stop once more).
      cancelPending = live.id;
      paintButtons();
      let failed = false;
      try {
        const r = await req<{ run: MemSyncRun; stopping: boolean }>("POST",
          "/sync/runs/" + encodeURIComponent(live.id) + "/cancel?project=" + enc);
        if (!r.ok || !r.data) { failed = true; say(errMessage(r.err)); }
        else {
          cancelAccepted.add(live.id);
          toast(r.data.stopping ? tr("memory.sync.run.stoppingToast", { id: live.id }) : tr("memory.sync.run.cancelledToast", { id: live.id }));
        }
        await refresh();
      } finally {
        cancelPending = "";
        paintButtons();
        if (failed && hadFocus && liveReview(over.runs)?.id === live.id && stopReview.isConnected && !stopReview.hidden && !stopReview.disabled && document.activeElement === document.body) stopReview.focus();
        follow();
      }
    })();
  };

  /** Auto and the interval as the form says (an empty interval is the default, absent), or null
   *  when neither moved; a bad interval is the sentence. */
  type SyncPatch = { auto: boolean; minIntervalSec: number | undefined };
  const readSync = (): SyncPatch | null | string => {
    if (!block || block.agent === "off") return null;
    const raw = interval.value.trim();
    if (raw && !/^\d+$/.test(raw)) return tr("memory.sync.block.wholeNumber", { field: tr("memory.sync.block.interval") });
    const secs = raw ? Number(raw) : undefined;
    if (syncOn.checked === (block.auto !== false) && secs === (block.minIntervalSec ?? undefined)) return null;
    return { auto: syncOn.checked, minIntervalSec: secs };
  };
  /** One CAS PUT of the row with the patch laid on the block as the row holds it now - every
   *  other field of the block and the row rides back untouched. */
  const putSync = async (patch: SyncPatch): Promise<string | null> => {
    const fresh = await configReq<{ config?: Record<string, unknown> | null; revision?: number }>("GET");
    if (!fresh.data) return fresh.err;
    const row = fresh.data.config || {};
    const held = row.sync && typeof row.sync === "object" ? row.sync as MemSyncBlock : {};
    const sync: MemSyncBlock = { ...held, auto: patch.auto };
    if (patch.minIntervalSec === undefined) delete sync.minIntervalSec;
    else sync.minIntervalSec = patch.minIntervalSec;
    const put = await configReq<{ error?: string }>("PUT", { revision: fresh.data.revision, config: { ...row, sync } });
    if (!put.data) return put.err;
    return put.data.error || null;
  };

  save.onclick = (): void => {
    void (async (): Promise<void> => {
      say("");
      const slot = at.value.trim();
      if (on.checked && !/^\d{2}:\d{2}$/.test(slot)) { say(tr("memory.auto.daily.timeNeeded")); return; }
      const runs = runsInput.value.trim();
      if (on.checked && runs && !(/^\d+$/.test(runs) && Number(runs) >= 1 && Number(runs) <= 10)) {
        say(tr("memory.sync.settings.autoReviewRunsBad"));
        return;
      }
      const picked = pick.read();
      if (picked.kind === "named" && !picked.names.length) { say(tr("memory.sync.pick.pickOne")); return; }
      const syncPatch = readSync();
      if (typeof syncPatch === "string") { say(syncPatch); return; }
      // Off clears the time and hands the runs back to the default; the workers go as picked.
      const body: Record<string, unknown> = {
        autoReview: on.checked ? slot : null,
        autoReviewRuns: on.checked && runs ? Number(runs) : null,
        agents: picked.kind === "off" ? "off" : picked.kind === "inherit" ? null : picked.names.length === 1 ? picked.names[0] : picked.names,
      };
      save.disabled = true;
      try {
        const r = await req<{ settings: MemProjectSettings }>("PATCH", "/projects/" + enc + "/settings", body);
        if (!r.ok || !r.data) { say(errMessage(r.err)); return; }
        // The project's settings are saved by now: the page says them even if the block fails.
        onSaved(r.data.settings);
        if (syncPatch) {
          const failure = await putSync(syncPatch);
          if (failure) { say(failure); return; }
        }
        toast(tr("memory.auto.saved"));
        closeSheet();
      } finally {
        save.disabled = false;
      }
    })();
  };
}
