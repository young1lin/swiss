/*
 * Copyright 2026 The swiss authors
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

import { $, apiJson, el, icon, state, toast } from "./util.js";
import { popupMenu } from "./menu.js";

/* --- activity monitor (docs/22 W3.2) -------------------------------------------------------------- */
/* A section page over the right pane: live sessions on the connection's server, one shared
   table for both dialects (pid / user / state / wait / duration / query), the panel's own
   session chipped, and Cancel / Terminate per row. It polls every 5s WHILE OPEN and stops
   the moment it closes — an unwatched monitor is a timer nobody is reading. Redis has no
   sessions page: the entry point is hidden for it. */

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

/** Draw the section into the (fresh) right pane and start the poll. "close" is the view's own
 *  close action — passed in so this module never imports data-view back. */
function dbActivityPane(close: () => void): void {
  const d = state.db;
  const main = document.querySelector<HTMLElement>(".db-main");
  if (!main) return;
  main.innerHTML = "";
  const d_ = d!;
  const conn = d_.conns.find((c) => { return c.name === d?.conn; });
  const head = el("div", "db-head");
  const left = el("div", "db-head-left");
  left.appendChild(el("h2", "db-title pane-title", "Activity"));
  left.appendChild(el("div", "db-meta",
    (conn ? conn.label : d_.conn) + " · refreshes every 5s while this page is open"));
  head.appendChild(left);
  const ctl = el("div", "db-head-ctl");
  const btn = el("button", "btn", "Close");
  btn.type = "button";
  btn.onclick = close;
  ctl.appendChild(btn);
  head.appendChild(ctl);
  main.appendChild(head);
  const wrap = el("div", "db-grid-wrap");
  wrap.id = "dbActivityWrap";
  wrap.appendChild(el("div", "db-hint", "Loading sessions…"));
  main.appendChild(wrap);
  void dbActivityLoad();
  dbActivityPollStart();
}

async function dbActivityLoad() {
  const d = state.db;
  const wrap = $("dbActivityWrap");
  if (!d || !d.conn || !wrap) return;
  const j = await apiJson<ApiDbActivityReply>("/api/db/" + encodeURIComponent(d.conn!) + "/activity");
  // A view that closed mid-flight leaves the timer to self-clear and the DOM alone.
  if (!j || !$("dbActivityWrap")) return;
  d!.activityRows = j.rows || [];
  dbActivityRender();
}

function dbActivityRender() {
  const d = state.db;
  const wrap = $("dbActivityWrap");
  if (!wrap) return;
  const rows = d?.activityRows || [];
  wrap.innerHTML = "";
  if (!rows.length) {
    wrap.appendChild(el("div", "db-hint", "No sessions — the server reports none."));
    return;
  }
  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  ["pid", "user", "state", "wait", "duration", "query", ""].forEach((h) => {
    hr.appendChild(el("th", h === "duration" ? "db-rowctl tnum" : "db-rowctl", h));
  });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  rows.forEach((r: ApiDbActivityRow) => {
    const tr = el("tr", r.own ? "db-act-own" : "");
    const pid = el("td", "db-cell tnum", String(r.pid == null ? "" : r.pid));
    pid.title = "pid " + r.pid;
    tr.appendChild(pid);
    const user = el("td", "", String(r.user == null ? "" : r.user));
    if (r.own) {
      const chip = el("span", "db-keytype", "this panel");
      chip.title = "The session this Activity page itself polls through";
      user.appendChild(chip);
    }
    tr.appendChild(user);
    tr.appendChild(el("td", "", String(r.state == null ? "" : r.state)));
    const wait = el("td", "", String(r.wait == null ? "" : r.wait));
    if (r.blockedBy) {
      const blocked = el("span", "db-act-blocked", " · blocked by " + r.blockedBy);
      blocked.title = "pids holding locks this session waits on";
      wait.appendChild(blocked);
    }
    tr.appendChild(wait);
    tr.appendChild(el("td", "tnum", dbActivityDuration(r.seconds)));
    // The query truncates in the cell; the full text is one hover away (title).
    const q = el("td", "db-cell", String(r.query == null ? "" : r.query));
    q.title = q.textContent;
    tr.appendChild(q);
    const ctl = el("td", "db-rowctl");
    const more = el("button", "db-act-more");
    more.type = "button";
    more.title = "Cancel or terminate this session";
    more.innerHTML = icon("ellipsis");
    more.onclick = (e: MouseEvent): void => { e.stopPropagation(); dbActivityMenu(more, r); };
    ctl.appendChild(more);
    tr.appendChild(ctl);
    tbody.appendChild(tr);
  });
  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
}

/** pgadmin's pair, in the panel's menu words: Cancel interrupts the running statement and
 *  keeps the session; Terminate (red, last) closes the session itself. */
function dbActivityMenu(anchorEl: HTMLElement, row: ApiDbActivityRow): void {
  popupMenu(anchorEl.getBoundingClientRect(), [
    { label: "Cancel query", fn: () => { void dbActivityKill(row, "cancel"); } },
    { label: "Terminate session", danger: true, fn: () => { void dbActivityKill(row, "terminate"); } },
  ]);
}

async function dbActivityKill(row: ApiDbActivityRow, mode: string): Promise<void> {
  const d = state.db;
  const j = await apiJson<{ result?: boolean }>("/api/db/" + encodeURIComponent(d?.conn!) + "/activity-kill", {
    method: "POST",
    body: JSON.stringify({ pid: row.pid, mode: mode }),
  });
  if (!j) return;
  if (j.result === false) {
    toast("pid " + row.pid + " was already gone", true);
  } else {
    toast(mode === "cancel" ? "Cancelled pid " + row.pid : "Terminated pid " + row.pid);
  }
  void dbActivityLoad();
}

function dbActivityPollStart() {
  dbActivityPollStop();
  dbActivityTimer = setInterval(() => {
    // Self-guarding: the page closed, the view unmounted, or state died — stop instead of
    // polling into a DOM nobody sees. A hidden tab skips its tick but keeps the timer.
    if (!state.db || !state.db.activity || !$("dbActivityWrap")) { dbActivityPollStop(); return; }
    if (document.hidden) return;
    void dbActivityLoad();
  }, DB_ACTIVITY_POLL_MS);
}

function dbActivityPollStop() {
  if (dbActivityTimer) { clearInterval(dbActivityTimer); dbActivityTimer = null; }
}

export { dbActivityDuration, dbActivityPane, dbActivityPollStop, dbActivityRender };
