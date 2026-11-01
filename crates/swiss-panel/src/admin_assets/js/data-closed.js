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

                                                 
import { dbNewTabId } from "./db-state.js";

/* ================================================================================================
   Recently closed consoles (SPEC §data.tabs). A console tab holding text is work the reader
   typed, or the Data agent wrote: its × used to drop it for good, and so did a bulk close, a
   database switch and a page reload. Each of those now files the tab here first - its text,
   its name and, for one of the agent's, the name the agent knows it by - per browser in
   localStorage, newest first, bounded. The strip's overflow menu brings one back
   (data-tabs.ts), and the agent's open_console naming a tab the reader closed brings that one
   back with its text (data-sql.ts dbAgentTab). A result is not kept: its rows are a read to
   run again.

   The entry's `id` IS the console tab's id (db-state.ts dbNewTabId): the id is what survives a
   close, so a tool holding one still addresses the same console after the tab is restored.
   ================================================================================================ */

/** One closed console, under the connection it was open on. */
                              
                                                              
             
               
               
                                                                         
                  
                                                                                      
                  
                    
                                         
             
 

const CLOSED_KEY = "swiss.dbClosedTabs";
/** How many closed consoles the list keeps, every connection's together. */
const CLOSED_MAX = 30;
/** The list's stored size it keeps under, in characters: the oldest go past it, so one long
 *  script cannot crowd out the other preferences this origin keeps. */
const CLOSED_CHARS = 1_000_000;

/** An entry as it may sit in storage: one written before ids existed has none yet. */
                                                              

function closedEntry(v         )                    {
  if (!v || typeof v !== "object") return false;
  const e = v                           ;
  return typeof e.conn === "string" && typeof e.text === "string" && typeof e.at === "number"
    && (e.id === undefined || typeof e.id === "string");
}

function readClosed()                {
  let raw         ;
  try { raw = JSON.parse(localStorage.getItem(CLOSED_KEY) || "[]"); } catch { return []; }
  if (!Array.isArray(raw)) return [];
  let repaired = false;
  const list                = raw.filter(closedEntry).map((e              )              => {
    if (e.id) return e               ;
    // An entry filed before ids: minted here and written back, so the id a tool read once
    // still addresses it on the next visit.
    repaired = true;
    return { ...e, id: dbNewTabId() };
  });
  if (repaired) writeClosed(list);
  return list;
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
    const e              = { id: tab.id, conn, text: tab.sqlText, at: now };
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

/** One of the connection's closed consoles by its id (the console tab's own), if any. */
export function dbClosedById(conn               , id        )                     {
  return dbClosedFor(conn).find((e) => e.id === id) || null;
}

/** Rewrite a closed console's text where it sits - a statement the agent edited or deleted
 *  while the tab was closed. Its id and place in the list stay, so the tab it comes back as
 *  is still the one that was asked for. False when it is gone already. */
export function dbClosedSetText(id        , text        )          {
  const list = readClosed();
  const at = list.findIndex((e) => e.id === id);
  if (at < 0) return false;
  list[at].text = text;
  writeClosed(list);
  return true;
}

/** Forget the connection's closed consoles (the menu's Clear). */
export function dbClosedClear(conn        )       {
  writeClosed(readClosed().filter((e) => e.conn !== conn));
}
