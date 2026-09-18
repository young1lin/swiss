/*
 * Copyright 2026 The swiss authors
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

import type { ApiJobRow, ApiJobRunRecord } from "../src/types/api.js";
import { describe, it, expect, beforeAll } from "vitest";
import {
  cloneJson,
  defTemplate,
  envToLines,
  formToV2,
  historyMeta,
  parseEnvLines,
  triggerSummary,
  v2ToForm,
} from "../src/jobs-v2.js";
/* Visual refresh V5 (docs/18): one primary action per row, the rest behind an ellipsis that
   opens menu.js's popupMenu. No text Edit/History/Delete buttons inline, no red Delete
   repeated down the list — danger lives in the menu, where it is red on exactly one item.
   The builders live in polling.js, whose import graph (add-sheet.js) assigns to the DOM at
   module top level — a permissive element stub satisfies that, and the builders themselves
   are pure string functions. */
/* Shared by the V5 and V6 describes below: the builders and the state they read, imported
   once under the permissive DOM stub (polling.js's import graph touches the DOM at module
   top level — see the V5 note). */
let jobRowHtml: (j: Record<string, unknown>) => string;
let ruleRowHtml: (r: Record<string, unknown>) => string;
let connRowHtml: (c: Record<string, unknown>) => string;
let state: { jobs: { busy: Record<string, boolean> }; tun: { busy: Record<string, string> } };

beforeAll(async () => {
  const anyG = globalThis as unknown as Record<string, unknown>;
  if (!anyG.document) {
    const elem = () => ({
      style: {}, dataset: {}, innerHTML: "", textContent: "",
      classList: { add() {}, remove() {} },
      setAttribute() {}, appendChild() {}, addEventListener() {},
    });
    anyG.document = {
      getElementById: () => elem(), createElement: () => elem(),
      querySelector: () => null, querySelectorAll: () => [],
      addEventListener() {}, documentElement: elem(), body: elem(),
    };
  }
  const polling = await import("../src/polling.js") as unknown as {
    jobRowHtml: (j: Record<string, unknown>) => string;
    ruleRowHtml: (r: Record<string, unknown>) => string;
    connRowHtml: (c: Record<string, unknown>) => string;
  };
  ({ jobRowHtml, ruleRowHtml, connRowHtml } = polling);
  state = (await import("../src/util.js")).state;
});

describe("visual refresh V5 — one primary action per row", () => {

  it("the job row keeps Run now plus one ellipsis; Edit/History/Delete move to the menu", () => {
    const html = jobRowHtml({ name: "nightly", command: "cargo test", enabled: true, trigger: { kind: "cron", expression: "0 4 * * *" } });
    expect((html.match(/<button/g) || []).length).toBe(2);
    expect(html).toContain("data-run");
    expect(html).toContain("data-more");
    expect(html).toContain("#i-ellipsis");
    expect(html).not.toContain(">Delete<");
    expect(html).not.toContain(">Edit<");
    expect(html).not.toContain(">History<");
  });

  it("the rule row keeps Start/Stop plus the ellipsis; Force free and Delete are menu items", () => {
    const html = ruleRowHtml({ id: "r1", name: "pg", localPort: 18989, targetHost: "127.0.0.1", targetPort: 5432, connectionName: "bastion", state: "down", portOwner: { pid: 7, name: "swiss" } });
    expect((html.match(/<button/g) || []).length).toBe(2);
    expect(html).toContain('data-act="start"');
    expect(html).not.toContain("Force free");
    expect(html).not.toContain(">Delete<");
    expect(html).not.toContain(">Edit<");
  });

  it("the connection row keeps Test plus the ellipsis", () => {
    const html = connRowHtml({ id: "c1", name: "bastion", host: "10.0.0.4", port: 22, username: "jdoe", authType: "key", state: "down" });
    expect((html.match(/<button/g) || []).length).toBe(2);
    expect(html).toContain("data-test");
    expect(html).not.toContain(">Delete<");
    expect(html).not.toContain(">Edit<");
  });
});

/* Visual refresh V6 follow-up (docs/18): every status dot carries a title. A colour — and
   the idle hollow ring above all — names no behaviour of its own, and the row already shows
   the state words, so the title reuses them via ONE builder (util.js dotTitle): the wording
   can never disagree between first paint and the 6s patch if both call the same function. */
describe("visual refresh V6 — the status dot carries a title", () => {
  it("the job dot: idle explains itself, up says up", () => {
    const off = jobRowHtml({ name: "off", command: "cargo test", enabled: false });
    expect(off).toContain('<span class="dot idle" data-dot title="idle — starts on first request"></span>');
    const ok = jobRowHtml({ name: "nightly", command: "cargo test", enabled: true, lastRunAt: "2026-09-12T00:00:00Z", lastOk: true });
    expect(ok).toContain('<span class="dot up" data-dot title="up"></span>');
  });

  it("the rule dot: error names its reason, a busy row says starting", () => {
    const bad = ruleRowHtml({ id: "r1", name: "pg", localPort: 18989, targetHost: "127.0.0.1", targetPort: 5432, connectionName: "s", state: "error", reason: "SSH refused" });
    expect(bad).toContain('<span class="dot error" data-dot title="error: SSH refused"></span>');
    state.tun.busy = { r2: "start" };
    const busy = ruleRowHtml({ id: "r2", name: "pg", localPort: 18990, targetHost: "127.0.0.1", targetPort: 5432, connectionName: "s", state: "down" });
    expect(busy).toContain('<span class="dot starting" data-dot title="starting"></span>');
    state.tun.busy = {};
  });

  it("the connection dot: connected reads as up", () => {
    const html = connRowHtml({ id: "c1", name: "bastion", host: "10.0.0.4", port: 22, username: "jdoe", authType: "key", state: "connected" });
    expect(html).toContain('<span class="dot up" data-dot title="up"></span>');
  });

  it("the serves dots: a known MCP's dot says its state, an unknown one defers to the row", () => {
    const html = ruleRowHtml({
      id: "r1", name: "pg", localPort: 18989, targetHost: "127.0.0.1", targetPort: 5432, connectionName: "s", state: "up",
      mcpRows: [{ name: "mysql", state: "up", known: true }, { name: "ghost", state: "", known: false }],
    });
    expect(html).toContain('mysql<span class="dot up" title="up"></span>');
    // No title on the unknown dot: the .serves span around it already answers the hover
    // ("no MCP named ghost"), and an empty title attribute would suppress that.
    expect(html).toContain('ghost<span class="dot "></span>');
  });
});

// The pure half of the S6 jobs panel (docs/11 §7): everything the config editor promises
// about round-trips is testable right here, without a browser — the guarantees the API
// cannot give (a form edit must not drop fields the form does not know) live in this module.

describe("triggerSummary", () => {
  it("reads the v2 trigger object", () => {
    expect(triggerSummary({ trigger: { kind: "cron", expression: "30 3 * * *" } } as ApiJobRow)).toBe("cron 30 3 * * *");
    expect(triggerSummary({ trigger: { kind: "interval", everyMs: 3600000, firstRun: "aligned" } } as ApiJobRow)).toBe("every 3600 s");
    expect(triggerSummary({ trigger: { kind: "interval", everyMs: 90000, firstRun: "immediate" } } as ApiJobRow)).toBe("every 90 s · immediate");
    expect(triggerSummary({ trigger: { kind: "manual" } } as ApiJobRow)).toBe("manual");
  });

  it("falls back to the v1 flat fields, so the row survives an older gateway", () => {
    expect(triggerSummary({ cron: "30 3 * * *" } as ApiJobRow)).toBe("cron 30 3 * * *");
    expect(triggerSummary({ everySec: 60 } as ApiJobRow)).toBe("every 60 s");
    expect(triggerSummary({} as ApiJobRow)).toBe("no schedule");
  });
});

describe("historyMeta", () => {
  it("describes a plain ran record", () => {
    expect(historyMeta({ trigger: "manual", ms: 42, exitCode: 0 } as ApiJobRunRecord)).toBe("manual · 42 ms · exit 0");
  });

  it("carries the retry position of an attempt", () => {
    expect(historyMeta({ trigger: "timer", attempt: 2, attempts: 3, ms: 10, exitCode: 3 } as ApiJobRunRecord))
      .toBe("timer · attempt 2/3 · 10 ms · exit 3");
  });

  it("names non-run outcomes for what they are, with no exit or timing masquerade", () => {
    expect(historyMeta({ trigger: "timer", outcome: "skipped", reason: "overlap" } as ApiJobRunRecord)).toBe("timer · skipped (overlap)");
    expect(historyMeta({ trigger: "timer", outcome: "missed", missedCount: 71 } as ApiJobRunRecord)).toBe("timer · missed · 71 more missed");
    expect(historyMeta({ trigger: "timer", outcome: "refused", error: "no such action" } as ApiJobRunRecord))
      .toBe("timer · refused · no such action");
  });

  it("marks timeout and cancellation", () => {
    expect(historyMeta({ trigger: "timer", ms: 1500, timedOut: true } as ApiJobRunRecord)).toBe("timer · 1500 ms · timed out");
    expect(historyMeta({ trigger: "manual", ms: 10, canceled: true } as ApiJobRunRecord)).toBe("manual · 10 ms · canceled");
  });
});

describe("the form <-> definition round trip", () => {
  it("keeps keys the form does not know (docs/10 §5: losing one deletes configuration)", () => {
    const base = {
      title: "nightly",
      trigger: { kind: "cron", expression: "30 3 * * *", timezone: "local" },
      action: { type: "process.legacy-command", input: { command: "cmd /c x" } },
      // A field from a FUTURE gateway this form has never heard of:
      fanout: { regions: ["eu", "us"] },
    };
    const form = v2ToForm(base);
    form.title = "nightly vacuum";
    const out = formToV2(form, base, { command: "cmd /c x" });
    expect(out.fanout).toEqual({ regions: ["eu", "us"] });
    expect(out.title).toBe("nightly vacuum");
  });

  it("is identity (modulo defaults spelled out) for a definition the form leaves alone", () => {
    const base = defTemplate("probe");
    const out = formToV2(v2ToForm(base), base, { command: "" });
    // The form owns overlap/misfire and writes their defaults explicitly - explicit
    // defaults parse identically and keep the JSON editor honest about what will save.
    const strip = (d: Record<string, unknown>) => ({ ...d, overlap: undefined, misfire: undefined });
    expect(strip(out)).toEqual(strip(base));
    expect(out.overlap).toBe("skip");
    expect(out.misfire).toBe("skip");
  });

  it("spells defaults by omission, not by zeroing them", () => {
    const base = defTemplate("clean");
    const form = v2ToForm(base);
    form.retryMax = ""; // one attempt is the default: absent, not maxAttempts: 0
    const out = formToV2(form, base, { command: "" });
    expect(out.retry).toBeUndefined();
    expect(out.output).toBeUndefined();
  });

  it("writes the policy fields it owns", () => {
    const base = defTemplate("policied");
    const form = v2ToForm(base);
    form.overlap = "queue-one";
    form.misfire = "run-once";
    form.retryMax = "3";
    form.retryDelayMs = "500";
    form.retryBackoff = "exponential";
    form.retryOn = ["failure", "timeout"];
    form.capture = "none";
    form.labels = "ops, nightly";
    const out = formToV2(form, base, { command: "" });
    expect(out.overlap).toBe("queue-one");
    expect(out.misfire).toBe("run-once");
    expect(out.retry).toEqual({ maxAttempts: 3, delayMs: 500, backoff: "exponential", retryOn: ["failure", "timeout"] });
    expect(out.output).toEqual({ capture: "none" });
    expect(out.labels).toEqual(["ops", "nightly"]);
  });

  it("never mutates the base object it reads from", () => {
    const base = defTemplate("frozen");
    const frozen = cloneJson(base);
    formToV2(v2ToForm(base), base, { command: "" });
    expect(base).toEqual(frozen);
  });
});

describe("the environment-variables box", () => {
  it("renders an env object as KEY=value lines, blank for nothing", () => {
    expect(envToLines(null)).toBe("");
    expect(envToLines({})).toBe("");
    expect(envToLines({ DEPLOY_ENV: "staging", N: "7" })).toBe("DEPLOY_ENV=staging\nN=7");
  });

  it("parses KEY=value lines, keeping = inside the value", () => {
    expect(parseEnvLines("A=1\n\nB=x=y\r\nC=")).toEqual({
      env: { A: "1", B: "x=y", C: "" },
      error: "",
    });
  });

  it("refuses a line without =, with a message naming the line", () => {
    const r = parseEnvLines("A=1\nWHAT");
    expect(r.env).toBeNull();
    expect(r.error).toContain("line 2");
    expect(r.error).toContain("WHAT");
  });

  it("refuses an empty key (=value)", () => {
    const r = parseEnvLines("=x");
    expect(r.env).toBeNull();
    expect(r.error).toContain("line 1");
  });
});

