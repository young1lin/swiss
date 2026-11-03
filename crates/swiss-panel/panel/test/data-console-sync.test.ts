import { describe, expect, it } from "vitest";
import { ConsoleSync, type ConsoleRecord } from "../src/data-console-sync.js";

const record = (sql: string, revision = 0): ConsoleRecord => ({
  id: "tab-1", tab: "Report", agent: true, agentTab: "Report", open: true,
  sql, revision, createdAt: 1, updatedAt: 1, closedAt: null,
});

describe("server console synchronization", () => {
  it("serializes edits made while a save is in flight without losing either", async () => {
    let resolve!: (saved: ConsoleRecord) => void;
    const sent: ConsoleRecord[] = [];
    const sync = new ConsoleSync(async (_conn, item) => {
      sent.push({ ...item });
      if (sent.length === 1) return new Promise<ConsoleRecord>(r => { resolve = r; });
      return { ...item, revision: item.revision + 1 };
    });
    sync.stage("a", record("first"));
    const pending = sync.flush("a");
    sync.stage("a", record("second"));
    resolve(record("first", 1));
    expect(await pending).toBe(true);
    expect(sent.map(r => [r.sql, r.revision])).toEqual([["first", 0], ["second", 1]]);
  });

  it("keeps a conflicting draft and refuses to retry it against a newer revision", async () => {
    let attempts = 0;
    const sync = new ConsoleSync(async () => { attempts++; throw new Error("conflict"); });
    sync.stage("a", record("unsaved", 3));
    expect(await sync.flush("a")).toBe(false);
    expect(sync.pending("a")[0].sql).toBe("unsaved");
    expect(await sync.flush("a")).toBe(false);
    expect(attempts).toBe(1);
  });

  it("does not replace an unsaved draft when a stale load returns", async () => {
    const sync = new ConsoleSync(async (_conn, item) => ({ ...item, revision: item.revision + 1 }));
    expect(sync.seed("a", record("saved", 2))).toBe(true);
    sync.stage("a", record("draft", 2));
    expect(sync.seed("a", record("older", 1))).toBe(false);
    expect(sync.pending("a")[0]).toMatchObject({ sql: "draft", revision: 2 });
    expect(await sync.flush("a")).toBe(true);
    expect(sync.pending("a")).toEqual([]);
    expect(sync.seed("a", record("older", 1))).toBe(false);
  });
});
