/* SPEC §data.agent Tabs: the server owns Agent consoles once this page has read its store.
 * This module is the page's only door to `/api/llm/db/consoles`: it serializes an open
 * console's edits through ConsoleSync (one compare-and-swap at a time), writes a closed
 * console's record directly, and adopts the revision the server answered with back onto the
 * tab, so the next edit compares against what is really stored. A store the gateway refuses
 * or does not have leaves the page on its local snapshot - never half on, never blocking the
 * console itself. */
import type { DbSqlTab } from "./types/state.js";
import { api, toast } from "./util.js";
import { servedError, tr } from "./i18n.js";
import { dbTabs } from "./db-state.js";
import { ConsoleSync, type ConsoleRecord, type SyncState } from "./data-console-sync.js";
import type { DbClosedTab } from "./data-closed.js";

/** The POST body's mode: a new page console is create-only, a later save requires the record
 *  to still exist, and the agent's own path auto-creates (db_consoles.rs SaveMode). */
type Mode = "auto" | "create" | "update";

/** A refusal keeps the server's wire `code` (`console.changed`) so a caller can act on what
 *  was refused, and `error` is already in the reader's language (servedError). */
export type PutResult = { ok: true; console: ConsoleRecord } | { ok: false; conflict: boolean; error: string; code?: string };

type Refusal = { error?: string; code?: string };

function refused(status: number, j: Refusal): { ok: false; conflict: boolean; error: string; code?: string } {
  const out: { ok: false; conflict: boolean; error: string; code?: string } = { ok: false, conflict: status === 409, error: servedError(j, storeError(status)) };
  if (typeof j.code === "string") out.code = j.code;
  return out;
}

const enabled = new Set<string>();
const sync = new ConsoleSync(async (conn, record) => {
  const result = await put(conn, record, record.revision === 0 ? "create" : "update");
  if (!result.ok) throw new Error(result.error);
  return result.console;
});

function storeError(status: number): string {
  return tr("dataTabs.consoleStoreHttp", { n: status });
}

async function put(conn: string, record: ConsoleRecord, mode: Mode): Promise<PutResult> {
  try {
    const r = await api("/api/llm/db/consoles", { method: "POST", body: JSON.stringify({ conn, console: record, mode }) });
    const j = await r.json().catch(() => { return {}; }) as { console?: ConsoleRecord } & Refusal;
    if (!r.ok || !j.console) return refused(r.status, j);
    return { ok: true, console: j.console };
  } catch {
    return { ok: false, conflict: false, error: tr("util.requestFailedGatewayRunning") };
  }
}

/** A closed record as Recently closed reads it: the same id the strip would have used, its
 *  stored name (empty means the page's own label for this kind) and its lifetime. */
export function dbClosedEntry(conn: string, record: ConsoleRecord): DbClosedTab {
  const entry: DbClosedTab = {
    id: record.id, conn, text: record.sql, at: record.closedAt || record.updatedAt || 0,
    createdAt: record.createdAt || null, updatedAt: record.updatedAt || null, revision: record.revision,
  };
  if (record.tab) entry.custom = record.tab;
  if (record.agent) { entry.agent = true; if (record.agentTab) entry.agentTab = record.agentTab; }
  return entry;
}

/** Is this connection's page backed by the server store? False until a successful read. */
export function dbConsoleStoreOn(conn: string | null): boolean {
  return !!conn && enabled.has(conn);
}

export function dbConsoleStoreOff(conn: string): void {
  enabled.delete(conn);
}

/** The connection's records, read once per visit; null when the gateway cannot answer, which
 *  keeps the page's local snapshot in charge. Reads create no files on the server. */
const reading = new Map<string, Promise<ConsoleRecord[] | null>>();
export function dbConsoleStoreList(conn: string, force = false): Promise<ConsoleRecord[] | null> {
  const running = reading.get(conn);
  if (running && !force) return running;
  const read = (async (): Promise<ConsoleRecord[] | null> => {
    try {
      const r = await api("/api/llm/db/consoles?conn=" + encodeURIComponent(conn));
      if (!r.ok) return null;
      const j = await r.json() as { consoles?: ConsoleRecord[] };
      if (!Array.isArray(j.consoles)) return null;
      enabled.add(conn);
      for (const record of j.consoles) sync.seed(conn, record);
      return j.consoles;
    } catch {
      return null;
    }
  })();
  reading.set(conn, read);
  void read.then(() => { if (reading.get(conn) === read) reading.delete(conn); });
  return read;
}

/** One open console as the server stores it. `title` is the card's name (dbTabTitle), which
 *  the agent also reads back through list_console_tabs. */
export function dbConsoleRecord(conn: string, tab: DbSqlTab, title: string): ConsoleRecord {
  return {
    id: tab.id, tab: title, agent: tab.agent === true, agentTab: tab.agentTab ?? null,
    open: true, sql: tab.sqlText, revision: tab.revision || 0,
    createdAt: tab.createdAt || 0, updatedAt: tab.updatedAt || 0, closedAt: null,
  };
}

const flushTimers = new Map<string, ReturnType<typeof setTimeout>>();
let settled: ((conn: string, ok: boolean) => void) | null = null;

/** The page repaints the strip when a save settles, so a refusal is visible without another gesture. */
export function dbConsoleOnSettled(fn: (conn: string, ok: boolean) => void): void {
  settled = fn;
}

/** Stage an edit (typing, a rename, an agent frame). Each connection debounces its own edits,
 *  so switching connections cannot cancel a parked draft's save. A send flushes explicitly. */
export function dbConsoleStage(conn: string, tab: DbSqlTab, title: string): void {
  if (!dbConsoleStoreOn(conn)) return;
  sync.stage(conn, dbConsoleRecord(conn, tab, title));
  const previous = flushTimers.get(conn);
  if (previous !== undefined) clearTimeout(previous);
  flushTimers.set(conn, setTimeout(() => {
    flushTimers.delete(conn);
    void dbConsoleFlush(conn, true);
  }, 500));
}

/** Adopt the revisions just saved: the tab keeps its text, and the next save compares against
 *  the record the server now holds. Only the live connection's tabs are touched. */
function adoptSaved(conn: string): void {
  for (const t of dbTabs()) {
    if (t.kind !== "sql") continue;
    const saved = sync.value(conn, t.id);
    if (!saved) continue;
    t.revision = saved.revision;
    if (saved.createdAt > 0) t.createdAt = saved.createdAt;
    if (saved.updatedAt > 0) t.updatedAt = saved.updatedAt;
  }
}

/** Send every staged edit for this connection. False means a draft was kept and not saved:
 *  `quiet` lets the send path say it in its own words, once. */
export async function dbConsoleFlush(conn: string, quiet = false): Promise<boolean> {
  if (!dbConsoleStoreOn(conn)) return true;
  const ok = await sync.flush(conn);
  adoptSaved(conn);
  if (!ok && !quiet) toast(tr("dataTabs.consoleSaveFailed"), true);
  settled?.(conn, ok);
  return ok;
}

export function dbConsoleSyncState(conn: string | null, id: string): SyncState {
  return conn ? sync.state(conn, id) : "clean";
}

/** The refused save's reason, for the console's conflict bar; null when nothing failed. */
export function dbConsoleSyncReason(conn: string | null, id: string): string | null {
  return conn ? sync.reason(conn, id) : null;
}

export function dbConsoleValue(conn: string, id: string): ConsoleRecord | null {
  return sync.value(conn, id);
}

/** The reader chose the server's text over a refused local draft. */
export function dbConsoleAdopt(conn: string, server: ConsoleRecord): void {
  sync.adopt(conn, server);
}

/** The reader chose to keep the draft and overwrite the newer server record: the draft stays,
 *  the revision catches up, and the next flush sends it. */
export function dbConsoleRebase(conn: string, server: ConsoleRecord): void {
  sync.rebase(conn, server.id, server);
}

export function dbConsoleForget(conn: string, id: string): void {
  sync.forget(conn, id);
}

/** A closed record's write (close, restore, edit while closed): direct, because it is not in
 *  the open strip the sync queue serializes. */
export async function dbConsolePut(conn: string, record: ConsoleRecord, mode: Mode): Promise<PutResult> {
  const result = await put(conn, record, mode);
  if (result.ok) {
    // A closed record's write never owns the open strip: a clean item is replaced, a draft
    // still waiting keeps its text and only catches up to the revision.
    if (sync.state(conn, record.id) === "clean") sync.adopt(conn, result.console);
    else sync.rebase(conn, result.console.id, result.console);
  }
  return result;
}

/** Forget one record the page proved it saw (the restore that found it gone). */
export async function dbConsoleRemove(conn: string, id: string, revision: number | null): Promise<PutResult | { ok: true; removed: boolean }> {
  try {
    const query = "?conn=" + encodeURIComponent(conn) + "&id=" + encodeURIComponent(id) + (revision == null ? "" : "&revision=" + revision);
    const r = await api("/api/llm/db/consoles" + query, { method: "DELETE" });
    const j = await r.json().catch(() => { return {}; }) as { removed?: boolean } & Refusal;
    if (!r.ok) return refused(r.status, j);
    sync.forget(conn, id);
    return { ok: true, removed: j.removed === true };
  } catch {
    return { ok: false, conflict: false, error: tr("util.requestFailedGatewayRunning") };
  }
}

/** Clear this connection's closed history on the server. Open consoles are work in progress
 *  and stay; false keeps the list exactly as it was. */
export async function dbConsoleClearClosed(conn: string): Promise<boolean> {
  try {
    const r = await api("/api/llm/db/consoles?conn=" + encodeURIComponent(conn) + "&closed=true", { method: "DELETE" });
    if (!r.ok) {
      const j = await r.json().catch(() => { return {}; }) as Refusal;
      toast(servedError(j, storeError(r.status)), true);
      return false;
    }
    return true;
  } catch {
    toast(tr("util.requestFailedGatewayRunning"), true);
    return false;
  }
}
