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

type Reply = { status?: number; body?: unknown };
let replies: Reply[];
let bodies: { url: string; method: string; body: Record<string, unknown> }[];

beforeEach(() => {
  document.body.innerHTML = '<div id="toast"></div>';
  vi.useFakeTimers();
  replies = [];
  bodies = [];
  vi.stubGlobal("fetch", vi.fn(async (url: unknown, init?: RequestInit) => {
    const method = init?.method || "GET";
    const parsed = init?.body ? JSON.parse(String(init.body)) as Record<string, unknown> : {};
    bodies.push({ url: String(url), method, body: parsed });
    const next = replies.shift() || { status: 200, body: { consoles: [] } };
    return { ok: (next.status || 200) < 400, status: next.status || 200, json: async () => next.body ?? {} };
  }));
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
    expect(bodies[0].url).toContain("/api/llm/db/consoles?conn=c");
  });

  it("creates a new console once and updates it against the revision the server returned", async () => {
    replies.push({ body: { consoles: [] } });
    await dbConsoleStoreList("c");
    replies.push({ body: { console: record({ revision: 1, sql: "SELECT 1" }) } });
    dbConsoleStage("c", consoleTab("SELECT 1"), "Report");
    expect(await dbConsoleFlush("c")).toBe(true);
    expect(bodies.at(-1)).toMatchObject({ method: "POST", body: { mode: "create", conn: "c" } });
    expect((bodies.at(-1)?.body.console as ConsoleRecord).revision).toBe(0);
    replies.push({ body: { console: record({ revision: 2, sql: "SELECT 2" }) } });
    dbConsoleStage("c", consoleTab("SELECT 2", 1), "Report");
    expect(await dbConsoleFlush("c")).toBe(true);
    expect(bodies.at(-1)).toMatchObject({ body: { mode: "update" } });
    expect((bodies.at(-1)?.body.console as ConsoleRecord).revision).toBe(1);
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
    expect(bodies.filter(b => b.method === "POST").map(b => b.body.conn)).toEqual(["c"]);
    expect(dbConsoleSyncState("c", "tab-1")).toBe("clean");
    expect(dbConsoleSyncState("other", "tab-1")).toBe("pending");
    await vi.advanceTimersByTimeAsync(250);
    expect(bodies.filter(b => b.method === "POST").map(b => b.body)).toMatchObject([
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
    expect(bodies.filter(b => b.method === "POST")).toHaveLength(0);
    replies.push({ body: { console: record({ sql: "SELECT 2" }) } });
    await vi.advanceTimersByTimeAsync(1);
    expect(bodies.filter(b => b.method === "POST").map(b => b.body)).toMatchObject([
      { conn: "c", console: { sql: "SELECT 2" } },
    ]);
    expect(dbConsoleSyncState("c", "tab-1")).toBe("clean");
  });

  it("keeps a refused draft without retrying it, until the reader rebases on the newer record", async () => {
    replies.push({ body: { consoles: [] } });
    await dbConsoleStoreList("c");
    replies.push({ status: 409, body: { error: "conflict: console changed" } });
    dbConsoleStage("c", consoleTab("unsaved", 3), "Report");
    expect(await dbConsoleFlush("c")).toBe(false);
    expect(dbConsoleSyncState("c", "tab-1")).toBe("failed");
    const attempts = bodies.length;
    expect(await dbConsoleFlush("c")).toBe(false);
    expect(bodies.length, "a failed save is not retried behind the reader").toBe(attempts);
    dbConsoleRebase("c", record({ revision: 7, sql: "server" }));
    replies.push({ body: { console: record({ revision: 8, sql: "unsaved" }) } });
    expect(await dbConsoleFlush("c")).toBe(true);
    expect((bodies.at(-1)?.body.console as ConsoleRecord).revision).toBe(7);
    expect((bodies.at(-1)?.body.console as ConsoleRecord).sql).toBe("unsaved");
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
    replies.push({ status: 500, body: { error: "disk" } });
    expect(await dbConsoleClearClosed("c")).toBe(false);
    expect(bodies.at(-1)).toMatchObject({ method: "DELETE" });
    expect(bodies.at(-1)?.url).toContain("closed=true");
    replies.push({ body: { cleared: 2 } });
    expect(await dbConsoleClearClosed("c")).toBe(true);
  });
});
