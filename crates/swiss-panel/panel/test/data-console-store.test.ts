// @vitest-environment happy-dom
/* SPEC §data.agent Tabs: the page's door to the server-owned console store. These pin the
   compare-and-swap chain - create then update, a refused save that keeps the draft, and the
   explicit rebase that lets the reader overwrite the newer record - and the delete that clears
   history only when the server answered. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { freshTab } from "../src/db-state.js";
import type { DbSqlTab } from "../src/types/state.js";
import {
  dbConsoleClearClosed, dbConsoleFlush, dbConsoleForget, dbConsolePut, dbConsoleRebase, dbConsoleRecord, dbConsoleStage,
  dbConsoleStoreList, dbConsoleStoreOff, dbConsoleStoreOn, dbConsoleSyncState,
} from "../src/data-console-store.js";
import { consolePeer, type ConsolePeer, type Reply, type ConsoleCall } from "./console-bus-fixture.js";
import type { ConsoleRecord } from "../src/data-console-sync.js";

const record = (over: Partial<ConsoleRecord> = {}): ConsoleRecord => ({
  id: "tab-1", tab: "Report", agent: true, agentTab: "Report", open: true,
  sql: "SELECT 1", revision: 1, createdAt: 10, updatedAt: 10, closedAt: null, ...over,
});

const consoleTab = (sql: string, revision = 0): DbSqlTab => {
  const tab = freshTab("sql");
  tab.id = "tab-1";
  tab.sqlText = sql;
  tab.agent = true;
  tab.agentTab = "Report";
  tab.revision = revision;
  return tab;
};

let replies: Reply[];
let bodies: ConsoleCall[];
let peer: ConsolePeer;

beforeEach(() => {
  document.body.innerHTML = '<div id="toast"></div>';
  vi.useFakeTimers();
  replies = [];
  peer = consolePeer();
  bodies = peer.calls;
  peer.answer = () => replies.shift() || { body: { consoles: [] } };
  dbConsoleStoreOff("c");
  dbConsoleForget("c", "tab-1");
  dbConsoleStoreOff("other");
  dbConsoleForget("other", "tab-1");
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("server console store", () => {
  it("enables itself only after a read answers and seeds the records it returns", async () => {
    replies.push({ body: { consoles: [record()] } });
    expect(dbConsoleStoreOn("c")).toBe(false);
    const list = await dbConsoleStoreList("c");
    expect(list).toHaveLength(1);
    expect(dbConsoleStoreOn("c")).toBe(true);
    expect(bodies[0]).toMatchObject({ method: "LIST", body: { conn: "c", after: "" } });
  });

  it("creates a new console once and updates it against the revision the server returned", async () => {
    replies.push({ body: { consoles: [] } });
    await dbConsoleStoreList("c");
    replies.push({ body: { console: record({ revision: 1, sql: "SELECT 1" }) } });
    dbConsoleStage("c", consoleTab("SELECT 1"), "Report");
    expect(await dbConsoleFlush("c")).toBe(true);
    expect(bodies.at(-1)).toMatchObject({ method: "PUT", body: { mode: "create", conn: "c" } });
    expect((bodies.at(-1)?.body?.console as ConsoleRecord).revision).toBe(0);
    replies.push({ body: { console: record({ revision: 2, sql: "SELECT 2" }) } });
    dbConsoleStage("c", consoleTab("SELECT 2", 1), "Report");
    expect(await dbConsoleFlush("c")).toBe(true);
    expect(bodies.at(-1)).toMatchObject({ body: { mode: "update" } });
    expect((bodies.at(-1)?.body?.console as ConsoleRecord).revision).toBe(1);
    expect(dbConsoleSyncState("c", "tab-1")).toBe("clean");
  });

  it("autosaves each connection on its own deadline when switching during the debounce", async () => {
    replies.push({ body: { consoles: [] } }, { body: { consoles: [] } });
    await dbConsoleStoreList("c");
    await dbConsoleStoreList("other");
    replies.push(
      { body: { console: record({ sql: "SELECT 1" }) } },
      { body: { console: record({ sql: "SELECT 2" }) } },
    );
    dbConsoleStage("c", consoleTab("SELECT 1"), "Report");
    await vi.advanceTimersByTimeAsync(250);
    dbConsoleStage("other", consoleTab("SELECT 2"), "Other report");
    await vi.advanceTimersByTimeAsync(250);
    expect(bodies.filter(b => b.method === "PUT").map(b => b.body?.conn)).toEqual(["c"]);
    expect(dbConsoleSyncState("c", "tab-1")).toBe("clean");
    expect(dbConsoleSyncState("other", "tab-1")).toBe("pending");
    await vi.advanceTimersByTimeAsync(250);
    expect(bodies.filter(b => b.method === "PUT").map(b => b.body)).toMatchObject([
      { conn: "c", console: { sql: "SELECT 1" } },
      { conn: "other", console: { sql: "SELECT 2" } },
    ]);
    expect(dbConsoleSyncState("other", "tab-1")).toBe("clean");
  });

  it("coalesces edits to the same connection into the latest draft", async () => {
    replies.push({ body: { consoles: [] } });
    await dbConsoleStoreList("c");
    dbConsoleStage("c", consoleTab("SELECT 1"), "Report");
    await vi.advanceTimersByTimeAsync(250);
    dbConsoleStage("c", consoleTab("SELECT 2"), "Report");
    await vi.advanceTimersByTimeAsync(499);
    expect(bodies.filter(b => b.method === "PUT")).toHaveLength(0);
    replies.push({ body: { console: record({ sql: "SELECT 2" }) } });
    await vi.advanceTimersByTimeAsync(1);
    expect(bodies.filter(b => b.method === "PUT").map(b => b.body)).toMatchObject([
      { conn: "c", console: { sql: "SELECT 2" } },
    ]);
    expect(dbConsoleSyncState("c", "tab-1")).toBe("clean");
  });

  it("keeps a refused draft without retrying it, until the reader rebases on the newer record", async () => {
    replies.push({ body: { consoles: [] } });
    await dbConsoleStoreList("c");
    replies.push({ fault: { code: "console.changed", message: "conflict: console changed" } });
    dbConsoleStage("c", consoleTab("unsaved", 3), "Report");
    expect(await dbConsoleFlush("c")).toBe(false);
    expect(dbConsoleSyncState("c", "tab-1")).toBe("failed");
    const attempts = bodies.length;
    expect(await dbConsoleFlush("c")).toBe(false);
    expect(bodies.length, "a failed save is not retried behind the reader").toBe(attempts);
    dbConsoleRebase("c", record({ revision: 7, sql: "server" }));
    replies.push({ body: { console: record({ revision: 8, sql: "unsaved" }) } });
    expect(await dbConsoleFlush("c")).toBe(true);
    expect((bodies.at(-1)?.body?.console as ConsoleRecord).revision).toBe(7);
    expect((bodies.at(-1)?.body?.console as ConsoleRecord).sql).toBe("unsaved");
    expect(dbConsoleSyncState("c", "tab-1")).toBe("clean");
  });

  it("sends a closed record with the mode the caller proved", async () => {
    replies.push({ body: { consoles: [record()] } });
    await dbConsoleStoreList("c");
    replies.push({ body: { console: record({ open: false, revision: 2, closedAt: 20 }) } });
    const result = await dbConsolePut("c", { ...dbConsoleRecord("c", consoleTab("SELECT 1", 1), "Report"), open: false }, "update");
    expect(result.ok).toBe(true);
    expect(bodies.at(-1)).toMatchObject({ body: { mode: "update" } });
  });

  it("clears closed history on the server and keeps it when the delete fails", async () => {
    replies.push({ body: { consoles: [] } });
    await dbConsoleStoreList("c");
    replies.push({ fault: { code: "", message: "disk" } });
    expect(await dbConsoleClearClosed("c")).toBe(false);
    expect(bodies.at(-1)).toMatchObject({ method: "CLEAR_CLOSED", body: { conn: "c" } });
    replies.push({ body: { cleared: 2 } });
    expect(await dbConsoleClearClosed("c")).toBe(true);
  });
});
