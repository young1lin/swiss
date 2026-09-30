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

import type { ApiJobRow, ApiJobRunRecord } from "../src/types/api.js";
import type { JobDef } from "../src/types/state.js";
import { describe, it, expect, beforeAll } from "vitest";
import { clearTunBusy, setTunBusy } from "../src/tunnel-state.js";
import {
  JOB_BACKOFFS,
  JOB_CAPTURES,
  JOB_FIRST_RUNS,
  JOB_MISFIRES,
  JOB_OVERLAPS,
  JOB_RETRY_ONS,
  JOB_TRIGGER_KINDS,
  cloneJson,
  defTemplate,
  envToLines,
  formToV2,
  historyMeta,
  legalOf,
  parseEnvLines,
  triggerSummary,
  v2ToForm,
} from "../src/jobs-v2.js";
/* Visual refresh V5 (SPEC §panel.design): one primary action per row, the rest behind an ellipsis that
   opens menu.js's popupMenu. No text Edit/History/Delete buttons inline, no red Delete
   repeated down the list — danger lives in the menu, where it is red on exactly one item.
   The tunnel builders live in polling.js and the job row in jobs.js (SPEC §panel.pages: its schedule
   column speaks the sheet's cron translator); both import graphs assign to the DOM at module
   top level - a permissive element stub satisfies that. Every row is BUILT (SPEC §panel.toolchain), so
   the builders answer nodes, read back through the serialising micro-DOM below. */
/* SPEC §panel.toolchain: the Node identity h()/frag() check children with, and the tree the builders
   return has to read back as markup for the grep-style assertions — a plain-object stub
   can do neither. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

class FakeNode extends NodeStub {
  tag: string;
  attrs: Record<string, string> = {};
  dataset: Record<string, string> = {};
  className = "";
  _text = "";
  get textContent(): string { return this._text; }
  set textContent(v: string) { if (v === "") { this.children = []; this._html = ""; } this._text = v; }
  id = "";
  type = "button";
  value = "";
  hidden = false;
  checked = false;
  disabled = false;
  selected = false;
  draggable = false;
  title = "";
  children: FakeNode[] = [];
  onclick: ((ev?: unknown) => void) | null = null;
  private _html = "";
  constructor(tag: string) { super(); this.tag = tag.toUpperCase(); }
  static fragment(): FakeNode { return new FakeNode("#document-fragment"); }
  get innerHTML(): string { return this._html || serialize(this); }
  set innerHTML(v: string) { this._html = v; this.children = []; }
  querySelector(): FakeNode | null { return null; }
  querySelectorAll(): FakeNode[] { return []; }
  appendChild(n: FakeNode): FakeNode {
    if (n.tag === "#DOCUMENT-FRAGMENT") { n.children.forEach((c) => { this.children.push(c); }); this._html = ""; return n; }
    this.children.push(n);
    this._html = "";
    return n;
  }
  addEventListener(): void {}
  removeEventListener(): void {}
  setAttribute(k: string, v: string): void {
    this.attrs[k] = v;
    if (k === "id") this.id = v;
    if (k.startsWith("data-")) this.dataset[k.slice(5)] = v;
  }
  removeAttribute(k: string): void { delete this.attrs[k]; }
  focus(): void {}
  getBoundingClientRect(): { top: number; left: number; right: number; bottom: number; width: number; height: number } {
    return { top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0 };
  }
  closest(sel: string): FakeNode | null { return sel.charAt(0) === "#" && this.id === sel.slice(1) ? this : null; }
}

/* One empty data- attribute stays bare, the way the string builder wrote it — the dot's
   data-dot is a flag, not a key/value pair. */
function serialize(node: FakeNode): string {
  if (!node.tag || node.tag === "#TEXT") return String(node._text ?? "");
  const attrs = Object.keys(node.attrs).map((k) => {
    return node.attrs[k] === "" ? " " + k : " " + k + '="' + node.attrs[k] + '"';
  }).join("");
  const id = node.id ? ' id="' + node.id + '"' : "";
  const cls = node.className ? ' class="' + node.className + '"' : "";
  const ttl = node.title ? ' title="' + node.title + '"' : "";
  const dis = node.disabled ? " disabled" : "";
  const kids = node.children.map((c) => { return serialize(c); }).join("");
  const tag = node.tag.toLowerCase();
  return "<" + tag + id + cls + ttl + attrs + dis + ">" + (kids || node._text) + "</" + tag + ">";
}

/* Shared by the V5 and V6 describes below: the builders and the state they read, imported
   once under the permissive DOM stub (polling.js's import graph touches the DOM at module
   top level — see the V5 note). */
let jobRowNode: (j: Record<string, unknown>) => FakeNode;
let ruleRowNode: (r: Record<string, unknown>) => FakeNode;
let connRowNode: (c: Record<string, unknown>) => FakeNode;

beforeAll(async () => {
  const anyG = globalThis as unknown as Record<string, unknown>;
  if (!anyG.document) {
    const elem = () => ({
      style: {}, dataset: {}, innerHTML: "", textContent: "",
      classList: { add() {}, remove() {} },
      setAttribute() {}, appendChild() {}, addEventListener() {},
    });
    anyG.document = {
      // The builders run against createElement — the elements they return carry the tree
      // serialize() reads back. The other lookups stay permissive elem() stubs.
      getElementById: () => elem(), createElement: (t: string) => new FakeNode(t),
      createElementNS: (_ns: string, t: string) => new FakeNode(t),
      createDocumentFragment: () => FakeNode.fragment(),
      createTextNode: (text: string) => { const n = new FakeNode("#text"); n.textContent = text; return n; },
      querySelector: () => null, querySelectorAll: () => [],
      addEventListener() {}, documentElement: elem(), body: elem(),
    };
  }
  const polling = await import("../src/polling.js") as unknown as {
    ruleRowNode: (r: Record<string, unknown>) => FakeNode;
    connRowNode: (c: Record<string, unknown>) => FakeNode;
  };
  ({ ruleRowNode, connRowNode } = polling);
  ({ jobRowNode } = await import("../src/jobs.js") as unknown as { jobRowNode: (j: Record<string, unknown>) => FakeNode });
});

describe("visual refresh V5 — one primary action per row", () => {

  it("the job row keeps Run now plus one ellipsis; Edit/History/Delete move to the menu", () => {
    const html = serialize(jobRowNode({ name: "nightly", command: "cargo test", enabled: true, trigger: { kind: "cron", expression: "0 4 * * *" } }));
    expect((html.match(/<button/g) || []).length).toBe(2);
    expect(html).toContain("data-run");
    expect(html).toContain("data-more");
    expect(html).toContain("#i-ellipsis");
    expect(html).not.toContain(">Delete<");
    expect(html).not.toContain(">Edit<");
    expect(html).not.toContain(">History<");
  });

  it("the rule row keeps Start/Stop plus the ellipsis; Force free and Delete are menu items", () => {
    const html = serialize(ruleRowNode({ id: "r1", name: "pg", localPort: 18989, targetHost: "127.0.0.1", targetPort: 5432, connectionName: "bastion", state: "down", portOwner: { pid: 7, name: "swiss" } }));
    expect((html.match(/<button/g) || []).length).toBe(2);
    expect(html).toContain('data-act="start"');
    expect(html).not.toContain("Force free");
    expect(html).not.toContain(">Delete<");
    expect(html).not.toContain(">Edit<");
  });

  it("the connection row keeps Test plus the ellipsis", () => {
    const html = serialize(connRowNode({ id: "c1", name: "bastion", host: "10.0.0.4", port: 22, username: "jdoe", authType: "key", state: "down" }));
    // Test, the ellipsis - and the host's eye, which reveals rather than acts (redacted()).
    expect((html.match(/<button/g) || []).length).toBe(3);
    expect((html.match(/<button[^>]*class="redact-eye"/g) || []).length).toBe(1);
    expect(html).toContain("data-test");
    expect(html).not.toContain(">Delete<");
    expect(html).not.toContain(">Edit<");
  });
});

/* Visual refresh V6 follow-up (SPEC §panel.design): every status dot carries a title. A colour — and
   the idle hollow ring above all — names no behaviour of its own, and the row already shows
   the state words, so the title reuses them via ONE builder (util.js dotTitle): the wording
   can never disagree between first paint and the 6s patch if both call the same function. */
describe("visual refresh V6 — the status dot carries a title", () => {
  it("the job row draws no dot at rest: Off is a tag, a failure a red tag, a run the titled pulse", () => {
    // SPEC §panel.pages: a job has no switch to agree with, and a green "scheduled" dot only repeated
    // the next-run column - so the lead dot is gone. An off job wears an Off tag after its name
    // (not a greyed row) and has no next run; the last run's failure is a red tag; the amber
    // pulse after the name means a run is in flight, and its title says so (SPEC §panel.design). The
    // old off dot's title ("idle — starts on first request") was the MCP lazy-start sentence.
    // Only a job with no outcome at all has never run: a manual Run now settles lastOk without a
    // lastRunAt (that is the scheduler's anchor), and the column says what it knows.
    expect(serialize(jobRowNode({ name: "m", command: "c", enabled: true, lastOk: true }))).toContain('<span class="lrow-col w-l">OK</span>');
    expect(serialize(jobRowNode({ name: "m", command: "c", enabled: true, lastOk: false }))).toContain('<span class="lrow-col w-l"><span class="tag bad">Failed</span></span>');
    const off = serialize(jobRowNode({ name: "off", command: "cargo test", enabled: false }));
    expect(off).not.toContain('class="dot');
    expect(off).toContain('<span class="tag">Off</span>');
    expect(off).toContain('<span class="lrow-col w-s">—</span>');
    expect(off).toContain('<span class="lrow-col w-l">Never run</span>');
    const bad = serialize(jobRowNode({ name: "nightly", command: "cargo test", enabled: true, lastRunAt: "2026-09-12T00:00:00Z", lastOk: false }));
    expect(bad).not.toContain('class="dot');
    expect(bad).toContain('<span class="tag bad">Failed</span>');
    const running = serialize(jobRowNode({ name: "nightly", command: "cargo test", enabled: true, running: true }));
    expect(running).toContain('class="dot starting" title="running"');
  });

  it("the schedule column is the sheet's own sentence, the raw spelling on hover", () => {
    // SPEC §panel.pages: schedFromJob -> schedToBody, the translator the sheet already has. An
    // interval needs no library; a cron speaks cronstrue once it has loaded, and until then
    // (and whenever it cannot say it) the column is the raw spelling.
    const every = serialize(jobRowNode({ name: "sync", command: "git pull", enabled: true, trigger: { kind: "interval", everyMs: 900000, firstRun: "after-interval" } }));
    expect(every).toContain('<span class="lrow-col w-m" title="every 900 s">Every 15 minutes</span>');
    const cron = serialize(jobRowNode({ name: "nightly", command: "cargo test", enabled: true, trigger: { kind: "cron", expression: "0 4 * * *" } }));
    expect(cron).toContain('<span class="lrow-col w-m" title="cron 0 4 * * *">cron 0 4 * * *</span>');
    // The next and last run read relative ("in 20 hr."); their titles are the whole moment, date
    // included. whenLabel gave a clock time alone for anything not a day in the past, so a run due
    // tomorrow at 05:59 hovered as "Next run: 05:59:00" (found on a walk of SPEC §panel.pages).
    const due = new Date(Date.now() + 20 * 3600 * 1000).toISOString();
    const next = serialize(jobRowNode({ name: "n", command: "c", enabled: true, nextDueAt: due }));
    expect(next).toContain('title="Next run: ' + new Date(due).toLocaleString("en") + '"');
    // A v2 title is the name; the id every action addresses rides the sub-line in mono.
    const titled = serialize(jobRowNode({ name: "nightly", title: "Nightly build", command: "cargo test", enabled: true }));
    expect(titled).toContain('<div class="lrow-name">Nightly build</div>');
    expect(titled).toContain('<div class="lrow-sub"><code>nightly</code> · <code>cargo test</code></div>');
  });

  it("the rule dot: error names its reason, a busy row says starting", () => {
    // SPEC §panel.toolchain: the dot is a built node — class, flag attribute and title are asserted
    // each on its own, because a builder fixes the tree, not the attribute order a string
    // concatenation happened to leave behind.
    const bad = serialize(ruleRowNode({ id: "r1", name: "pg", localPort: 18989, targetHost: "127.0.0.1", targetPort: 5432, connectionName: "s", state: "error", reason: "SSH refused" }));
    expect(bad).toContain('class="dot error"');
    expect(bad).toContain('title="error: SSH refused"');
    // SPEC §panel.pages: the library row's lead column holds it, and the reason is the row's red line.
    expect(bad).toContain('<span class="lrow-lead"><span class="dot error"');
    expect(bad).toContain('<div class="lrow-err" title="SSH refused">SSH refused</div>');
    setTunBusy("r2", "start");
    const busy = serialize(ruleRowNode({ id: "r2", name: "pg", localPort: 18990, targetHost: "127.0.0.1", targetPort: 5432, connectionName: "s", state: "down" }));
    expect(busy).toContain('class="dot starting"');
    expect(busy).toContain('title="starting"');
    clearTunBusy("r2");
  });

  it("the connection dot: connected reads as up", () => {
    const html = serialize(connRowNode({ id: "c1", name: "bastion", host: "10.0.0.4", port: 22, username: "jdoe", authType: "key", state: "connected" }));
    expect(html).toContain('class="dot up"');
    expect(html).toContain('title="up"');
  });

  it("the serves dots: a known MCP's dot says its state, an unknown one defers to the row", () => {
    const html = serialize(ruleRowNode({
      id: "r1", name: "pg", localPort: 18989, targetHost: "127.0.0.1", targetPort: 5432, connectionName: "s", state: "up",
      mcpRows: [{ name: "mysql", state: "up", known: true }, { name: "ghost", state: "", known: false }],
    }));
    expect(html).toMatch(/mysql<span class="dot up" title="up"/);
    // No title on the unknown dot: the .serves span around it already answers the hover
    // ("no MCP named ghost"), and an empty title attribute would suppress that.
    expect(html).toMatch(/ghost<span class="dot"(?![^>]*title)[^>]*><\/span>/);
  });
});

// The pure half of the S6 jobs panel (SPEC §jobs.api): everything the config editor promises
// about round-trips is testable right here, without a browser — the guarantees the API
// cannot give (a form edit must not drop fields the form does not know) live in this module.

describe("triggerSummary", () => {
  it("reads the v2 trigger object", () => {
    expect(triggerSummary({ trigger: { kind: "cron", expression: "30 3 * * *" } } as ApiJobRow)).toBe("cron 30 3 * * *");
    expect(triggerSummary({ trigger: { kind: "interval", everyMs: 3600000, firstRun: "after-interval" } } as ApiJobRow)).toBe("every 3600 s");
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
  it("never silently drops a key it does not own (the server's refusal must name it, not a form edit)", () => {
    // The JSON textarea can hand the form any object; def.rs check_known refuses a key it
    // does not know, naming it. That refusal is only reachable if the form leaves the key
    // in place - a form that quietly deleted it would turn a loud 400 into lost text. The
    // intersection says what this object is: a definition plus one key the server will refuse.
    const base: JobDef & { fanout: { regions: string[] } } = {
      title: "nightly",
      trigger: { kind: "cron", expression: "30 3 * * *", timezone: "local" },
      action: { type: "process.legacy-command", input: { command: "cmd /c x" } },
      fanout: { regions: ["eu", "us"] },
    };
    const form = v2ToForm(base);
    form.title = "nightly vacuum";
    const out = formToV2(form, base, { command: "cmd /c x" }) as typeof base;
    expect(out.fanout).toEqual({ regions: ["eu", "us"] });
    expect(out.title).toBe("nightly vacuum");
  });

  it("is identity (modulo defaults spelled out) for a definition the form leaves alone", () => {
    const base = defTemplate("probe");
    const out = formToV2(v2ToForm(base), base, { command: "" });
    // The form owns overlap/misfire and writes their defaults explicitly - explicit
    // defaults parse identically and keep the JSON editor honest about what will save.
    const strip = (d: JobDef) => ({ ...d, overlap: undefined, misfire: undefined });
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

/* SPEC §panel.toolchain: the sheet's enum values are the server's (swiss-jobs/src/jobs/def.rs enum_str
   and the descriptor's config_schema in src/builtin.rs). Until 2026-09 the template and the
   firstRun select both said "aligned", a word the parser refuses with `must be one of:
   "after-interval", "immediate"` - every Advanced-sheet create with the default interval
   trigger was a 400. These pin the lists to the parser's spelling. */
describe("the definition enums match def.rs", () => {
  it("offers exactly the parser's values, in the parser's spelling", () => {
    expect([...JOB_TRIGGER_KINDS].sort()).toEqual(["cron", "interval", "manual"]);
    expect([...JOB_FIRST_RUNS]).toEqual(["after-interval", "immediate"]);
    expect([...JOB_OVERLAPS]).toEqual(["skip", "queue-one"]);
    expect([...JOB_MISFIRES]).toEqual(["skip", "run-once"]);
    expect([...JOB_BACKOFFS]).toEqual(["fixed", "exponential"]);
    expect([...JOB_RETRY_ONS]).toEqual(["failure", "timeout"]);
    expect([...JOB_CAPTURES]).toEqual(["tail", "none"]);
  });

  it("templates and defaults a firstRun the server accepts (the 'aligned' regression)", () => {
    expect(defTemplate("x").trigger?.firstRun).toBe("after-interval");
    expect(JOB_FIRST_RUNS).toContain(defTemplate("x").trigger?.firstRun);
    // A definition without a firstRun reads as the server's default, not an invented word.
    expect(v2ToForm({ trigger: { kind: "interval", everyMs: 1000 } }).firstRun).toBe("after-interval");
    // And the form writes that default back verbatim.
    const out = formToV2(v2ToForm({ trigger: { kind: "interval", everyMs: 1000 } }), {}, {});
    expect(out.trigger).toEqual({ kind: "interval", everyMs: 1000, firstRun: "after-interval" });
  });

  it("legalOf narrows a select value to its list and refuses anything else by name", () => {
    expect(legalOf("immediate", JOB_FIRST_RUNS, "trigger.firstRun")).toBe("immediate");
    expect(() => legalOf("aligned", JOB_FIRST_RUNS, "trigger.firstRun"))
      .toThrow('trigger.firstRun must be one of after-interval, immediate, got "aligned"');
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

