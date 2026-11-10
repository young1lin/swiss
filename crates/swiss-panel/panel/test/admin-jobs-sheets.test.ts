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

// @vitest-environment happy-dom

/* The Jobs page's head and its three sheets on the library (SPEC §panel.pages): the create/edit sheet
   (the schedule builder), the advanced definition sheet behind the head's ⋯, and the run
   history as the event list. None of them had a suite before - their structure was only ever
   checked by eye - so this file pins what the page promises: the sheet opens visibly, each
   caption heads the fields it names, a caption over several controls presses none of them,
   the schedule is said back in one sentence, and a run's failure is a tag, not a colour. */

import { describe, it, expect, beforeAll, beforeEach, afterEach, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { installBus, type BusSocket } from "../src/bus/bus.js";
import { create, toBinary } from "../src/bus/codec.js";
import { FrameSchema, Kind, Module } from "../src/bus/pb/swiss/bus/v1/bus_pb.js";
import { HostSub, RunChangedSchema, RunState, RunsId } from "../src/bus/pb/swiss/bus/v1/host_pb.js";

/* eslint-disable-next-line @typescript-eslint/no-explicit-any -- the module's public surface is
   driven by name (renderJobs, openRunsSheet), so a typed import would be circular. */
let jobs: any;
/* eslint-disable-next-line @typescript-eslint/no-explicit-any -- the state setters, same reason */
let state: any;
/* eslint-disable-next-line @typescript-eslint/no-explicit-any -- the loader, same reason */
let polling: any;
let replies: Record<string, unknown> = {};
/* Runs inside a POST before it answers: a case does there what the browser does in that window. */
let duringPost: (() => void) | null = null;
let calls: string[] = [];
let jobsView: { followJobs(): void; clock(): void; unmount(): void; poll?: unknown };

const here = dirname(fileURLToPath(import.meta.url));
const $ = (id: string): HTMLElement => document.getElementById(id) as HTMLElement;
const tick = async (n = 4): Promise<void> => { for (let i = 0; i < n; i++) await new Promise((r) => setTimeout(r, 0)); };

/* The shell skeleton from the SERVED index.html (the house idiom): modules in this graph wire
 * shell controls at evaluation time, so the ids exist before the import. */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

beforeAll(async () => {
  // The schedule sentence lazy-loads the vendored cronstrue as a <script>. happy-dom refuses to
  // run script files and logs it - an artifact of the test DOM, so it is switched off; the
  // sentence then falls back to the raw spelling, which is what these cases expect.
  const settings = (window as unknown as { happyDOM: { settings: Record<string, boolean> } }).happyDOM.settings;
  settings.disableJavaScriptFileLoading = true;
  settings.handleDisabledFileLoadingAsSuccess = true;
  Object.assign(globalThis, {
    fetch: (url: string, init?: RequestInit): Promise<Response> => {
      const u = String(url);
      const key = (init?.method === "POST" ? "POST " : "") + u;
      calls.push(key);
      if (init?.method === "POST" && duringPost) duringPost();
      const reply = replies[key] ?? replies[u];
      const ok = reply !== undefined;
      return Promise.resolve({ status: ok ? 200 : 404, ok, json: () => Promise.resolve(reply ?? { error: "no stub for " + u }) } as Response);
    },
  });
  document.body.innerHTML = shellSkeleton();
  jobs = await import("../src/jobs.js");
  state = await import("../src/job-state.js");
  polling = await import("../src/polling.js");
  jobsView = await import("../src/views/jobs.js");
  (await import("../src/ui-state.js")).setCurrentView("jobs");
  openJobsBus();
});

afterEach(() => { vi.useRealTimers(); });

beforeEach(() => {
  replies = {};
  calls = [];
  duringPost = null;
  document.body.innerHTML = shellSkeleton();
  state.setJobRows([]);
  state.setJobGroupNames(["default"]);
});

function caps(): string[] {
  return Array.from($("sheet").querySelectorAll(".form-cap")).map((c) => c.textContent || "");
}
/** True when `a` comes before `b` in document order. */
function before(a: Element, b: Element): boolean {
  return !!(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);
}

class JobsSocket implements BusSocket {
  static all: JobsSocket[] = [];
  binaryType: BinaryType = "blob";
  private listeners = new Map<string, Array<(ev: Event) => void>>();
  constructor() { JobsSocket.all.push(this); }
  addEventListener(type: string, listener: (ev: Event) => void): void {
    const list = this.listeners.get(type) ?? [];
    list.push(listener);
    this.listeners.set(type, list);
  }
  removeEventListener(): void { /* the link outlives the case */ }
  send(_data: Uint8Array): void { /* the case does not read what the page sent */ }
  close(): void { /* never closed under test */ }
  private emit(type: string, ev: Event): void {
    for (const fn of this.listeners.get(type) ?? []) fn(ev);
  }
  open(): void { this.emit("open", new Event("open")); }
  receive(bytes: Uint8Array): void {
    const copy = new Uint8Array(bytes);
    for (const fn of this.listeners.get("message") ?? []) fn(new MessageEvent("message", { data: copy.buffer }));
  }
}

let bus: JobsSocket;

function openJobsBus(): void {
  JobsSocket.all.length = 0;
  installBus({
    socket: JobsSocket,
    random: () => 0.5,
    reload: () => {},
    info: () => Promise.resolve(),
    notice: () => {},
    url: "ws://127.0.0.1/api/bus",
  });
  bus = JobsSocket.all[JobsSocket.all.length - 1];
  bus.open();
}

function runFrame(owner: string, state: RunState, runId = 7n): Uint8Array {
  return toBinary(FrameSchema, create(FrameSchema, {
    module: Module.HOST,
    sub: HostSub.RUNS,
    id: RunsId.RUNS_CHANGED,
    kind: Kind.EVENT,
    seq: 1,
    key: "",
    origin: "page-a",
    body: toBinary(RunChangedSchema, create(RunChangedSchema, {
      runId,
      revision: 3,
      state,
      action: "jobs.run",
      owner,
      actor: "",
      lane: "",
      queuedAtMs: 1000n,
      startedAtMs: 1000n,
      endedAtMs: 1012n,
    })),
  }));
}

describe("the Jobs head (SPEC §panel.pages)", () => {
  it("is one sentence, the folder-plus glyph, a ⋯ and the one primary - no caption, no foot", () => {
    jobs.renderJobs();
    const pane = $("pane");
    expect(pane.querySelector(".pane-desc")?.textContent).toBe("Commands the gateway runs on a schedule, in local time.");
    const acts = Array.from(pane.querySelectorAll(".pane-actions > *"));
    expect(acts.map((b) => b.id)).toEqual(["jobNewGroup", "jobMore", "jobNew"]);
    expect($("jobNewGroup").querySelector("use")?.getAttribute("href")).toBe("#i-folder-plus");
    // New carries the plus glyph (SPEC §panel.design rule 7), like New in every other module.
    expect($("jobNew").className).toBe("btn icon");
    expect($("jobNew").getAttribute("aria-label")).toBe("New");
    expect($("jobNew").querySelector("use")?.getAttribute("href")).toBe("#i-plus");
    expect(pane.querySelector(".sec-cap")).toBeNull(); // "Scheduled commands" named the page twice
    expect(pane.querySelector(".page-foot, .tun-foot")).toBeNull(); // the count is the context bar's
  });
});

describe("the list stays put under the poll", () => {
  const job = { name: "echo", command: "cmd /c echo hi", enabled: true, trigger: { kind: "interval", everyMs: 3600000, firstRun: "after-interval" } };

  it("a poll patches the drawn rows in place - it never rebuilds them", async () => {
    // Found on a walk of SPEC §panel.pages, in the loader: loadJobs(true) chose between patch and
    // rebuild by looking for the page foot ([data-foot]). The foot is gone (the count is the
    // context bar's), so every 6 s poll rebuilt the list - cancelling a drag, dropping focus.
    replies["/api/jobs"] = { jobs: [job], groups: ["default"] };
    state.setJobRows([job]);
    jobs.renderJobs();
    const row = $("pane").querySelector('[data-job="echo"]');
    await polling.loadJobs(true);
    expect($("pane").querySelector('[data-job="echo"]')).toBe(row);
  });

  it("Run now keeps its focus through the run, and the row shows the outcome", async () => {
    // SPEC §jobs.panel: the wait is the terminal HOST.RUNS event. A success costs no GET.
    replies["/api/jobs"] = { jobs: [Object.assign({}, job, { lastOk: true })], groups: ["default"] };
    replies["POST /api/jobs/echo/run"] = { runId: 7 };
    state.setJobRows([job]);
    jobs.renderJobs();
    const run = $("pane").querySelector('[data-job="echo"] [data-run]') as HTMLButtonElement;
    run.focus();
    // The button is disabled while the run is out, and a real browser blurs a focused control
    // the moment it is disabled; happy-dom does not, so the case does it by hand where Chrome does.
    duringPost = (): void => { run.disabled = false; run.blur(); run.disabled = true; };
    run.click();
    await tick();
    bus.receive(runFrame("jobs", RunState.SUCCEEDED));
    await tick();
    expect(calls.filter((c) => c === "/api/runs/7")).toEqual([]);
    expect($("pane").querySelector('[data-job="echo"] [data-run]')).toBe(run); // patched, not rebuilt
    expect(run.disabled).toBe(false);
    expect(document.activeElement).toBe(run);
    // A manual run settles lastOk but never stamps lastRunAt (the scheduler's anchor), so the
    // column says what it knows - OK - instead of "Never run".
    expect($("pane").querySelectorAll('[data-job="echo"] .lrow-col')[2].textContent).toBe("OK");
  });
});

describe("the create sheet", () => {
  it("opens visibly on the library sheet, each caption heading its own fields", () => {
    jobs.renderJobs();
    $("jobNew").click();
    const host = $("sheet");
    expect(host.hidden).toBe(false);
    const dlg = host.querySelector(".sheet[role=dialog]")!;
    expect(dlg.getAttribute("aria-label")).toBe("New job");
    expect(dlg.querySelector("h2")?.textContent).toBe("New job in default");
    expect(caps()).toEqual(["Schedule", "Options", "Environment"]);
    // Environment used to sit directly on top of Options: an empty section, and the env
    // variables filed under Options. Now each caption heads what it names.
    const [schedule, options, environment] = Array.from(host.querySelectorAll(".form-cap"));
    expect(before(schedule, $("jf-sched")) && before($("jf-sched"), options)).toBe(true);
    expect(before(options, $("jf-timeout")) && before($("jf-enabled"), environment)).toBe(true);
    expect(before(environment, $("jf-env"))).toBe(true);
    // Every field is the library's: the command's rule is its hint, enabled is a check field.
    expect($("jf-command").closest(".fld")?.querySelector(".hint")?.textContent).toContain("One command");
    expect($("jf-enabled").closest("label.check")?.parentElement?.className).toBe("fld");
    expect(Array.from(dlg.querySelectorAll(".sheet-foot .btn")).map((b) => b.textContent)).toEqual(["Advanced…", "Cancel", "Add"]);
  });

  it("the modes are the library seg, and the schedule is said back in one sentence", () => {
    jobs.renderJobs();
    $("jobNew").click();
    const segEl = $("jf-sched").querySelector(".seg[role=tablist]")!;
    expect(Array.from(segEl.querySelectorAll("[data-mode]")).map((b) => b.getAttribute("data-mode")))
      .toEqual(["interval", "daily", "weekly", "monthly", "cron"]);
    // The labels are sentence case like every other seg (the en words were lowercase ids).
    expect(Array.from(segEl.querySelectorAll("[data-mode]")).map((b) => b.textContent))
      .toEqual(["Interval", "Daily", "Weekly", "Monthly", "Cron"]);
    expect(segEl.querySelector('[aria-selected="true"]')?.getAttribute("data-mode")).toBe("daily");
    (segEl.querySelector('[data-mode="interval"]') as HTMLElement).click();
    expect(segEl.querySelector('[aria-selected="true"]')?.getAttribute("data-mode")).toBe("interval");
    const every = $("jf-ev") as HTMLInputElement;
    every.value = "15";
    ($("jf-ev-u") as HTMLSelectElement).value = "minutes";
    every.dispatchEvent(new Event("input"));
    expect($("jf-say").textContent).toBe("Every 15 minutes.");
    expect($("jf-ev").closest(".two")).not.toBeNull(); // the pair, from ui/form.ts
  });

  it("weekly: the Days caption is a group, so it presses no weekday", () => {
    jobs.renderJobs();
    $("jobNew").click();
    ($("jf-sched").querySelector('[data-mode="weekly"]') as HTMLElement).click();
    const days = $("jf-days");
    const group = days.closest(".field")!;
    // A <label> hands a click on its caption to its first labelable child - "Days" pressed
    // Sunday. The group names the buttons instead (ui/form.ts field({ group })).
    expect(group.tagName).toBe("DIV");
    expect(group.getAttribute("role")).toBe("group");
    const cap = group.querySelector(":scope > span")!;
    expect(cap.textContent).toBe("Days");
    expect(group.getAttribute("aria-labelledby")).toBe(cap.id);
    const on = (): string[] => Array.from(days.querySelectorAll("button.on")).map((b) => b.getAttribute("data-dow") || "");
    const was = on();
    (cap as HTMLElement).click();
    expect(on()).toEqual(was);
  });

  it("Edit reads every schedule mode back without crashing - the weekly m[4] crash (2026-10-10)", async () => {
    // The weekly preset's parser read group 4 of a 3-group regex: creating a weekly
    // job worked (schedToBody) but Edit crashed at schedFromJob before the sheet drew.
    // Every mode's read-back is asserted here so the class stays covered, not just the fix.
    const cases: Array<[string, Record<string, unknown>, () => void]> = [
      ["interval-2h", { everySec: 7200 }, (): void => {
        expect(($("jf-sched").querySelector('[aria-selected="true"]') as HTMLElement).dataset.mode).toBe("interval");
        expect(($("jf-ev") as HTMLInputElement).value).toBe("2");
        expect(($("jf-ev-u") as HTMLSelectElement).value).toBe("hours");
      }],
      ["daily-9", { cron: "0 9 * * *" }, (): void => {
        expect(($("jf-sched").querySelector('[aria-selected="true"]') as HTMLElement).dataset.mode).toBe("daily");
        expect(($("jf-at") as HTMLInputElement).value).toBe("09:00");
      }],
      ["weekly-12345", { cron: "30 8 * * 1-5" }, (): void => {
        expect(($("jf-sched").querySelector('[aria-selected="true"]') as HTMLElement).dataset.mode).toBe("weekly");
        expect(($("jf-at") as HTMLInputElement).value).toBe("08:30");
        const on = Array.from($("jf-days").querySelectorAll("button.on")).map((b) => b.getAttribute("data-dow"));
        expect(on).toEqual(["1", "2", "3", "4", "5"]);
      }],
      ["weekly-list", { cron: "0 7 * * 1,3,5" }, (): void => {
        const on = Array.from($("jf-days").querySelectorAll("button.on")).map((b) => b.getAttribute("data-dow"));
        expect(on).toEqual(["1", "3", "5"]);
      }],
      ["weekly-full-week", { cron: "0 8 * * 0-7" }, (): void => {
        // "0-7" is every day; the old parser folded 7%7 onto 0 and met the break
        // immediately, silently narrowing an every-day job to Sundays (2026-10-10).
        const on = Array.from($("jf-days").querySelectorAll("button.on")).map((b) => b.getAttribute("data-dow"));
        expect(on).toEqual(["0", "1", "2", "3", "4", "5", "6"]);
      }],
      ["weekly-wrap", { cron: "0 8 * * 6-1" }, (): void => {
        const on = Array.from($("jf-days").querySelectorAll("button.on")).map((b) => b.getAttribute("data-dow"));
        expect(on).toEqual(["0", "1", "6"]);
      }],
      ["monthly-15", { cron: "0 8 15 * *" }, (): void => {
        expect(($("jf-sched").querySelector('[aria-selected="true"]') as HTMLElement).dataset.mode).toBe("monthly");
        expect(($("jf-md") as HTMLInputElement).value).toBe("15");
      }],
      ["cron-raw", { cron: "*/5 9-17 * * 1-5" }, (): void => {
        expect(($("jf-sched").querySelector('[aria-selected="true"]') as HTMLElement).dataset.mode).toBe("cron");
        expect(($("jf-cron-in") as HTMLInputElement).value).toBe("*/5 9-17 * * 1-5");
      }],
    ];
    for (const [name, triggerFields, assert] of cases) {
      state.setJobRows([Object.assign({ name, command: "cmd /c echo hi", enabled: true }, triggerFields)]);
      jobs.renderJobs();
      ($("pane").querySelector(`[data-job="${name}"] [data-more]`) as HTMLElement).click();
      const edit = Array.from(document.querySelectorAll("#menu button")).find((b) => b.textContent === "Edit") as HTMLElement;
      expect(edit, `the Edit item for ${name}`).toBeTruthy();
      edit.click();
      await tick();
      expect($("sheet").hidden, `the sheet for ${name}`).toBe(false);
      assert();
      $("jf-cancel").click(); // close for the next case
    }
  });
});

describe("the advanced sheet, behind the head's ⋯", () => {
  it("New (advanced) opens the definition sheet: the trigger's rule is its hint, retry-on a group", async () => {
    replies["/api/plugins/jobs/config"] = { config: { definitions: {}, groups: ["default"] }, revision: 3 };
    replies["/api/actions"] = { actions: [] };
    jobs.renderJobs();
    $("jobMore").click();
    const item = Array.from(document.querySelectorAll("#menu button")).find((b) => b.textContent === "New (advanced)") as HTMLElement;
    expect(item).toBeTruthy();
    item.click();
    await tick();
    const dlg = $("sheet").querySelector(".sheet[role=dialog]")!;
    expect($("sheet").hidden).toBe(false);
    expect(dlg.className).toBe("sheet"); // "sheet wide" was a class no stylesheet ever defined
    expect(dlg.querySelector("h2")?.textContent).toBe("New definition in default");
    // "one schedule per definition" is the trigger field's hint, not a field of its own.
    expect($("jv-kind").closest(".fld")?.querySelector(".hint")?.textContent).toBe("one schedule per definition");
    expect(dlg.textContent).not.toContain("first firing");
    const retry = dlg.querySelector("[data-retryon]")!.closest(".field[role=group]")!;
    expect(retry.querySelectorAll("label.check input[data-retryon]").length).toBe(2);
    // The trigger kind shows its own row and hides the other.
    const kind = $("jv-kind") as HTMLSelectElement;
    kind.value = "cron";
    kind.dispatchEvent(new Event("change"));
    expect($("jv-cron-row").style.display).toBe("");
    expect($("jv-interval-row").style.display).toBe("none");
    expect(Array.from(dlg.querySelectorAll(".sheet-foot .btn")).map((b) => b.textContent)).toEqual(["Form → JSON", "Cancel", "Save"]);
  });
});

describe("the run history, as the event list", () => {
  const run = (at: string, o: Record<string, unknown>): Record<string, unknown> => {
    return Object.assign({ at, trigger: "schedule", ok: true, ms: 40, outcome: "ran", attempt: 1, attempts: 1, output: "", chars: 0 }, o);
  };

  it("draws timeline rows: a failure's exit code is a red tag, identical runs fold into ×N", async () => {
    replies["/api/jobs/nightly/runs?limit=20"] = {
      runs: [
        run("2026-09-24T03:00:00Z", { ok: false, exitCode: 1, output: "disk full\nmore" }),
        run("2026-09-23T03:00:00Z", { output: "ok" }),
        run("2026-09-23T02:00:00Z", { output: "ok" }),
        run("2026-09-23T01:00:00Z", { outcome: "skipped", reason: "overlap", ok: false }),
      ],
    };
    await jobs.openRunsSheet("nightly");
    const dlg = $("sheet").querySelector(".sheet[role=dialog]")!;
    expect($("sheet").hidden).toBe(false);
    expect(dlg.querySelector("h2")?.textContent).toBe("Runs — nightly");
    const items = Array.from(dlg.querySelectorAll(".tl-item"));
    expect(items.length).toBe(3); // the two identical "ok" runs fold
    expect(items[0].querySelector(".tag.bad")?.textContent).toBe("exit 1");
    expect(items[0].querySelector(".tl-arg")?.textContent).toBe("disk full");
    expect(items[1].querySelector(".tl-n")?.textContent).toBe("×2");
    // Not a run at all: an amber tag and no duration.
    expect(items[2].querySelector(".tag.warn")?.textContent).toBe("skipped");
    expect(items[2].querySelector(".tl-ms")).toBeNull();
  });

  it("opening a row shows the meta line and the output block; closing it takes them away", async () => {
    replies["/api/jobs/nightly/runs?limit=20"] = { runs: [run("2026-09-24T03:00:00Z", { ok: false, exitCode: 2, output: "boom" })] };
    await jobs.openRunsSheet("nightly");
    const item = $("sheet").querySelector(".tl-item") as HTMLElement;
    (item.querySelector(".tl-sum") as HTMLElement).click();
    expect(item.classList.contains("open")).toBe(true);
    expect(item.querySelector(".tl-meta")?.textContent).toContain("exit 2");
    expect(item.querySelector(".vblock-cap")?.textContent).toBe("Output");
    expect(item.querySelector(".vblock pre")?.textContent).toContain("boom");
    (item.querySelector(".tl-sum") as HTMLElement).click();
    expect(item.classList.contains("open")).toBe(false);
    expect(item.querySelector(".tl-body")).toBeNull();
  });

  it("no runs yet is a note, and Done closes the sheet", async () => {
    replies["/api/jobs/idle/runs?limit=20"] = { runs: [] };
    await jobs.openRunsSheet("idle");
    expect($("sheet").querySelector(".note")?.textContent).toBe("No runs recorded yet — wait for the schedule, or press Run now.");
    $("jr-done").click();
    expect($("sheet").hidden).toBe(true);
  });
});

describe("the jobs list hears the bus (SPEC §jobs.panel)", () => {
  const job = { name: "echo", command: "cmd /c echo hi", enabled: true, trigger: { kind: "interval", everyMs: 3600000, firstRun: "after-interval" } };

  it("a remote run is not a jobs read, and two jobs events are one read", async () => {
    vi.useFakeTimers();
    replies["/api/jobs"] = { jobs: [job], groups: ["default"] };
    jobsView.followJobs();
    calls = [];
    bus.receive(runFrame("remote", RunState.SUCCEEDED));
    await vi.advanceTimersByTimeAsync(200);
    expect(calls.filter((c) => c === "/api/jobs")).toEqual([]);
    bus.receive(runFrame("jobs", RunState.SUCCEEDED));
    bus.receive(runFrame("jobs", RunState.FAILED));
    await vi.advanceTimersByTimeAsync(200);
    expect(calls.filter((c) => c === "/api/jobs")).toEqual(["/api/jobs"]);
    jobsView.unmount();
  });

  it("clock redraws the held rows and asks for nothing", () => {
    replies["/api/jobs"] = { jobs: [job], groups: ["default"] };
    state.setJobRows([job]);
    jobs.renderJobs();
    calls = [];
    jobsView.clock();
    expect(calls).toEqual([]);
    expect(jobsView.poll).toBeUndefined();
  });

  it("a failed run costs one GET for the error, and an early event is kept", async () => {
    replies["/api/jobs"] = { jobs: [job], groups: ["default"] };
    replies["POST /api/jobs/echo/run"] = { runId: 7 };
    replies["/api/runs/7"] = { runId: 7, state: "failed", error: "disk full" };
    state.setJobRows([job]);
    jobs.renderJobs();
    const run = $("pane").querySelector('[data-job="echo"] [data-run]') as HTMLButtonElement;
    duringPost = (): void => { bus.receive(runFrame("jobs", RunState.FAILED)); };
    run.click();
    await tick();
    expect(calls.filter((c) => c === "/api/runs/7")).toEqual(["/api/runs/7"]);
    expect($("toast").textContent).toContain("disk full");
  });

  it("leaving the page does not end the wait", async () => {
    replies["/api/jobs"] = { jobs: [job], groups: ["default"] };
    replies["POST /api/jobs/echo/run"] = { runId: 9 };
    replies["/api/runs/9"] = { runId: 9, state: "failed", error: "still here" };
    state.setJobRows([job]);
    jobs.renderJobs();
    const run = $("pane").querySelector('[data-job="echo"] [data-run]') as HTMLButtonElement;
    run.click();
    await tick();
    jobsView.unmount();
    calls = [];
    bus.receive(runFrame("jobs", RunState.FAILED, 9n));
    await tick();
    expect(calls.filter((c) => c.startsWith("/api/runs/"))).toEqual(["/api/runs/9"]);
  });
});
