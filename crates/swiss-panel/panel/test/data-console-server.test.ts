// @vitest-environment happy-dom
/* SPEC §data.agent Tabs, the page half of the server-owned console: entering a connection puts
   its open consoles back on the strip and its closed ones in Recently closed, this browser's
   older drafts are imported once (create-only, so a record the server already holds always
   wins), reopening a closed console re-reads it first, and a refused compare-and-swap is
   visible on the card and resolvable from its own menu. */

import { afterAll, afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import {
  dbActiveIndex, dbConn as dbConnState, dbSetSqlText, dbTab, dbTabs, freshTab, mountDbView, unmountDbView,
} from "../src/db-state.js";
import { tr } from "../src/i18n.js";
import type { DbSqlTab } from "../src/types/state.js";
import type { ConsoleRecord } from "../src/data-console-sync.js";
import { dbConn, dbPage } from "./db-fixtures.js";

const here = dirname(fileURLToPath(import.meta.url));

/** The shell ids the renderers read. The view itself is BUILT per entry (renderDbView replaces
 *  #pane's whole subtree, the strip and the editor included), which is also how the pane's
 *  delegated listeners - the strip's right-click menu among them - get wired. */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

interface Call { method: string; url: string; body: Record<string, unknown> | null }
let calls: Call[] = [];
/** A test's own answers; anything it leaves falls through to one empty page. */
let route: (c: Call) => [number, unknown] | undefined = () => undefined;

const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
Object.assign(globalThis, {
  fetch: (url: string, opts?: RequestInit): Promise<unknown> => {
    const call: Call = {
      method: (opts && opts.method) || "GET",
      url: String(url),
      body: opts && typeof opts.body === "string" ? JSON.parse(opts.body) as Record<string, unknown> : null,
    };
    calls.push(call);
    const fallback: [number, unknown] = call.url.startsWith("/api/llm/db/conversation")
      ? [200, { ok: true, runId: null }] : [200, dbPage()];
    const [status, body] = route(call) || fallback;
    return Promise.resolve({ ok: status < 400, status, json: () => Promise.resolve(body) });
  },
});
afterAll(() => {
  if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
  else delete (globalThis as Record<string, unknown>).fetch;
});
vi.stubGlobal("confirm", () => true);

/* The shell before the import: modules in the Data view's graph wire controls at evaluation. */
document.body.innerHTML = shellSkeleton();
const tabs = await import("../src/data-tabs.js");
const closed = await import("../src/data-closed.js");
const store = await import("../src/data-console-store.js");
const view = await import("../src/data-view.js");

/** One record as the gateway answers it. */
function rec(over: Partial<ConsoleRecord> = {}): ConsoleRecord {
  return {
    id: "srv-1", tab: "SQL", agent: false, agentTab: null, open: true, sql: "", revision: 1,
    createdAt: 10, updatedAt: 10, closedAt: null, ...over,
  };
}

/** Land on this connection, with the view built the way an entry builds it. Every case uses its
 *  own connection name: the page reads a store once per connection and caches it for the
 *  session, which is the behaviour under test. */
function enter(conn: string): void {
  Object.assign(dbConnState(), { conns: [dbConn(conn, "mysql")], conn });
  view.renderDbView();
}

const onConsoles = (c: Call): boolean => c.url.startsWith("/api/llm/db/consoles");
const posted = (): ConsoleRecord[] => calls.filter((c) => c.method === "POST" && onConsoles(c))
  .map((c) => c.body?.console as ConsoleRecord);

/** Run the microtasks a chain of awaits and immediate-answer fetches needs. */
async function settle(): Promise<void> {
  for (let i = 0; i < 30; i++) await Promise.resolve();
}

/** Click one row of the open floating menu by its label. */
function menuRow(label: string): HTMLButtonElement {
  const row = Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button")).find((b) => b.textContent === label);
  if (!row) throw new Error("no menu row " + label + " in " + Array.from(document.querySelectorAll("#menu button")).map((b) => b.textContent).join(" | "));
  return row;
}

beforeEach(() => {
  vi.useFakeTimers();
  calls = [];
  route = () => undefined;
  localStorage.clear();
  document.body.innerHTML = shellSkeleton();
  unmountDbView();
  mountDbView();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("a connection whose consoles live on the server (SPEC §data.agent Tabs)", () => {
  it("the first entry reads the store of the connection it opened", async () => {
    dbConnState().conn = "";
    route = (c) => {
      if (c.url === "/api/db") return [200, { connections: [dbConn("proof-entry", "mysql")] }];
      if (onConsoles(c)) return [200, { consoles: [] }];
      return undefined;
    };
    await view.loadDbView();
    await settle();
    const hit = calls.find((c) => c.method === "GET" && c.url.startsWith("/api/llm/db/consoles"));
    expect(hit && decodeURIComponent(hit.url), "the opened connection, not the empty record load captured").toContain("conn=proof-entry");
  });

  it("puts its open consoles back on the strip and its closed ones in Recently closed", async () => {
    enter("srv1");
    const open = rec({ id: "srv-1", tab: "Report", agent: true, agentTab: "Report", sql: "select  1\n", revision: 3, createdAt: 100, updatedAt: 200 });
    const shut = rec({ id: "srv-2", tab: "Old", open: false, sql: "SELECT 2;\n", revision: 2, createdAt: 50, updatedAt: 60, closedAt: 70 });
    route = (c) => onConsoles(c) && c.method === "GET" ? [200, { consoles: [open, shut] }] : undefined;

    await tabs.dbConsolesLoad("srv1");

    const strip = dbTabs().filter((t): t is DbSqlTab => t.kind === "sql");
    expect(strip).toHaveLength(1);
    // The bytes are the reader's: a record is restored exactly as stored, never reformatted.
    expect(strip[0]).toMatchObject({
      id: "srv-1", sqlText: "select  1\n", revision: 3, createdAt: 100, updatedAt: 200,
      agent: true, agentTab: "Report", custom: "Report",
    });
    expect(dbActiveIndex(), "the newest console is in front").toBe(dbTabs().indexOf(strip[0]));
    expect(closed.dbClosedFor("srv1")).toMatchObject([{
      id: "srv-2", text: "SELECT 2;\n", revision: 2, at: 70, createdAt: 50, updatedAt: 60, custom: "Old",
    }]);
    expect(store.dbConsoleStoreOn("srv1")).toBe(true);
    expect(posted(), "reading adopts, it never writes").toEqual([]);
  });

  it("imports this browser's older drafts once, and the server's record wins over a local copy of the same id", async () => {
    enter("srv2");
    tabs.dbOpenTabForce({ kind: "sql" });
    const local = dbTab() as DbSqlTab;
    local.id = "local-1";
    local.sqlText = "SELECT 9";
    const older = freshTab("sql");
    older.id = "srv-3";
    older.sqlText = "local copy";
    older.custom = "Old";
    closed.dbClosedRemember([{ conn: "srv2", tab: older }]);
    const shut = rec({ id: "srv-3", tab: "Old", open: false, sql: "server copy", revision: 4, closedAt: 60 });
    route = (c) => {
      if (!onConsoles(c)) return undefined;
      if (c.method === "GET") return [200, { consoles: [shut] }];
      if (c.method === "POST") return [200, { console: { ...(c.body?.console as ConsoleRecord), revision: 5, createdAt: 900, updatedAt: 900 } }];
      return undefined;
    };

    await tabs.dbConsolesLoad("srv2");

    expect(posted().map((r) => r.id), "only the draft the server never saw is created").toEqual(["local-1"]);
    expect(posted()[0]).toMatchObject({ revision: 0, sql: "SELECT 9", open: true });
    expect(calls.filter((c) => c.method === "POST").every((c) => c.body?.mode === "create")).toBe(true);
    expect(local.revision).toBe(5);
    expect(closed.dbClosedFor("srv2").map((e) => [e.id, e.text])).toEqual([["srv-3", "server copy"]]);
    expect(closed.dbClosedLocalFor("srv2"), "migrated entries leave this browser").toEqual([]);
  });

  it("re-reads a closed console before reopening it, so a newer edit is what comes back", async () => {
    enter("srv3");
    const shut = rec({ id: "srv-4", tab: "Old", open: false, sql: "as closed", revision: 2, closedAt: 70 });
    route = (c) => onConsoles(c) && c.method === "GET" ? [200, { consoles: [shut] }] : undefined;
    await tabs.dbConsolesLoad("srv3");

    route = (c) => {
      if (!onConsoles(c)) return undefined;
      if (c.method === "GET") return [200, { consoles: [{ ...shut, sql: "edited elsewhere", revision: 3 }] }];
      if (c.method === "POST") return [200, { console: { ...(c.body?.console as ConsoleRecord), revision: 4 } }];
      return undefined;
    };
    await tabs.dbRestoreClosed("srv-4");

    expect(posted()[0]).toMatchObject({ id: "srv-4", open: true, closedAt: null, sql: "edited elsewhere", revision: 3 });
    expect(calls.filter((c) => c.method === "POST").every((c) => c.body?.mode === "update")).toBe(true);
    const t = dbTabs().find((x) => x.kind === "sql" && x.id === "srv-4") as DbSqlTab;
    expect(t).toMatchObject({ sqlText: "edited elsewhere", revision: 4, custom: "Old" });
    expect(closed.dbClosedById("srv3", "srv-4"), "it is no longer closed").toBeNull();
  });

  it("does not reopen an id the server deleted meanwhile", async () => {
    enter("srv4");
    const shut = rec({ id: "srv-5", open: false, sql: "gone", revision: 2, closedAt: 70 });
    route = (c) => onConsoles(c) && c.method === "GET" ? [200, { consoles: [shut] }] : undefined;
    await tabs.dbConsolesLoad("srv4");

    route = (c) => onConsoles(c) && c.method === "GET" ? [200, { consoles: [] }] : undefined;
    await tabs.dbRestoreClosed("srv-5");

    expect(dbTabs().some((t) => t.kind === "sql" && t.id === "srv-5")).toBe(false);
    expect(closed.dbClosedById("srv4", "srv-5"), "a dead id is dropped, never reopened").toBeNull();
    expect(document.getElementById("toast")?.textContent).toBe(tr("dataTabs.idMissing"));
  });
});

describe("a refused console save (SPEC §data.agent Tabs)", () => {
  /** Load one open console and type over it, so the next save compares against revision 2. */
  async function draftOver(conn: string, id: string): Promise<DbSqlTab> {
    enter(conn);
    const open = rec({ id, tab: "Report", sql: "as stored", revision: 2 });
    route = (c) => onConsoles(c) && c.method === "GET" ? [200, { consoles: [open] }] : undefined;
    await tabs.dbConsolesLoad(conn);
    const t = dbTabs().find((x) => x.kind === "sql" && x.id === id) as DbSqlTab;
    dbSetSqlText(t, "my draft");
    return t;
  }

  it("marks the card, then takes the server's copy when the reader asks for it", async () => {
    const t = await draftOver("srv5", "srv-6");
    let revision = 2;
    route = (c) => {
      if (!onConsoles(c)) return undefined;
      if (c.method === "GET") return [200, { consoles: [rec({ id: "srv-6", tab: "Report", sql: "newer", revision: ++revision })] }];
      if (c.method === "POST") return [409, { error: "conflict: console changed; reload before saving" }];
      return undefined;
    };
    expect(await store.dbConsoleFlush("srv5"), "the send path must see the refusal").toBe(false);
    expect(store.dbConsoleSyncState("srv5", "srv-6")).toBe("failed");

    const card = document.querySelector(`#dbTabStrip [data-dbtab="${dbActiveIndex()}"]`) as HTMLElement;
    expect(card.querySelector(".db-tab-dot.err")?.getAttribute("title")).toBe(tr("dataTabs.consoleConflict"));
    card.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
    menuRow(tr("dataTabs.reloadConsoleServer")).click();
    await settle();

    expect(t.sqlText).toBe("newer");
    expect(t.revision).toBe(3);
    expect(store.dbConsoleSyncState("srv5", "srv-6")).toBe("clean");
    expect(document.getElementById("toast")?.textContent).toBe(tr("dataTabs.consoleReloaded"));
  });

  it("keeps the draft when the reader chooses it, and saves it over the newer record", async () => {
    const t = await draftOver("srv6", "srv-7");
    let revision = 2;
    route = (c) => {
      if (!onConsoles(c)) return undefined;
      if (c.method === "GET") return [200, { consoles: [rec({ id: "srv-7", tab: "Report", sql: "newer", revision: ++revision })] }];
      if (c.method === "POST") {
        const sent = c.body?.console as ConsoleRecord;
        if (c.body?.mode === "update" && sent.revision === 3) return [200, { console: { ...sent, revision: 4 } }];
        return [409, { error: "conflict: console changed; reload before saving" }];
      }
      return undefined;
    };
    expect(await store.dbConsoleFlush("srv6")).toBe(false);

    tabs.renderDbTabs();
    const card = document.querySelector(`#dbTabStrip [data-dbtab="${dbActiveIndex()}"]`) as HTMLElement;
    card.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
    menuRow(tr("dataTabs.keepConsoleDraft")).click();
    await settle();

    expect(t.sqlText, "the reader's text is never replaced behind their back").toBe("my draft");
    expect(t.revision).toBe(4);
    expect(posted().at(-1)).toMatchObject({ id: "srv-7", sql: "my draft", revision: 3 });
    expect(store.dbConsoleSyncState("srv6", "srv-7")).toBe("clean");
    expect(document.getElementById("toast")?.textContent).toBe(tr("dataTabs.consoleDraftSaved"));
  });
});
