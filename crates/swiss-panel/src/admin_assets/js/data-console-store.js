/* One person, two windows are two views of the same work (SPEC §bus.editing).
 * Compare-and-swap refuses concurrent edits: nothing is lost or merged silently.
 * A future merge must be three-way: base = the record at the draft's starting
 * revision, ours = the draft, theirs = the current record.
 * DATA.CONSOLES is the only transport (SPEC §bus.requests); ConsoleSync keeps
 * debounce, serialized writes and pending/failed drafts across reconnects. */
                                                 
import { toast } from "./util.js";
import { servedFault, tr } from "./i18n.js";
import { dbConn, dbTabs } from "./db-state.js";
import { ConsolePending, ConsoleSync,                                    } from "./data-console-sync.js";
                                                    
import { busRequest, BusFault } from "./bus/bus.js";
import { create } from "./bus/codec.js";
import { Module } from "./bus/pb/swiss/bus/v1/bus_pb.js";
import {
  ConsoleListSchema, ConsolePutSchema, ConsoleRecordSchema, ConsoleRemoveSchema, ConsoleRemovedSchema,
  ConsoleSavedSchema, ConsolesClearedSchema, ConsolesId, ConsolesOfSchema, ConsolesPageSchema,
  DataSub, SaveMode,                                   
} from "./bus/pb/swiss/bus/v1/data_pb.js";

/** The request's mode: a new page console is create-only, a later save requires the record
 *  to still exist, and the agent's own path auto-creates (db_consoles.rs SaveMode). */
                                         

/** A refusal keeps the server's wire `code` (`console.changed`) so a caller can act on what
 *  was refused, and `error` is already in the reader's language (servedError). */
                                                                                                                              

const route = { module: Module.DATA, sub: DataSub.CONSOLES };

export function consoleFromWire(record             )                {
  return { ...record, revision: Number(record.revision), createdAt: Number(record.createdAt),
    updatedAt: Number(record.updatedAt), agentTab: record.agentTab ?? null,
    closedAt: record.closedAt === undefined ? null : Number(record.closedAt) };
}

function refused(error         )                                                                 {
  if (error instanceof BusFault) return {
    ok: false, conflict: ["console.exists", "console.removed", "console.changed", "console.changedDelete"].includes(error.code),
    error: servedFault(error), code: error.code,
  };
  return { ok: false, conflict: false, error: tr("util.requestFailedGatewayRunning") };
}

const enabled = new Set        ();
const sync = new ConsoleSync(async (conn, record) => {
  const result = await put(conn, record, record.revision === 0 ? "create" : "update");
  if (!result.ok) {
    if (result.code === "bus.offline" || result.code === "bus.timeout") throw new ConsolePending(result.error);
    throw new Error(result.error);
  }
  return result.console;
});

async function put(conn        , record               , mode      )                     {
  try {
    const reply = await busRequest({ ...route, id: ConsolesId.CONSOLES_PUT, schema: ConsolePutSchema }, "",
      create(ConsolePutSchema, { conn, console: create(ConsoleRecordSchema, {
        ...record, revision: BigInt(record.revision), createdAt: BigInt(record.createdAt), updatedAt: BigInt(record.updatedAt),
        agentTab: record.agentTab ?? undefined, closedAt: record.closedAt == null ? undefined : BigInt(record.closedAt),
      }), mode: mode === "create" ? SaveMode.CREATE : mode === "update" ? SaveMode.UPDATE : SaveMode.AUTO }), ConsoleSavedSchema);
    if (!reply.console) return refused(new BusFault("bus.badBody", "", {}));
    return { ok: true, console: consoleFromWire(reply.console) };
  } catch (error) { return refused(error); }
}

/** A closed record as Recently closed reads it: the same id the strip would have used, its
 *  stored name (empty means the page's own label for this kind) and its lifetime. */
export function dbClosedEntry(conn        , record               )              {
  const entry              = {
    id: record.id, conn, text: record.sql, at: record.closedAt || record.updatedAt || 0,
    createdAt: record.createdAt || null, updatedAt: record.updatedAt || null, revision: record.revision,
  };
  if (record.tab) entry.custom = record.tab;
  if (record.agent) { entry.agent = true; if (record.agentTab) entry.agentTab = record.agentTab; }
  return entry;
}

/** Is this connection's page backed by the server store? False until a successful read. */
export function dbConsoleStoreOn(conn               )          {
  return !!conn && enabled.has(conn);
}

export function dbConsoleStoreOff(conn        )       {
  enabled.delete(conn);
}

/** The connection's records, read once per visit; null when the gateway cannot answer, which
 *  keeps the page's local snapshot in charge. Reads create no files on the server. */
const reading = new Map                                         ();
export function dbConsoleStoreList(conn        , force = false)                                  {
  const running = reading.get(conn);
  if (running && !force) return running;
  const read = (async ()                                  => {
    try {
      const records                  = [];
      let after = "";
      do {
        const page = await busRequest({ ...route, id: ConsolesId.CONSOLES_LIST, schema: ConsolesPageSchema }, "",
          create(ConsolesPageSchema, { conn, after }), ConsoleListSchema);
        records.push(...page.consoles.map(consoleFromWire));
        after = page.next;
      } while (after);
      enabled.add(conn);
      return records;
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
export function dbConsoleRecord(conn        , tab          , title        )                {
  return {
    id: tab.id, tab: title, agent: tab.agent === true, agentTab: tab.agentTab ?? null,
    open: true, sql: tab.sqlText, revision: tab.revision || 0,
    createdAt: tab.createdAt || 0, updatedAt: tab.updatedAt || 0, closedAt: null,
  };
}

const flushTimers = new Map                                       ();
let settled                                               = null;

/** The page repaints the strip when a save settles, so a refusal is visible without another gesture. */
export function dbConsoleOnSettled(fn                                     )       {
  settled = fn;
}

/** Stage an edit (typing, a rename, an agent frame). Each connection debounces its own edits,
 *  so switching connections cannot cancel a parked draft's save. A send flushes explicitly. */
export function dbConsoleStage(conn        , tab          , title        )       {
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
function adoptSaved(conn        )       {
  if (dbConn().conn !== conn) return;
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
export async function dbConsoleFlush(conn        , quiet = false)                   {
  if (!dbConsoleStoreOn(conn)) return true;
  const ok = await sync.flush(conn);
  adoptSaved(conn);
  if (!ok && !quiet) toast(tr("dataTabs.consoleSaveFailed"), true);
  settled?.(conn, ok);
  return ok;
}

export function dbConsoleSyncState(conn               , id        )            {
  return conn ? sync.state(conn, id) : "clean";
}

/** The refused save's reason, for the console's conflict bar; null when nothing failed. */
export function dbConsoleSyncReason(conn               , id        )                {
  return conn ? sync.reason(conn, id) : null;
}

export function dbConsoleValue(conn        , id        )                       {
  return sync.value(conn, id);
}

/** The reader chose the server's text over a refused local draft. */
export function dbConsoleAdopt(conn        , server               )       {
  sync.adopt(conn, server);
}

/** The reader chose to keep the draft and overwrite the newer server record: the draft stays,
 *  the revision catches up, and the next flush sends it. */
export function dbConsoleRebase(conn        , server               )       {
  sync.rebase(conn, server.id, server);
}

export function dbConsoleForget(conn        , id        )       {
  sync.forget(conn, id);
}

/** A closed record's write (close, restore, edit while closed): direct, because it is not in
 *  the open strip the sync queue serializes. */
export async function dbConsolePut(conn        , record               , mode      )                     {
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
export async function dbConsoleRemove(conn        , id        , revision               )                                                      {
  try {
    const reply = await busRequest({ ...route, id: ConsolesId.CONSOLES_REMOVE, schema: ConsoleRemoveSchema }, "",
      create(ConsoleRemoveSchema, { conn, id, revision: BigInt(revision ?? 0) }), ConsoleRemovedSchema);
    sync.forget(conn, id);
    return { ok: true, removed: reply.removed };
  } catch (error) { return refused(error); }
}

/** Clear this connection's closed history on the server. Open consoles are work in progress
 *  and stay; false keeps the list exactly as it was. */
export async function dbConsoleClearClosed(conn        )                   {
  try {
    await busRequest({ ...route, id: ConsolesId.CONSOLES_CLEAR_CLOSED, schema: ConsolesOfSchema }, "",
      create(ConsolesOfSchema, { conn }), ConsolesClearedSchema);
    return true;
  } catch (error) {
    toast(refused(error).error, true);
    return false;
  }
}
