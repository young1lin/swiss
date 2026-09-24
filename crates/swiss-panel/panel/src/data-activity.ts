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

import type { ApiDbActivityReply, ApiDbActivityRow } from "./types/api.js";
import { $, apiJson, el, iconNode, toast } from "./util.js";
import { h } from "./h.js";
import { dbConn, dbIsMounted, dbTab } from "./db-state.js";
import { tr } from "./i18n.js";
import { popupMenu } from "./ui/menu.js";
import { tag } from "./ui/status.js";

/* --- activity monitor (docs/22 W3.2) -------------------------------------------------------------- */
/* One open object among the others (docs/42 T2): live sessions on the connection's server, one
   shared table for both dialects (pid / user / state / wait / duration / query), the panel's own
   session chipped, and Cancel / Terminate per row. It paints into the pane's grid slot like a
   row page does, and it polls every 5s WHILE ITS TAB IS ACTIVE — a backgrounded or closed
   monitor is a timer nobody is reading. Redis has no sessions page: the entry point is hidden
   for it. */

const DB_ACTIVITY_POLL_MS = 5000;
let dbActivityTimer: ReturnType<typeof setInterval> | null = null;

/** Seconds as the compact duration the table shows — "42s", "1m 12s", "2h 05m", "3d 04h".
 *  Pure so the column's formatting can be pinned without a DOM. */
function dbActivityDuration(secs: number | null | undefined): string {
  const s = Math.max(0, Math.floor(Number(secs) || 0));
  if (s < 60) return s + "s";
  const m = Math.floor(s / 60);
  if (m < 60) return m + "m " + (s % 60) + "s";
  const h = Math.floor(m / 60);
  if (h < 24) return h + "h " + String(m % 60).padStart(2, "0") + "m";
  return Math.floor(h / 24) + "d " + String(h % 24).padStart(2, "0") + "h";
}

/** Start the monitor: first answer now, then the poll. The tab itself is the open/close state
 *  (docs/42 T2) — there is no flag left to set and no pane to take over. */
function dbActivityStart(): void {
  void dbActivityLoad();
  dbActivityPollStart();
}

async function dbActivityLoad() {
  const d = dbConn();
  const t = dbTab();
  if (!d.conn || t.kind !== "activity") return;
  const j = await apiJson<ApiDbActivityReply>("/api/db/" + encodeURIComponent(d.conn!) + "/activity");
  // A tab switched away from (or a view unmounted) mid-flight leaves the timer to self-clear and
  // the DOM alone — the answer belongs to the tab that asked, and it is no longer in front.
  const after = dbTab();
  if (!j || after !== t) return;
  t.activityRows = j.rows || [];
  dbActivityRender();
}

function dbActivityRender() {
  const t = dbTab();
  const wrap = $("dbGridWrap");
  if (!wrap || t.kind !== "activity") return;
  const rows = t.activityRows;
  wrap.textContent = "";
  // null is "the first fetch has not answered yet", [] is "the server has no sessions". The
  // tab opens empty and paints immediately, so collapsing the two would greet every open with
  // a "no sessions" that is not yet true.
  if (!rows) {
    wrap.appendChild(el("div", "db-hint", tr("dataActivity.loadingSessions")));
    return;
  }
  if (!rows.length) {
    wrap.appendChild(el("div", "db-hint", tr("dataActivity.noSessions")));
    return;
  }
  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  // pid/query are server words and stay untranslated tokens; the four word columns go through
  // tr(). The numeric class picks by INDEX (duration is 4): the label itself is translated, so
  // comparing it against an English literal would only match in English.
  ["pid", tr("dataActivity.colUser"), tr("dataActivity.colState"), tr("dataActivity.colWait"),
    tr("dataActivity.colDuration"), "query", ""].forEach((h: string, i: number): void => {
    hr.appendChild(el("th", i === 4 ? "db-rowctl tnum" : "db-rowctl", h));
  });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  rows.forEach((r: ApiDbActivityRow) => {
    const tri = el("tr", r.own ? "db-act-own" : "");
    const pid = el("td", "db-cell tnum", String(r.pid));
    pid.title = tr("dataActivity.pidTip", { pid: r.pid });
    tri.appendChild(pid);
    const user = el("td", "", r.user == null ? "" : r.user);
    // The panel's own session is a descriptive word: the library's toneless tag (docs/46 §3.7).
    if (r.own) user.append(" ", tag(tr("dataActivity.thisPanel"), { title: tr("dataActivity.thisPanelTip") }));
    tri.appendChild(user);
    // state and wait are COALESCE'd to "" in activity_sql on both dialects - never null.
    tri.appendChild(el("td", "", r.state));
    const wait = el("td", "", r.wait);
    if (r.blockedBy) {
      const blocked = el("span", "db-act-blocked", " · " + tr("dataActivity.blockedBy", { pid: r.blockedBy }));
      blocked.title = tr("dataActivity.blockedByTip");
      wait.appendChild(blocked);
    }
    tri.appendChild(wait);
    tri.appendChild(el("td", "tnum", dbActivityDuration(r.seconds)));
    // The query truncates in the cell; the full text is one hover away (title).
    const q = el("td", "db-cell", String(r.query == null ? "" : r.query));
    q.title = q.textContent;
    tri.appendChild(q);
    // The per-row menu trigger addresses its row by pid and answers through #pane's
    // delegated click (docs/37 R5); the row is re-found from live state at event time.
    const ctl = el("td", "db-rowctl");
    ctl.appendChild(h("button", {
      class: "db-act-more", type: "button", title: tr("dataActivity.cancelTerminateSession"),
      data: { apid: String(r.pid) },
    }, iconNode("ellipsis")));
    tri.appendChild(ctl);
    tbody.appendChild(tri);
  });
  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
}

/** #pane's delegated click for the Activity page (docs/37 R5). Behavior note (docs/37 §10.1):
 *  the per-row menu re-finds its row from live d.activityRows by pid at event time — a poll
 *  that repainted the table between render and click can never kill the wrong session. */
function dbActivityClick(t: Element, ev: MouseEvent): boolean {
  const more = t.closest<HTMLElement>("[data-apid]");
  if (more) {
    // stopPropagation: connect.js closes any open menu on clicks that reach document, and
    // without this the very click that opens the menu also tears it down.
    ev.stopPropagation();
    const pid = Number(more.dataset.apid);
    const at = dbTab();
    const row = (at.kind === "activity" ? at.activityRows || [] : []).find((r: ApiDbActivityRow): boolean => { return r.pid === pid; });
    if (row) dbActivityMenu(more, row);
    return true;
  }
  return false;
}

/** pgadmin's pair, in the panel's menu words: Cancel interrupts the running statement and
 *  keeps the session; Terminate (red, last) closes the session itself. */
function dbActivityMenu(anchorEl: HTMLElement, row: ApiDbActivityRow): void {
  popupMenu(anchorEl.getBoundingClientRect(), [
    { label: tr("dataActivity.cancelQuery"), fn: () => { void dbActivityKill(row, "cancel"); } },
    { label: tr("dataActivity.terminateSession"), danger: true, fn: () => { void dbActivityKill(row, "terminate"); } },
  ]);
}

async function dbActivityKill(row: ApiDbActivityRow, mode: string): Promise<void> {
  const d = dbConn();
  const j = await apiJson<{ result?: boolean }>("/api/db/" + encodeURIComponent(d.conn!) + "/activity-kill", {
    method: "POST",
    body: JSON.stringify({ pid: row.pid, mode: mode }),
  });
  if (!j) return;
  if (j.result === false) {
    toast(tr("dataActivity.pidAlreadyGone", { pid: row.pid }), true);
  } else {
    toast(tr(mode === "cancel" ? "dataActivity.cancelledPid" : "dataActivity.terminatedPid", { pid: row.pid }));
  }
  void dbActivityLoad();
}

function dbActivityPollStart() {
  dbActivityPollStop();
  dbActivityTimer = setInterval(() => {
    // Self-guarding: the page closed or the view unmounted — stop instead of polling into a
    // DOM nobody sees. A hidden tab skips its tick but keeps the timer.
    if (!dbIsMounted() || dbTab().kind !== "activity") { dbActivityPollStop(); return; }
    if (document.hidden) return;
    void dbActivityLoad();
  }, DB_ACTIVITY_POLL_MS);
}

function dbActivityPollStop() {
  if (dbActivityTimer) { clearInterval(dbActivityTimer); dbActivityTimer = null; }
}

export { dbActivityClick, dbActivityDuration, dbActivityLoad, dbActivityPollStart, dbActivityPollStop, dbActivityRender, dbActivityStart };
