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
   Recently closed consoles (SPEC §data.tabs). A console tab holding text is work the reader
   typed, or the Data agent wrote: its × used to drop it for good, and so did a bulk close, a
   database switch and a page reload. Each of those now files the tab here first - its text,
   its name and, for one of the agent's, the name the agent knows it by - per browser in
   localStorage, newest first, bounded. The strip's overflow menu brings one back
   (data-tabs.ts), and the agent's open_console naming a tab the reader closed brings that one
   back with its text (data-sql.ts dbAgentTab). A result is not kept: its rows are a read to
   run again.
   ================================================================================================ */

/** One closed console, under the connection it was open on. */
                              
             
               
               
                                                                         
                  
                                                                                      
                  
                    
                                         
             
 

const CLOSED_KEY = "swiss.dbClosedTabs";
/** How many closed consoles the list keeps, every connection's together. */
const CLOSED_MAX = 30;
/** The list's stored size it keeps under, in characters: the oldest go past it, so one long
 *  script cannot crowd out the other preferences this origin keeps. */
const CLOSED_CHARS = 1_000_000;

function closedEntry(v         )                   {
  if (!v || typeof v !== "object") return false;
  const e = v                           ;
  return typeof e.id === "string" && typeof e.conn === "string" && typeof e.text === "string" && typeof e.at === "number";
}

function readClosed()                {
  try {
    const v          = JSON.parse(localStorage.getItem(CLOSED_KEY) || "[]");
    return Array.isArray(v) ? v.filter(closedEntry) : [];
  } catch { return []; }
}

function writeClosed(list               )       {
  const kept = list.slice(0, CLOSED_MAX);
  while (kept.length > 1 && JSON.stringify(kept).length > CLOSED_CHARS) kept.pop();
  try {
    if (kept.length) localStorage.setItem(CLOSED_KEY, JSON.stringify(kept));
    else localStorage.removeItem(CLOSED_KEY);
  } catch { /* storage refused (full, or blocked): the tabs close as they used to */ }
}

/** File the consoles about to close, each under its connection: one holding no text is
 *  nothing to bring back. The same text closed again on a connection moves to the front
 *  instead of standing twice. */
export function dbClosedRemember(gone                                   )       {
  const fresh                = [];
  const now = Date.now();
  for (const { conn, tab } of gone) {
    if (!conn || !tab.sqlText.trim()) continue;
    const e              = { id: now.toString(36) + "-" + Math.random().toString(36).slice(2, 8), conn, text: tab.sqlText, at: now };
    if (tab.custom) e.custom = tab.custom;
    if (tab.agent) { e.agent = true; if (tab.agentTab) e.agentTab = tab.agentTab; }
    fresh.push(e);
  }
  if (!fresh.length) return;
  const same = (a             , b             )          => a.conn === b.conn && a.text === b.text;
  const before = readClosed().filter((e) => !fresh.some((f) => same(e, f)));
  writeClosed(fresh.concat(before));
}

/** The connection's closed consoles, newest first. */
export function dbClosedFor(conn               )                {
  return conn ? readClosed().filter((e) => e.conn === conn) : [];
}

/** Take one off the list to bring it back: null when it is gone already (another page took it). */
export function dbClosedTake(id        )                     {
  const list = readClosed();
  const at = list.findIndex((e) => e.id === id);
  if (at < 0) return null;
  const [e] = list.splice(at, 1);
  writeClosed(list);
  return e;
}

/** Take the newest closed console of the agent's of that name on the connection, if any. */
export function dbClosedTakeAgent(conn        , agentTab        )                     {
  const e = dbClosedFor(conn).find((x) => x.agent === true && (x.agentTab || "") === agentTab);
  return e ? dbClosedTake(e.id) : null;
}

/** Forget the connection's closed consoles (the menu's Clear). */
export function dbClosedClear(conn        )       {
  writeClosed(readClosed().filter((e) => e.conn !== conn));
}
