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

                                                                           
import { $, apiJson, el, iconNode, toast } from "./util.js";
import { h } from "./h.js";
import { popupMenu } from "./menu.js";
import { dbConn, dbIsMounted } from "./db-state.js";
import { tr } from "./i18n.js";

/* --- activity monitor (docs/22 W3.2) -------------------------------------------------------------- */
/* A section page over the right pane: live sessions on the connection's server, one shared
   table for both dialects (pid / user / state / wait / duration / query), the panel's own
   session chipped, and Cancel / Terminate per row. It polls every 5s WHILE OPEN and stops
   the moment it closes — an unwatched monitor is a timer nobody is reading. Redis has no
   sessions page: the entry point is hidden for it. */

const DB_ACTIVITY_POLL_MS = 5000;
let dbActivityTimer                                        = null;
// The view's own close action, registered when the section opens — #pane's delegated click
// reaches it without data-view threading the callback through every dispatcher.
let dbActivityCloseFn                      = null;

/** Seconds as the compact duration the table shows — "42s", "1m 12s", "2h 05m", "3d 04h".
 *  Pure so the column's formatting can be pinned without a DOM. */
function dbActivityDuration(secs                           )         {
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
function dbActivityPane(close            )       {
  dbActivityCloseFn = close;
  const d = dbConn();
  const main = document.querySelector             (".db-main");
  if (!main) return;
  main.textContent = "";
  const conn = d.conns.find((c) => { return c.name === d.conn; });
  const head = el("div", "db-head");
  const left = el("div", "db-head-left");
  left.appendChild(el("h2", "db-title pane-title", tr("dataActivity.title")));
  left.appendChild(el("div", "db-meta",
    (conn ? conn.label : d.conn) + " · " + tr("dataActivity.refreshesWhileOpen")));
  head.appendChild(left);
  // The Close button answers through #pane's delegated click via data-actclose (docs/37 R5).
  head.appendChild(h("div", { class: "db-head-ctl" },
    h("button", { class: "btn", type: "button", data: { actclose: "" } }, tr("dataActivity.close"))));
  main.appendChild(head);
  const wrap = el("div", "db-grid-wrap");
  wrap.id = "dbActivityWrap";
  wrap.appendChild(el("div", "db-hint", tr("dataActivity.loadingSessions")));
  main.appendChild(wrap);
  void dbActivityLoad();
  dbActivityPollStart();
}

async function dbActivityLoad() {
  const d = dbConn();
  const wrap = $("dbActivityWrap");
  if (!d.conn || !wrap) return;
  const j = await apiJson                    ("/api/db/" + encodeURIComponent(d.conn ) + "/activity");
  // A view that closed mid-flight leaves the timer to self-clear and the DOM alone.
  if (!j || !$("dbActivityWrap")) return;
  d.activityRows = j.rows || [];
  dbActivityRender();
}

function dbActivityRender() {
  const d = dbConn();
  const wrap = $("dbActivityWrap");
  if (!wrap) return;
  const rows = d.activityRows || [];
  wrap.textContent = "";
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
    tr("dataActivity.colDuration"), "query", ""].forEach((h        , i        )       => {
    hr.appendChild(el("th", i === 4 ? "db-rowctl tnum" : "db-rowctl", h));
  });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  rows.forEach((r                  ) => {
    const tri = el("tr", r.own ? "db-act-own" : "");
    const pid = el("td", "db-cell tnum", String(r.pid));
    pid.title = tr("dataActivity.pidTip", { pid: r.pid });
    tri.appendChild(pid);
    const user = el("td", "", r.user == null ? "" : r.user);
    if (r.own) {
      const chip = el("span", "db-keytype", tr("dataActivity.thisPanel"));
      chip.title = tr("dataActivity.thisPanelTip");
      user.appendChild(chip);
    }
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
function dbActivityClick(t         , ev            )          {
  if (t.closest("[data-actclose]")) {
    if (dbActivityCloseFn) dbActivityCloseFn();
    return true;
  }
  const more = t.closest             ("[data-apid]");
  if (more) {
    // stopPropagation: connect.js closes any open menu on clicks that reach document, and
    // without this the very click that opens the menu also tears it down.
    ev.stopPropagation();
    const pid = Number(more.dataset.apid);
    const row = (dbConn().activityRows || []).find((r                  )          => { return r.pid === pid; });
    if (row) dbActivityMenu(more, row);
    return true;
  }
  return false;
}

/** pgadmin's pair, in the panel's menu words: Cancel interrupts the running statement and
 *  keeps the session; Terminate (red, last) closes the session itself. */
function dbActivityMenu(anchorEl             , row                  )       {
  popupMenu(anchorEl.getBoundingClientRect(), [
    { label: tr("dataActivity.cancelQuery"), fn: () => { void dbActivityKill(row, "cancel"); } },
    { label: tr("dataActivity.terminateSession"), danger: true, fn: () => { void dbActivityKill(row, "terminate"); } },
  ]);
}

async function dbActivityKill(row                  , mode        )                {
  const d = dbConn();
  const j = await apiJson                      ("/api/db/" + encodeURIComponent(d.conn ) + "/activity-kill", {
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
    if (!dbIsMounted() || !dbConn().activity || !$("dbActivityWrap")) { dbActivityPollStop(); return; }
    if (document.hidden) return;
    void dbActivityLoad();
  }, DB_ACTIVITY_POLL_MS);
}

function dbActivityPollStop() {
  if (dbActivityTimer) { clearInterval(dbActivityTimer); dbActivityTimer = null; }
}

export { dbActivityClick, dbActivityDuration, dbActivityPane, dbActivityPollStop, dbActivityRender };
