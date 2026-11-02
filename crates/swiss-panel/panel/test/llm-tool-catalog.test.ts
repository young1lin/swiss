/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */

import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { TOOL_SUMMARIES } from "../src/views/llm-core.js";
import en from "../src/locales/en.js";
import zh from "../src/locales/zh.js";

/* The catalog ratchet: toolSummary() falls back to the server's own words when the panel
 * does not know a tool - a courtesy for a server NEWER than this panel binary. A tool this
 * repo itself ships must never fall through to it: every name in swiss-llm's BUILTIN_TOOLS
 * and DB_TOOLS (the single source of truth, read straight from agent.rs) needs an entry in
 * TOOL_SUMMARIES, every entry must name a tool the server actually serves (the retired
 * `list` once lingered here), and every entry's key must resolve in both locales. The
 * i18n-complete ratchet cannot see this class: these keys reach tr() through a table, and a
 * missing entry shows as silent English, not as a missing key. */
const here = path.dirname(fileURLToPath(import.meta.url));
const agentRs = fs.readFileSync(
  path.resolve(here, "..", "..", "..", "..", "crates", "swiss-llm", "src", "agent.rs"), "utf8");

function servedNames(constName: string): string[] {
  const m = agentRs.match(new RegExp("const " + constName + "[^=]*=\\s*\\[([\\s\\S]*?)\\];"));
  if (!m) throw new Error("agent.rs no longer declares " + constName + " - update this test");
  return [...m[1].matchAll(/name: "([A-Za-z_]+)"/g)].map((x) => x[1]);
}

describe("the tool catalog is whole in both languages", () => {
  it("every builtin tool the server serves is mapped - and nothing the server retired", () => {
    const mapped = Object.keys(TOOL_SUMMARIES)
      .filter((k) => k.startsWith("builtin.")).map((k) => k.slice("builtin.".length)).sort();
    expect(mapped).toEqual(servedNames("BUILTIN_TOOLS").sort());
  });

  it("every db tool the server serves is mapped - and nothing the server retired", () => {
    const mapped = Object.keys(TOOL_SUMMARIES)
      .filter((k) => k.startsWith("db.")).map((k) => k.slice("db.".length)).sort();
    expect(mapped).toEqual(servedNames("DB_TOOLS").sort());
  });

  it("every mapped key resolves in both locales", () => {
    for (const key of Object.values(TOOL_SUMMARIES)) {
      expect(Object.prototype.hasOwnProperty.call(en, key), "en is missing " + key).toBe(true);
      expect(Object.prototype.hasOwnProperty.call(zh, key), "zh is missing " + key).toBe(true);
    }
  });
});
