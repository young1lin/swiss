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

/* The event list (docs/46 §2.4, U10). MCP Logs, Traffic activity, Remote runs and a job's
 * run history are the same thing - "this happened, at this time, took this long, and maybe
 * failed" - and were four hand-built lists that each repeated the date on every row and
 * painted a whole row red for one failure. One component:
 *
 *   [>] 14:02:11  mysql_query  {"sql":"select …"}  claude-code  ×3  [error]      1.2 s
 *
 *   time      its own column, HH:MM:SS - the DATE is a day heading above the run of rows
 *             ("Today", "Yesterday", "Mon, Sep 21"), not text repeated on every line
 *   title     the tool / command: mono, a value you would type
 *   arg       one line of arguments, ellipsized; the expanded body has the whole thing
 *   who       the client or target - only when the loaded items disagree about it (Logs
 *             usually has one client, Traffic several): a column that says the same word
 *             forty times is noise (rule 25)
 *   ×N        consecutive items with the same `same` signature fold into one line
 *   status    a failure is a red TAG, not a red row (rule 2: the tone is the state)
 *   ms        right-aligned; amber at or past slowMs
 *
 * No box and no row lines (U7): rows sit on the page, the hover is the row, and the one
 * expanded row becomes a card. No handlers either: the view's delegated listener answers
 * .tl-sum clicks and calls timelineToggle, handing it the body it owns. */
                                               
import { h } from "../h.js";
import { locale, tr } from "../i18n.js";
import { iconNode } from "./icon.js";
import { tag } from "./status.js";

                               
                                                                                  
             
                            
             
                
               
               
              
                                                  
                                                                              
                
                 
 

                               
                     
                                                                                           
                                                           
                  
                     
               
                                                                     
                    
 

const DAY = 24 * 60 * 60 * 1000;

function dayKey(at        )         {
  const d = new Date(at);
  return d.getFullYear() + "-" + d.getMonth() + "-" + d.getDate();
}

/** "Today", "Yesterday", or the date in the reader's locale (with the year only when it is
 *  not this one). Local days, not 24-hour windows: 00:30 today and 23:50 last night are on
 *  different days however close they are. */
export function dayLabel(at        , now        )         {
  if (dayKey(at) === dayKey(now)) return tr("ui.today");
  if (dayKey(at) === dayKey(now - DAY)) return tr("ui.yesterday");
  const d = new Date(at);
  const sameYear = d.getFullYear() === new Date(now).getFullYear();
  return d.toLocaleDateString(locale(), sameYear
    ? { weekday: "short", month: "short", day: "numeric" }
    : { year: "numeric", month: "short", day: "numeric" });
}

/** HH:MM:SS, 24-hour, in the reader's locale digits. */
export function timeLabel(at        )         {
  return new Date(at).toLocaleTimeString(locale(), { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

/** A duration: whole milliseconds under a second, one decimal of seconds under ten, whole
 *  seconds past that - the precision a reader can use at each scale. The cut-overs sit where
 *  the rounding lands, not at the round numbers: 999.6 ms is "1.0 s", never "1000 ms", and
 *  9 960 ms is "10 s", never "10.0 s". */
export function fmtMs(ms        )         {
  const whole = Math.round(ms);
  if (whole < 1000) return tr("ui.ms", { n: whole });
  return tr("ui.sec", { n: (ms / 1000).toFixed(ms < 9950 ? 1 : 0) });
}

/** Consecutive items with an equal, non-empty `same` fold into one run. Order is kept. */
export function collapseRuns(items                )                   {
  const runs                   = [];
  for (const it of items) {
    const last = runs.length ? runs[runs.length - 1] : null;
    if (last && it.same && last[0].same === it.same && dayKey(last[0].at) === dayKey(it.at)) last.push(it);
    else runs.push([it]);
  }
  return runs;
}

function itemNode(run                , o              , showWho         )              {
  const it = run[0];
  const open = !!o.open && o.open.has(it.id);
  const slow = it.ms != null && it.ms >= (o.slowMs ?? 1000);
  return h("div", { class: "tl-item" + (open ? " open" : ""), data: Object.assign({ "tl-id": it.id }, it.data) },
    h("button", { type: "button", class: "tl-sum", aria: { expanded: String(open) } },
      h("span", { class: "tl-chev" }, iconNode("chevron-right")),
      h("time", { class: "tl-time", dateTime: new Date(it.at).toISOString() }, timeLabel(it.at)),
      h("span", { class: "tl-title" }, it.title),
      h("span", { class: "tl-arg", title: it.arg || undefined }, it.arg || ""),
      showWho && it.who ? h("span", { class: "tl-who", title: it.who }, it.who) : null,
      run.length > 1 ? h("span", { class: "tl-n", title: tr("ui.runN", { n: run.length }) }, tr("ui.timesN", { n: run.length })) : null,
      it.status ? tag(it.status.text, { tone: it.status.tone }) : null,
      it.ms != null ? h("span", { class: "tl-ms" + (slow ? " slow" : "") }, fmtMs(it.ms)) : null),
    open && o.body ? h("div", { class: "tl-body" }, o.body(it, run)) : null);
}

export function timeline(items                , o               = {})              {
  const now = o.now ?? Date.now();
  const whos = new Set(items.map((i) => i.who).filter((w)              => !!w));
  const showWho = o.showWho ?? whos.size > 1;
  const kids           = [];
  let day = "";
  for (const run of collapseRuns(items)) {
    const k = dayKey(run[0].at);
    if (o.dayHeads !== false && k !== day) {
      kids.push(h("div", { class: "tl-day" }, dayLabel(run[0].at, now)));
      day = k;
    }
    kids.push(itemNode(run, o, showWho));
  }
  return h("div", { class: "tl" }, kids);
}

/** A quiet line at the top of an expanded row: what the row's own columns left out because it
 *  is the same on every row (the transport, the client, the size) or because only the open row
 *  needs it (a ×N run's span). Empty parts drop out; the rest join with a middle dot. */
export function timelineMeta(parts          )              {
  const kids           = [];
  parts.filter((p) => p != null && p !== false && p !== "").forEach((p, i) => {
    if (i) kids.push(" · ");
    kids.push(p);
  });
  return h("div", { class: "tl-meta" }, kids);
}

/** Open or close one row in place: the class, aria-expanded, and the body the view hands
 *  in (painted only on open). Pure DOM, no listener - the view's delegated click calls it.
 *  Answers the new open state; false when no row has that id. */
export function timelineToggle(root             , id        , body         )          {
  const item = Array.prototype.find.call(root.querySelectorAll             (".tl-item"),
    (n             ) => n.dataset.tlId === id)                           ;
  if (!item) return false;
  const open = !item.classList.contains("open");
  item.classList.toggle("open", open);
  const sum = item.querySelector(".tl-sum");
  if (sum) sum.setAttribute("aria-expanded", String(open));
  const old = item.querySelector(":scope > .tl-body");
  if (old) old.remove();
  if (open && body != null) item.appendChild(h("div", { class: "tl-body" }, body));
  return open;
}
