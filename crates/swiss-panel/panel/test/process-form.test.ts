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

/* The editor's asynchronous seams use the real fetch surface and house sheet.
 * Deferred answers reproduce replacement and revision races without timers. */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { AppEntry } from "../src/views/process/types.js";

let editor: typeof import("../src/views/process/form.js");
let layers: typeof import("../src/ui/sheet.js");
interface Request { path: string; method: string; body?: string; signal?: AbortSignal | null }
let requests: Request[];
let config: { apps: AppEntry[] };
let revision: number;
let respond: (request: Request) => Promise<Response>;
const configPath = "/api/plugins/process/config";
const here = dirname(fileURLToPath(import.meta.url));
const $ = (id: string): HTMLElement => document.getElementById(id) as HTMLElement;
const value = (id: string, text: string): void => { ($("pr-f-" + id) as HTMLInputElement).value = text; };
const app = (name = "demo", program = "node"): AppEntry => ({ name, start: { program } });
const puts = (): Request[] => requests.filter((r) => r.method === "PUT");

function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

function answer(body: unknown, status = 200): Response {
  return { status, ok: status >= 200 && status < 300, json: () => Promise.resolve(body) } as Response;
}

function deferred<T>(): { promise: Promise<T>; resolve(value: T): void } {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

async function drain(): Promise<void> {
  for (let n = 0; n < 24; n++) await Promise.resolve();
}

function save(): Promise<void> {
  $("prFormSave").click();
  return drain();
}

beforeAll(async () => {
  document.body.innerHTML = shellSkeleton();
  editor = await import("../src/views/process/form.js");
  layers = await import("../src/ui/sheet.js");
});

beforeEach(() => {
  document.body.innerHTML = shellSkeleton();
  localStorage.clear();
  requests = [];
  config = { apps: [] };
  revision = 7;
  respond = (request) => Promise.resolve(answer(request.method === "GET"
    ? { config, revision } : { revision: revision + 1, warnings: [] }));
  vi.stubGlobal("confirm", () => true);
  vi.stubGlobal("fetch", (url: string, init?: RequestInit): Promise<Response> => {
    const request: Request = { path: String(url), method: init?.method || "GET", body: init?.body as string | undefined, signal: init?.signal };
    requests.push(request);
    return respond(request);
  });
});

afterEach(() => { editor.disposeForm(); vi.unstubAllGlobals(); });

describe("the process app editor session", () => {
  it("round-trips stop argv as one argument per line, preserving spaces and quotes", async () => {
    config = { apps: [{ ...app(), stop: { program: "node", args: ["stop service.js", '--label="local service"'] } }] };
    await editor.openForm("demo");
    expect($("pr-f-stop-args").tagName).toBe("TEXTAREA");
    expect(($("pr-f-stop-args") as HTMLTextAreaElement).value).toBe('stop service.js\n--label="local service"');
    await save();
    const body = JSON.parse(puts()[0].body || "{}");
    expect(puts()[0].path).toBe(configPath);
    expect(body.config.apps[0].stop.args).toEqual(["stop service.js", '--label="local service"']);
  });

  it("uses the same exact line-based argv parser for start and stop", async () => {
    await editor.openForm(null);
    value("name", "demo");
    value("program", "node");
    value("stop-program", "node");
    const argv = '  spaced argument  \n--flag="two words"';
    value("args", argv);
    value("stop-args", argv);
    await save();
    const body = JSON.parse(puts()[0].body || "{}");
    expect(body.config.apps[0].start.args).toEqual(["  spaced argument  ", '--flag="two words"']);
    expect(body.config.apps[0].stop.args).toEqual(body.config.apps[0].start.args);
  });

  it("preserves a config-authored empty argument through an Edit+Save that never touched args", async () => {
    config = { apps: [{ ...app(), start: { program: "node", args: ["--pass", "", "-v"] } }] };
    await editor.openForm("demo");
    expect(($("pr-f-args") as HTMLTextAreaElement).value).toBe("--pass\n\n-v");
    await save();
    expect(puts()).toHaveLength(1);
    const body = JSON.parse(puts()[0].body || "{}");
    expect(body.config.apps[0].start.args).toEqual(["--pass", "", "-v"]);
  });

  it("preserves a config-authored trailing empty argument through an untouched Edit+Save", async () => {
    config = { apps: [{ ...app(), start: { program: "node", args: ["--pass", ""] } }] };
    await editor.openForm("demo");
    expect(($("pr-f-args") as HTMLTextAreaElement).value).toBe("--pass\n");
    await save();
    expect(puts()).toHaveLength(1);
    const body = JSON.parse(puts()[0].body || "{}");
    expect(body.config.apps[0].start.args).toEqual(["--pass", ""]);
  });

  it("keeps trimming the user's final Enter once the args text has been edited", async () => {
    config = { apps: [{ ...app(), start: { program: "node", args: ["--pass", ""] } }] };
    await editor.openForm("demo");
    // Any edit hands the field back to the line parser, whose single
    // trailing-blank-line trim keeps a closing Enter from becoming an argument.
    value("args", "--pass\n-v\n");
    await save();
    const body = JSON.parse(puts()[0].body || "{}");
    expect(body.config.apps[0].start.args).toEqual(["--pass", "-v"]);
  });

  it("refuses an argument carrying an embedded line break instead of silently splitting it", async () => {
    await editor.openForm(null);
    value("name", "demo");
    value("program", "node");
    value("args", "--pass\rstill-one-arg");
    await save();
    expect(puts()).toHaveLength(0);
    expect($("pr-f-err").hidden).toBe(false);
    expect($("pr-f-err").textContent).toContain("line break");
  });

  it("trims only the textarea's final newline; inner blank lines stay empty arguments", async () => {
    await editor.openForm(null);
    value("name", "demo");
    value("program", "node");
    value("args", "server.js\n\n--port 4317\n");
    await save();
    const body = JSON.parse(puts()[0].body || "{}");
    expect(body.config.apps[0].start.args).toEqual(["server.js", "", "--port 4317"]);
  });

  it("a dsh template fills the whole launch with node as the image, and saves it as argv", async () => {
    await editor.openForm(null);
    const template = $("pr-f-template") as HTMLSelectElement;
    expect(Array.from(template.options).map((o) => o.value)).toEqual(["", "dsh-web", "dsh-web-no-open"]);
    template.value = "dsh-web-no-open";
    template.dispatchEvent(new Event("change"));
    expect(($("pr-f-name") as HTMLInputElement).value).toBe("dsh");
    expect(($("pr-f-args") as HTMLTextAreaElement).value).toBe("web\n--no-open\n--port\n3080");
    expect($("pr-f-template-why").textContent).toContain("node");
    ($("pr-f-autostart") as HTMLInputElement).checked = true;
    await save();
    const body = JSON.parse(puts()[0].body || "{}");
    expect(body.config.apps[0]).toEqual({
      name: "dsh",
      start: { program: "dsh", args: ["web", "--no-open", "--port", "3080"] },
      ports: [3080],
      image: "node",
      autostart: true,
    });
  });

  it("a template never replaces a typed name, and None empties only what a template filled", async () => {
    await editor.openForm(null);
    const template = $("pr-f-template") as HTMLSelectElement;
    const pick = (key: string): void => { template.value = key; template.dispatchEvent(new Event("change")); };
    pick("dsh-web");
    value("dir", "~\\work");
    pick("");
    expect(($("pr-f-name") as HTMLInputElement).value).toBe("");
    expect(($("pr-f-program") as HTMLInputElement).value).toBe("");
    expect(($("pr-f-image") as HTMLInputElement).value).toBe("");
    expect(($("pr-f-dir") as HTMLInputElement).value).toBe("~\\work");
    value("name", "my-dsh");
    pick("dsh-web");
    expect(($("pr-f-name") as HTMLInputElement).value).toBe("my-dsh");
    expect(($("pr-f-program") as HTMLInputElement).value).toBe("dsh");
  });

  it("the port is the reader's: --port follows Ports, survives a template switch, and saves as one", async () => {
    await editor.openForm(null);
    const template = $("pr-f-template") as HTMLSelectElement;
    const pick = (key: string): void => { template.value = key; template.dispatchEvent(new Event("change")); };
    const ports = $("pr-f-ports") as HTMLInputElement;
    const typePorts = (text: string): void => { ports.value = text; ports.dispatchEvent(new Event("input")); };
    pick("dsh-web");
    expect(ports.value).toBe("3080");
    typePorts("3892");
    expect(($("pr-f-args") as HTMLTextAreaElement).value).toBe("web\n--port\n3892");
    // Half-typed or several ports leave the args on the last port that read as one.
    typePorts("38");
    typePorts("3892, 3893");
    typePorts("3892");
    pick("dsh-web-no-open");
    expect(ports.value).toBe("3892");
    expect(($("pr-f-args") as HTMLTextAreaElement).value).toBe("web\n--no-open\n--port\n3892");
    await save();
    const body = JSON.parse(puts()[0].body || "{}");
    expect(body.config.apps[0].start.args).toEqual(["web", "--no-open", "--port", "3892"]);
    expect(body.config.apps[0].ports).toEqual([3892]);
  });

  it("a port typed before the template is kept, and args the reader edited never follow Ports", async () => {
    await editor.openForm(null);
    const template = $("pr-f-template") as HTMLSelectElement;
    const ports = $("pr-f-ports") as HTMLInputElement;
    ports.value = "4100";
    template.value = "dsh-web";
    template.dispatchEvent(new Event("change"));
    expect(($("pr-f-args") as HTMLTextAreaElement).value).toBe("web\n--port\n4100");
    value("args", "web\n--port\n4100\n--host\n127.0.0.1");
    ports.value = "4200";
    ports.dispatchEvent(new Event("input"));
    expect(($("pr-f-args") as HTMLTextAreaElement).value).toBe("web\n--port\n4100\n--host\n127.0.0.1");
  });

  it("Edit offers no template: an existing app's launch is edited, not re-picked", async () => {
    config = { apps: [app()] };
    await editor.openForm("demo");
    expect(document.getElementById("pr-f-template")).toBeNull();
  });

  it("refuses Add with an existing name without issuing a PUT", async () => {
    config = { apps: [app()] };
    await editor.openForm(null);
    value("name", "demo");
    value("program", "replacement");
    await save();
    expect(puts()).toHaveLength(0);
    expect($("pr-f-err").hidden).toBe(false);
    expect($("pr-f-err").textContent).toContain("already exists");
  });

  it("keeps Edit identity readonly and saves to its original name", async () => {
    config = { apps: [app(), app("other")] };
    await editor.openForm("demo");
    expect(($("pr-f-name") as HTMLInputElement).readOnly).toBe(true);
    value("name", "other");
    value("program", "edited");
    await save();
    const body = JSON.parse(puts()[0].body || "{}");
    expect(body.config.apps.find((a: AppEntry) => a.name === "demo").start.program).toBe("edited");
    expect(body.config.apps.find((a: AppEntry) => a.name === "other")).toEqual(app("other"));
  });

  it("refuses a target app changed since open even before the first PUT", async () => {
    config = { apps: [app()] };
    await editor.openForm("demo");
    value("program", "my-edit");
    config = { apps: [app("demo", "concurrent-edit")] };
    revision++;
    await save();
    expect(puts()).toHaveLength(0);
    expect($("pr-f-err").hidden).toBe(false);
    expect($("pr-f-err").textContent).toContain("changed");
  });

  it("never retries a 409 over a concurrently modified target app", async () => {
    config = { apps: [app()] };
    await editor.openForm("demo");
    value("program", "my-edit");
    respond = (request) => {
      if (request.method === "PUT") {
        config = { apps: [app("demo", "concurrent-edit")] };
        revision++;
        return Promise.resolve(answer({ error: "configuration changed" }, 409));
      }
      return Promise.resolve(answer({ config, revision }));
    };
    await save();
    expect(puts()).toHaveLength(1);
    expect($("pr-f-err").hidden).toBe(false);
  });

  it("retries one 409 for unrelated apps only and preserves their latest configuration", async () => {
    config = { apps: [app(), app("other")] };
    const onSaved = vi.fn(async () => {});
    await editor.openForm("demo", onSaved);
    value("program", "my-edit");
    respond = (request) => {
      if (request.method === "PUT" && puts().length === 1) {
        config = { apps: [app(), app("other", "latest-other")] };
        revision = 8;
        return Promise.resolve(answer({}, 409));
      }
      return Promise.resolve(answer(request.method === "GET" ? { config, revision } : {}));
    };
    await save();
    expect(puts()).toHaveLength(2);
    const body = JSON.parse(puts()[1].body || "{}");
    expect(body.revision).toBe(8);
    expect(body.config.apps.find((a: AppEntry) => a.name === "other")).toEqual(app("other", "latest-other"));
    expect(body.config.apps.find((a: AppEntry) => a.name === "demo").start.program).toBe("my-edit");
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it("drops a late open answer rather than replacing the newer editor", async () => {
    config = { apps: [app("first"), app("second", "second-program")] };
    const slow = deferred<Response>();
    respond = () => requests.length === 1 ? slow.promise : Promise.resolve(answer({ config, revision }));
    const first = editor.openForm("first");
    await editor.openForm("second");
    value("program", "unsaved-second");
    slow.resolve(answer({ config, revision }));
    await first;
    expect(($("pr-f-name") as HTMLInputElement).value).toBe("second");
    expect(($("pr-f-program") as HTMLInputElement).value).toBe("unsaved-second");
  });

  it("drops a late picker answer after a replacement editor opens", async () => {
    await editor.openForm(null);
    const slow = deferred<Response>();
    respond = (request) => request.path.startsWith("/api/process/fs") ? slow.promise : Promise.resolve(answer({ config, revision }));
    $("pr-f-browse").click();
    await editor.openForm(null);
    slow.resolve(answer({ dir: "/old", sep: "/", parent: "/", entries: [{ name: "old-tool", dir: false, exe: true }] }));
    await drain();
    expect(($("prPickPath") as HTMLInputElement).value).toBe("");
    expect($("prPickList").textContent).not.toContain("old-tool");
  });

  it("drops an older picker navigation answer within the same editor", async () => {
    await editor.openForm(null);
    const slow = deferred<Response>();
    respond = (request) => request.path === "/api/process/fs" ? slow.promise : Promise.resolve(answer({ dir: "/new", sep: "/", parent: "/", entries: [{ name: "new-tool", dir: false, exe: true }] }));
    $("pr-f-browse").click();
    ($("prPickPath") as HTMLInputElement).value = "/new";
    $("prPickGo").click();
    await drain();
    slow.resolve(answer({ dir: "/old", sep: "/", entries: [{ name: "old-tool", dir: false, exe: true }] }));
    await drain();
    expect(($("prPickPath") as HTMLInputElement).value).toBe("/new");
    expect($("prPickList").textContent).toContain("new-tool");
    expect($("prPickList").textContent).not.toContain("old-tool");
  });

  it("Escape with the Program picker open hides the picker layer, not the form", async () => {
    await editor.openForm(null);
    value("name", "demo");
    value("program", "C:\\tools\\cc.exe");
    const slow = deferred<Response>();
    respond = (request) => { return request.path.startsWith("/api/process/fs") ? slow.promise : Promise.resolve(answer({ config, revision })); };
    $("pr-f-browse").click();
    expect($("prPick").hidden).toBe(false);
    $("prPickPath").dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect($("prPick").hidden).toBe(true);
    expect($("sheet").hidden).toBe(false);
    expect(document.getElementById("prForm")).toBeTruthy();
    expect(($("pr-f-program") as HTMLInputElement).value).toBe("C:\\tools\\cc.exe");
    expect(($("pr-f-name") as HTMLInputElement).value).toBe("demo");
    // The hidden guard still refuses the listing the layer's close invalidated.
    slow.resolve(answer({ dir: "C:\\tools", sep: "\\", parent: "C:\\", entries: [{ name: "cc.exe", dir: false, exe: true }] }));
    await drain();
    expect($("prPickList").textContent).not.toContain("cc.exe");
    // With the picker layer shut, Escape closes the form as before.
    $("pr-f-program").dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect($("sheet").hidden).toBe(true);
    expect(document.getElementById("prForm")).toBeNull();
  });

  it("does not send a cancelled save after its config read answers late", async () => {
    await editor.openForm(null);
    value("name", "old");
    value("program", "node");
    const slow = deferred<Response>();
    respond = () => slow.promise;
    $("prFormSave").click();
    $("prFormCancel").click();
    respond = () => Promise.resolve(answer({ config, revision }));
    await editor.openForm(null);
    value("name", "replacement");
    slow.resolve(answer({ config, revision }));
    await drain();
    expect(puts()).toHaveLength(0);
    expect(($("pr-f-name") as HTMLInputElement).value).toBe("replacement");
  });

  it("does not close or reload a replacement sheet when an old save succeeds", async () => {
    const onSaved = vi.fn(async () => {});
    await editor.openForm(null, onSaved);
    value("name", "old");
    value("program", "node");
    const slow = deferred<Response>();
    respond = (request) => request.method === "PUT" ? slow.promise : Promise.resolve(answer({ config, revision }));
    await save();
    expect(puts()).toHaveLength(1);
    await editor.openForm(null);
    value("name", "replacement");
    slow.resolve(answer({}));
    await drain();
    expect($("sheet").hidden).toBe(false);
    expect(($("pr-f-name") as HTMLInputElement).value).toBe("replacement");
    expect(onSaved).not.toHaveBeenCalled();
  });

  it("does not paint an old save error into a replacement sheet", async () => {
    await editor.openForm(null);
    value("name", "old");
    value("program", "node");
    const slow = deferred<Response>();
    respond = (request) => request.method === "PUT" ? slow.promise : Promise.resolve(answer({ config, revision }));
    await save();
    await editor.openForm(null);
    slow.resolve(answer({ error: "old request rejected" }, 400));
    await drain();
    expect($("pr-f-err").hidden).toBe(true);
    expect($("pr-f-err").textContent).toBe("");
  });

  it("allows a replacement editor to save independently of an old outstanding PUT", async () => {
    await editor.openForm(null);
    value("name", "old");
    value("program", "node");
    const slow = deferred<Response>();
    respond = (request) => request.method === "PUT" ? slow.promise : Promise.resolve(answer({ config, revision }));
    await save();
    await editor.openForm(null);
    value("name", "replacement");
    value("program", "node");
    const onNew = vi.fn();
    respond = (request) => { if (request.method === "PUT") onNew(); return Promise.resolve(answer({ config, revision })); };
    await save();
    expect(onNew).toHaveBeenCalledTimes(1);
    slow.resolve(answer({}));
    await drain();
  });

  it("does not reopen after another feature replaces the pending editor sheet", async () => {
    const slow = deferred<Response>();
    respond = () => slow.promise;
    const pending = editor.openForm(null);
    const other = layers.sheet({ title: "Another feature", body: "Keep this sheet", foot: [] });
    layers.showSheet(other);
    slow.resolve(answer({ config, revision }));
    await pending;
    expect($("sheet").firstElementChild).toBe(other);
    expect($("sheet").textContent).toContain("Keep this sheet");
  });
  it("Cancel and Escape invalidate config reads before a form has loaded", async () => {
    for (const key of ["Cancel", "Escape"]) {
      const slow = deferred<Response>();
      respond = () => slow.promise;
      const pending = editor.openForm(null);
      const signal = requests[requests.length - 1].signal;
      if (key === "Cancel") $("prFormCancel").click();
      else $("prFormCancel").dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
      expect(signal?.aborted).toBe(true);
      slow.resolve(answer({ config, revision }));
      await pending;
      expect($("sheet").hidden).toBe(true);
      expect(document.getElementById("prForm")).toBeNull();
    }
  });

  it("backdrop cancellation invalidates the pending config answer", async () => {
    const slow = deferred<Response>();
    respond = () => slow.promise;
    const pending = editor.openForm(null);
    $("sheet").click();
    slow.resolve(answer({ config, revision }));
    await pending;
    expect($("sheet").hidden).toBe(true);
    expect(document.getElementById("prForm")).toBeNull();
  });

  it("dispose invalidates pending work without closing another feature's sheet", async () => {
    const slow = deferred<Response>();
    respond = () => slow.promise;
    const pending = editor.openForm(null);
    const signal = requests[0].signal;
    const other = layers.sheet({ title: "Other", body: "Keep this", foot: [] });
    layers.showSheet(other);
    editor.disposeForm();
    expect(signal?.aborted).toBe(true);
    slow.resolve(answer({ config, revision }));
    await pending;
    expect($("sheet").hidden).toBe(false);
    expect($("sheet").firstElementChild).toBe(other);
  });

  it("a target removed while editing cannot be recreated by Save", async () => {
    config = { apps: [app()] };
    await editor.openForm("demo");
    config = { apps: [] };
    revision++;
    await save();
    expect(puts()).toHaveLength(0);
    expect($("pr-f-err").hidden).toBe(false);
  });

  it("a same-name app added after the first PUT conflict prevents the Add retry", async () => {
    await editor.openForm(null);
    value("name", "demo");
    value("program", "my-app");
    respond = (request) => {
      if (request.method === "PUT") {
        config = { apps: [app("demo", "concurrent-add")] };
        revision++;
        return Promise.resolve(answer({}, 409));
      }
      return Promise.resolve(answer({ config, revision }));
    };
    await save();
    expect(puts()).toHaveLength(1);
    expect($("pr-f-err").textContent).toContain("already exists");
  });

  it("a second revision conflict stops after one retry and keeps the form open", async () => {
    config = { apps: [app()] };
    await editor.openForm("demo");
    respond = (request) => Promise.resolve(answer(request.method === "PUT" ? {} : { config, revision }, request.method === "PUT" ? 409 : 200));
    await save();
    expect(puts()).toHaveLength(2);
    expect($("sheet").hidden).toBe(false);
    expect($("pr-f-err").hidden).toBe(false);
    expect(($("prFormSave") as HTMLButtonElement).disabled).toBe(false);
  });

  it("compares object values rather than key insertion order and keeps app order", async () => {
    config = { apps: [app("first"), { ...app(), env: { A: "one", B: "two" } }, app("last")] };
    await editor.openForm("demo");
    config = { apps: [app("first"), { start: { program: "node" }, env: { B: "two", A: "one" }, name: "demo" }, app("last")] };
    value("program", "edited");
    await save();
    expect(puts()).toHaveLength(1);
    const body = JSON.parse(puts()[0].body || "{}");
    expect(body.config.apps.map((a: AppEntry) => a.name)).toEqual(["first", "demo", "last"]);
  });

  it("warnings keep the saved sheet and refresh once; a second save uses its new baseline", async () => {
    const onSaved = vi.fn(async () => {});
    await editor.openForm(null, onSaved);
    value("name", "demo");
    value("program", "node");
    respond = (request) => {
      if (request.method === "PUT") {
        config = JSON.parse(request.body || "{}").config;
        revision++;
        return Promise.resolve(answer({ config, revision, warnings: ["App is still running"] }));
      }
      return Promise.resolve(answer({ config, revision }));
    };
    await save();
    expect($("sheet").hidden).toBe(false);
    expect($("pr-f-warn").textContent).toBe("App is still running");
    expect(onSaved).toHaveBeenCalledTimes(1);
    expect(($("pr-f-name") as HTMLInputElement).readOnly).toBe(true);
    value("program", "edited");
    await save();
    expect(puts()).toHaveLength(2);
    expect(config.apps[0].start.program).toBe("edited");
    expect(onSaved).toHaveBeenCalledTimes(2);
  });

  it("deletion refreshes its captured callback after a revision-checked PUT", async () => {
    config = { apps: [app(), app("keep")] };
    const onSaved = vi.fn(async () => {});
    await editor.deleteApp("demo", onSaved);
    expect(puts()).toHaveLength(1);
    const body = JSON.parse(puts()[0].body || "{}");
    expect(body.revision).toBe(7);
    expect(body.config.apps).toEqual([app("keep")]);
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it("a late deletion config read does not write after disposal", async () => {
    const slow = deferred<Response>();
    respond = () => slow.promise;
    const pending = editor.deleteApp("demo");
    const signal = requests[0].signal;
    editor.disposeForm();
    slow.resolve(answer({ config: { apps: [app()] }, revision }));
    await pending;
    expect(signal?.aborted).toBe(true);
    expect(puts()).toHaveLength(0);
  });

  it("a late deletion response cannot refresh or toast over a new editor", async () => {
    config = { apps: [app()] };
    const slow = deferred<Response>();
    const onSaved = vi.fn(async () => {});
    respond = (request) => request.method === "PUT" ? slow.promise : Promise.resolve(answer({ config, revision }));
    const pending = editor.deleteApp("demo", onSaved);
    await drain();
    expect(puts()).toHaveLength(1);
    await editor.openForm(null);
    slow.resolve(answer({}));
    await pending;
    expect(onSaved).not.toHaveBeenCalled();
    expect($("sheet").hidden).toBe(false);
    expect($("toast").textContent).toBe("");
  });

  it("a deletion conflict never removes a concurrently edited target on retry", async () => {
    config = { apps: [app()] };
    const onSaved = vi.fn(async () => {});
    respond = (request) => Promise.resolve(answer(request.method === "PUT" ? {} : { config, revision }, request.method === "PUT" ? 409 : 200));
    await editor.deleteApp("demo", onSaved);
    expect(puts()).toHaveLength(1);
    expect(onSaved).not.toHaveBeenCalled();
  });
});
