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

import { tr } from "./i18n.js";
import { toast } from "./util.js";
import type { DbSqlTab } from "./types/state.js";
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
export interface DbClosedTab {
  /** The console tab's id - the same one the tab had open. */
  id: string;
  conn: string;
  text: string;
  /** The reader's (or the agent's) name for the tab, when it had one. */
  custom?: string;
  /** The agent's console (SPEC §data.agent Tabs), and the name it opened it under. */
  agent?: boolean;
  agentTab?: string;
  /** When it was closed (Date.now()). */
  at: number;
  createdAt?: number | null;
  updatedAt?: number | null;
  /** The server record's revision when this entry came from the store (SPEC §data.agent Tabs):
   *  a restore opened from the cache writes against it, and a stale one is a visible conflict. */
  revision?: number;
}

const CLOSED_KEY = "swiss.dbClosedTabs";
/** How many closed consoles the list keeps, every connection's together. */
const CLOSED_MAX = 200;
/** The list's stored size it keeps under, in characters, to protect other preferences. */
const CLOSED_CHARS = 1_000_000;
// Failed legacy migration keeps identities stable for this session until storage is writable.
const legacyIds = new Map<string, string>();
let migrationWarned = false;

/* Once the gateway answers its console store, the closed list of that connection is the
 * SERVER's (SPEC §data.agent Tabs): this cache is what the synchronous renderers read, filled
 * by the page's load and kept in step by close/restore/clear. localStorage stays the fallback
 * for a gateway without the store, and the migration source the first load imports once. */
const remote = new Map<string, DbClosedTab[]>();
const remoteOn = new Set<string>();

/** Does this connection read its closed consoles from the server store? */
export function dbClosedRemote(conn: string | null): boolean {
  return !!conn && remoteOn.has(conn);
}

/** Replace the connection's cached list with the server's records (newest first by update). */
export function dbClosedAdopt(conn: string, list: DbClosedTab[]): void {
  remoteOn.add(conn);
  remote.set(conn, sortClosed(list));
}

/** The connection is back on its local list (the store refused, or another one was selected). */
export function dbClosedLocalOnly(conn: string): void {
  remoteOn.delete(conn);
  remote.delete(conn);
}

export function dbClosedAdd(conn: string, entry: DbClosedTab): void {
  const list = (remote.get(conn) || []).filter((e) => e.id !== entry.id);
  remote.set(conn, sortClosed([entry, ...list]));
}

export function dbClosedDrop(conn: string, id: string): void {
  const list = remote.get(conn);
  if (list) remote.set(conn, list.filter((e) => e.id !== id));
}

export function dbClosedEmpty(conn: string): void {
  remote.set(conn, []);
}

/** The local entries not yet migrated to the server, oldest first. */
export function dbClosedLocalFor(conn: string): DbClosedTab[] {
  return readClosed().filter((e) => e.conn === conn);
}

/** Drop migrated entries from localStorage by id. False when the browser refused the write,
 *  which keeps them for the next visit instead of pretending the migration is complete. */
export function dbClosedLocalForget(conn: string, ids: string[]): boolean {
  if (!ids.length) return true;
  const gone = new Set(ids);
  return writeClosed(readClosed().filter((e) => !(e.conn === conn && gone.has(e.id))));
}

/** An entry as it may sit in storage: one written before ids existed has none yet. */
type StoredClosed = Omit<DbClosedTab, "id"> & { id?: string };

function closedEntry(v: unknown): v is StoredClosed {
  if (!v || typeof v !== "object") return false;
  const e = v as Record<string, unknown>;
  return typeof e.conn === "string" && typeof e.text === "string" && typeof e.at === "number"
    && (e.id === undefined || typeof e.id === "string");
}

function readClosed(): DbClosedTab[] {
  let raw: unknown;
  try { raw = JSON.parse(localStorage.getItem(CLOSED_KEY) || "[]"); } catch { return []; }
  if (!Array.isArray(raw)) return [];
  let repaired = false;
  const list: DbClosedTab[] = raw.filter(closedEntry).map((e: StoredClosed, index: number): DbClosedTab => {
    if (e.id) return e as DbClosedTab;
    // An entry filed before ids: minted here and written back, so the id a tool read once
    // still addresses it on the next visit.
    repaired = true;
    const key = JSON.stringify([index, e.conn, e.at, e.custom, e.agentTab]);
    let id = legacyIds.get(key);
    if (!id) { id = dbNewTabId(); if (legacyIds.size >= CLOSED_MAX) legacyIds.delete(legacyIds.keys().next().value || ""); legacyIds.set(key, id); }
    return { ...e, id };
  });
  if (repaired && !writeClosed(list) && !migrationWarned) { migrationWarned = true; toast(tr("dataTabs.migrationFailed"), true); }
  return list;
}

function writeClosed(list: DbClosedTab[]): boolean {
  const encoded = JSON.stringify(list);
  if (list.length > CLOSED_MAX || encoded.length > CLOSED_CHARS) return false;
  try {
    if (list.length) localStorage.setItem(CLOSED_KEY, encoded);
    else localStorage.removeItem(CLOSED_KEY);
    return true;
  } catch { return false; }
}

/** File the consoles about to close, each under its connection, including blank consoles whose
 *  IDs can already have been copied. Refiling the same ID updates its entry; equal text in different
 *  consoles remains separate work. A failed save refuses the close. */
export function dbClosedRemember(gone: { conn: string; tab: DbSqlTab }[]): boolean {
  const fresh: DbClosedTab[] = [];
  const now = Date.now();
  for (const { conn, tab } of gone) {
    if (!conn || tab.analysisId) continue;
    const e: DbClosedTab = { id: tab.id, conn, text: tab.sqlText, at: now,
      createdAt: tab.createdAt ?? null, updatedAt: tab.updatedAt ?? null };
    if (tab.custom) e.custom = tab.custom;
    if (tab.agent) { e.agent = true; if (tab.agentTab) e.agentTab = tab.agentTab; }
    fresh.push(e);
  }
  if (!fresh.length) return true;
  const same = (a: DbClosedTab, b: DbClosedTab): boolean => a.conn === b.conn && a.id === b.id;
  const before = readClosed().filter((e) => !fresh.some((f) => same(e, f)));
  return writeClosed(fresh.concat(before));
}

/** Newest meaningful update first, then close time, then the stable id: the order the history
 *  menu and the agent's restore-by-name read. Pure. */
function sortClosed(list: DbClosedTab[]): DbClosedTab[] {
  const time = (n: unknown): number => typeof n === "number" && Number.isFinite(n) && n > 0 && n <= 8.64e15 ? n : 0;
  return list.slice().sort((a, b) => time(b.updatedAt) - time(a.updatedAt) || time(b.at) - time(a.at) || a.id.localeCompare(b.id));
}

/** The connection's closed consoles, newest first. */
export function dbClosedFor(conn: string | null): DbClosedTab[] {
  if (!conn) return [];
  if (remoteOn.has(conn)) return remote.get(conn) || [];
  return sortClosed(readClosed().filter((e) => e.conn === conn));
}

/** Take one off the list to bring it back: null when it is gone already (another page took it). */
export function dbClosedTake(id: string, conn?: string): DbClosedTab | null {
  const list = readClosed();
  const at = list.findIndex((e) => e.id === id && (conn === undefined || e.conn === conn));
  if (at < 0) return null;
  const [e] = list.splice(at, 1);
  return writeClosed(list) ? e : null;
}

/** Take the newest closed console of the agent's of that name on the connection, if any. */
export function dbClosedTakeAgent(conn: string, agentTab: string): DbClosedTab | null {
  const e = dbClosedFor(conn).find((x) => x.agent === true && (x.agentTab || "") === agentTab);
  return e ? dbClosedTake(e.id, conn) : null;
}

/** One of the connection's closed consoles by its id (the console tab's own), if any. */
export function dbClosedById(conn: string | null, id: string): DbClosedTab | null {
  return dbClosedFor(conn).find((e) => e.id === id) || null;
}

/** Rewrite a closed console's text where it sits - a statement the agent edited or deleted
 *  while the tab was closed. Its id and place in the list stay, so the tab it comes back as
 *  is still the one that was asked for. False when it is gone already. */
export function dbClosedSetText(id: string, text: string, conn?: string): boolean {
  if (conn && remoteOn.has(conn)) {
    const list = remote.get(conn) || [];
    const at = list.findIndex((e) => e.id === id);
    if (at < 0) return false;
    if (list[at].text === text) return true;
    remote.set(conn, sortClosed(list.map((e, i) => i === at ? { ...e, text, updatedAt: Date.now() } : e)));
    return true;
  }
  const list = readClosed();
  const at = list.findIndex((e) => e.id === id && (conn === undefined || e.conn === conn));
  if (at < 0) return false;
  if (list[at].text === text) return true;
  list[at].text = text;
  list[at].updatedAt = Date.now();
  return writeClosed(list);
}

/** Forget the connection's closed consoles (the menu's Clear). */
export function dbClosedClear(conn: string): boolean {
  if (remoteOn.has(conn)) { remote.set(conn, []); return true; }
  return writeClosed(readClosed().filter((e) => e.conn !== conn));
}
