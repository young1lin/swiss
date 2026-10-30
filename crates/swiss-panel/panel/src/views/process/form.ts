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

/* The app editor owns configuration revisions and its inline picker. */
import { $, api, toast } from "../../util.js";
import { h } from "../../h.js";
import { tr } from "../../i18n.js";
import { btn } from "../../ui/button.js";
import { note } from "../../ui/page.js";
import { closeSheet, sheet, showSheet } from "../../ui/sheet.js";
import { checkField, field, form, formCap, formFold, hint, pair } from "../../ui/form.js";
import { envToObj, envToText } from "../../fields.js";
import { pickPanel, pickerRow } from "../../ui/pick.js";
import type { AppEntry } from "./types.js";

interface ConfigAnswer { config?: { apps?: AppEntry[]; [key: string]: unknown }; revision?: number }
interface SaveAnswer { error?: string; warnings?: unknown; config?: { apps?: AppEntry[] } }
interface PickerEntry { name: string; dir: boolean; exe: boolean }
interface PickerAnswer { dir?: string; sep?: string; parent?: string | null; entries?: PickerEntry[]; truncated?: boolean }
/* What an argv textarea was filled with: the exact text plus the array it
 * was joined from, captured so a field the user never touched can hand its
 * original argv back whole (see formArgv). */
interface ArgFill { text: string; argv: string[] }

interface EditorSession {
  host: HTMLElement;
  root: HTMLElement;
  controller: AbortController;
  observer: MutationObserver;
  original: AppEntry | null;
  baseline: string | null;
  saving: boolean;
  picker: { parent: string | null; sep: string; sequence: number };
  filled: { args: ArgFill | null; stopArgs: ArgFill | null };
  onSaved?: () => Promise<void>;
}

let active: EditorSession | null = null;
let deletion: AbortController | null = null;

/* A session owns its exact dialog, never an id in whichever sheet is open now.
 * Abort is best-effort; ownership checks still protect against late responses. */
function owns(session: EditorSession): boolean {
  return active === session && session.host.isConnected && !session.host.hidden
    && session.root.parentElement === session.host && !session.controller.signal.aborted;
}

export function disposeForm(): void {
  if (active) {
    active.observer.disconnect();
    active.controller.abort();
    active = null;
  }
  if (deletion) { deletion.abort(); deletion = null; }
}

function cancel(session: EditorSession): void {
  if (!owns(session)) return;
  disposeForm();
  closeSheet();
}

function control<T extends HTMLElement = HTMLElement>(session: EditorSession, id: string): T {
  const node = session.root.querySelector<T>("#" + id);
  if (!node) throw new Error("missing process editor control: " + id);
  return node;
}

async function request<T>(session: EditorSession, path: string, init?: RequestInit, accepts = (): boolean => owns(session)): Promise<{ response: Response; body: T } | null> {
  if (!accepts()) return null;
  try {
    const response = await api(path, { ...init, signal: session.controller.signal });
    if (!accepts()) return null;
    const body = await response.json().catch(() => ({})) as T;
    return accepts() ? { response, body } : null;
  } catch (e) {
    if (accepts()) showFormErr(session, tr("util.requestFailedGatewayRunning"));
    return null;
  }
}

async function readConfig(session: EditorSession): Promise<ConfigAnswer | null> {
  const result = await request<ConfigAnswer & SaveAnswer>(session, "/api/plugins/process/config");
  if (!result) return null;
  if (!result.response.ok) {
    showFormErr(session, result.body.error || tr("util.httpN", { n: result.response.status }));
    return null;
  }
  return result.body;
}

/* Object key order is not a concurrent edit; argv order and every stored value are.
 * Capture the serialized baseline at open, not a mutable reference to the response. */
function fingerprint(app: AppEntry): string {
  return JSON.stringify(app, (_key, value: unknown): unknown => {
    if (!value || typeof value !== "object" || Array.isArray(value)) return value;
    const record = value as Record<string, unknown>;
    const sorted: Record<string, unknown> = {};
    for (const key of Object.keys(record).sort()) sorted[key] = record[key];
    return sorted;
  });
}

/* --- the executable picker -- -- -- -- -- -- -- -- -- -- */

function togglePicker(session: EditorSession): void {
  if (!owns(session)) return;
  const box = control(session, "prPick");
  if (!box.hidden) { box.hidden = true; session.picker.sequence++; return; }
  box.hidden = false;
  const typed = control<HTMLInputElement>(session, "pr-f-program").value.trim();
  const cut = Math.max(typed.lastIndexOf("\\"), typed.lastIndexOf("/"));
  void pickerLoad(session, cut >= 0 ? typed.slice(0, cut) || typed.slice(0, 1) : null);
}

async function pickerLoad(session: EditorSession, dir: string | null): Promise<void> {
  if (!owns(session)) return;
  const sequence = ++session.picker.sequence;
  const accepts = (): boolean => owns(session) && sequence === session.picker.sequence && !control(session, "prPick").hidden;
  const url = "/api/process/fs" + (dir ? "?dir=" + encodeURIComponent(dir) : "");
  const result = await request<PickerAnswer & SaveAnswer>(session, url, undefined, accepts);
  if (!result) return;
  const j = result.body;
  if (!result.response.ok) { showFormErr(session, j.error || tr("util.httpN", { n: result.response.status })); return; }
  if (j.sep === "/" || j.sep === "\\") session.picker.sep = j.sep;
  session.picker.parent = j.parent || null;
  control<HTMLInputElement>(session, "prPickPath").value = j.dir || dir || "";
  control<HTMLButtonElement>(session, "prPickUp").disabled = !session.picker.parent;
  const box = control(session, "prPickList");
  box.replaceChildren();
  const list = j.entries || [];
  if (!list.length) box.append(note(tr("process.pick.empty")));
  const shown = j.dir || dir || "";
  const sep = session.picker.sep;
  for (const entry of list) {
    const row = pickerRow(entry.dir ? "dir" : entry.exe ? "exe" : "other", entry.name, entry.exe);
    row.onclick = (): void => {
      if (!owns(session)) return;
      const path = joinPath(shown, entry.name, sep);
      if (entry.dir) { void pickerLoad(session, path); return; }
      if (entry.exe) {
        control<HTMLInputElement>(session, "pr-f-program").value = path;
        control(session, "prPick").hidden = true;
        session.picker.sequence++;
        toast(tr("process.pick.chosen", { name: entry.name }));
      }
    };
    box.append(row);
  }
  if (j.truncated) box.append(note(tr("process.pick.truncated")));
}

function joinPath(dir: string, name: string, sep: string): string {
  return dir.endsWith("\\") || dir.endsWith("/") ? dir + name : dir + sep + name;
}

/* --- the Add / Edit sheet -- -- -- -- -- -- -- */

function input(id: string, value: string, ph?: string): HTMLInputElement {
  return h("input", { type: "text", id: id, value: value, placeholder: ph }) as HTMLInputElement;
}

function area(id: string, value: string, rowsN: number): HTMLTextAreaElement {
  return h("textarea", { id: id, rows: rowsN }, value) as HTMLTextAreaElement;
}

function formSheet(session: EditorSession, app: AppEntry | null): HTMLElement {
  const a: AppEntry = app || { name: "", start: { program: "" } };
  const startArgs = a.start.args || [];
  const stop = a.stop || { program: "", args: [] };
  const stopArgs = stop.args || [];
  const args = startArgs.join("\n");
  // Capture each argv textarea's fill (session-owned copies, never a
  // reference into the opened entry): readForm reuses the source array
  // verbatim while the field still holds this exact text, which is what
  // keeps a config-authored trailing empty argument (["--pass", ""])
  // alive through an Edit+Save that never touched args (SPEC §process.panel).
  session.filled.args = { text: args, argv: startArgs.slice() };
  session.filled.stopArgs = { text: stopArgs.join("\n"), argv: stopArgs.slice() };
  return sheet({
    title: app ? tr("process.form.editTitle", { name: a.name }) : tr("process.form.addTitle"),
    sub: app ? a.name : undefined,
    body: h("div", { id: "prForm" },
      form(
        // The common case is four answers (SPEC §process.panel): what to
        // call it, what to run, with what arguments, on which ports. Image,
        // working dir, the stop command, env, log and UI live behind the fold.
        field({ label: tr("process.form.name"), required: true, control: h("input", { type: "text", id: "pr-f-name", value: a.name, placeholder: tr("process.form.nameExample"), readOnly: !!app }), hint: tr("process.form.nameHint") }),
        field({ label: tr("process.form.program"), required: true, control: input("pr-f-program", a.start.program, "node"), hint: tr("process.form.programHint"), action: btn(tr("process.pick.pick"), { id: "pr-f-browse", kind: "ghost" }) }),
        // The picker panel, not a second sheet: the house sheet is one slot, so
        // a picker sheet would blow the form away with everything typed into it
        // (SPEC §panel.ui). It lives under the field it serves.
        (() => {
          const pick = pickPanel(tr("process.pick.path"), tr("process.pick.go"), tr("process.pick.up"));
          pick.root.id = "prPick";
          pick.path.id = "prPickPath";
          pick.go.id = "prPickGo";
          pick.up.id = "prPickUp";
          pick.list.id = "prPickList";
          return h("span", null, pick.root, hint(tr("process.pick.hint")));
        })(),
        field({ label: tr("process.form.args"), control: area("pr-f-args", args, 3), hint: tr("process.form.argsHint") }),
        field({ label: tr("process.form.ports"), control: input("pr-f-ports", (a.ports || []).join(", "), "4317, 4318"), hint: tr("process.form.portsHint") }),
        checkField({ label: tr("process.form.autostart"), hint: tr("process.form.autostartHint"), control: h("input", { type: "checkbox", id: "pr-f-autostart", checked: !!a.autostart }) as HTMLInputElement }),
        formFold({ summary: tr("process.form.advanced") },
          pair(
            field({ label: tr("process.form.image"), control: input("pr-f-image", a.image || "", "node.exe"), hint: tr("process.form.imageHint") }),
            field({ label: tr("process.form.dir"), control: input("pr-f-dir", a.dir || "", "~\\src") }),
          ),
          formCap(tr("process.form.stopCap")),
          pair(
            field({ label: tr("process.form.stopProgram"), control: input("pr-f-stop-program", stop.program, "node") }),
            field({ label: tr("process.form.stopArgs"), control: area("pr-f-stop-args", session.filled.stopArgs.text, 3), hint: tr("process.form.stopArgsHint") }),
          ),
          formCap(tr("process.form.envCap")),
          field({ label: tr("process.form.env"), control: area("pr-f-env", envToText(a.env), 3), hint: tr("process.form.envHint") }),
          formCap(tr("process.form.pathsCap")),
          field({ label: tr("process.form.log"), control: input("pr-f-log", a.log || "", "~\\logs\\otel-*.log"), hint: tr("process.form.logHint") }),
          field({ label: tr("process.form.ui"), control: input("pr-f-ui", a.ui || "", "http://127.0.0.1:4317"), hint: tr("process.form.uiHint") }),
        ),
        hint("", { id: "pr-f-warn", hidden: true }),
        hint("", { id: "pr-f-err", bad: true, hidden: true, live: true }),
      )),
    foot: [
      btn(tr("process.action.cancel"), { kind: "ghost", id: "prFormCancel" }),
      h("span", { class: "grow" }),
      btn(tr("process.form.save"), { kind: "primary", id: "prFormSave" }),
    ],
  });
}

function showFormErr(session: EditorSession, text: string | null): void {
  if (!owns(session)) return;
  const el = control(session, "pr-f-err");
  el.textContent = text || "";
  el.hidden = !text;
}

function showFormWarnings(session: EditorSession, warnings: string[]): void {
  if (!owns(session)) return;
  const el = control(session, "pr-f-warn");
  el.textContent = warnings.join("\n");
  el.hidden = !warnings.length;
}

/* Read the form into an AppEntry. argv, never a string to split; ports as
 * numbers; env exactly as typed (a reference stays a reference). */
function readForm(session: EditorSession): AppEntry | string {
  const txt = (id: string): string => control<HTMLInputElement | HTMLTextAreaElement>(session, id).value;
  const name = session.original?.name || txt("pr-f-name").trim();
  const program = txt("pr-f-program").trim();
  if (!/^[a-z0-9-]{1,32}$/.test(name)) return tr("process.form.badName");
  if (!program) return tr("process.form.badProgram");
  const entry: AppEntry = { name, start: { program } };
  const args = formArgv(session, "pr-f-args", session.filled.args);
  if (typeof args === "string") return args;
  if (args.length) entry.start.args = args;
  const portsRaw = txt("pr-f-ports").split(",").map((s: string) => { return s.trim(); }).filter(Boolean);
  if (portsRaw.length) {
    const ports: number[] = [];
    for (const p of portsRaw) {
      const n = Number(p);
      if (!Number.isInteger(n) || n < 1 || n > 65535) return tr("process.form.badPort", { port: p });
      ports.push(n);
    }
    entry.ports = ports;
  }
  // The verdicts need the image a port's listener must match (the backend
  // rejects ports without one). When the form leaves it empty the program's
  // own file name IS that image - deriving it keeps the fold shut for the
  // common case instead of failing the save on a field the reader never saw.
  let image = txt("pr-f-image").trim();
  if (!image && entry.ports && entry.ports.length) {
    image = program.split(/[\\/]/).pop() || "";
  }
  if (image) {
    if (!entry.ports || !entry.ports.length) return tr("process.form.imageNeedsPorts");
    entry.image = image;
  }
  const dir = txt("pr-f-dir").trim();
  if (dir) entry.dir = dir;
  if (control<HTMLInputElement>(session, "pr-f-autostart").checked) entry.autostart = true;
  const stopProgram = txt("pr-f-stop-program").trim();
  if (stopProgram) {
    const stop: { program: string; args?: string[] } = { program: stopProgram };
    const stopArgs = formArgv(session, "pr-f-stop-args", session.filled.stopArgs);
    if (typeof stopArgs === "string") return stopArgs;
    if (stopArgs.length) stop.args = stopArgs;
    entry.stop = stop;
  }
  const env = envToObj(txt("pr-f-env"));
  if (Object.keys(env).length) entry.env = env;
  const log = txt("pr-f-log").trim();
  if (log) entry.log = log;
  const ui = txt("pr-f-ui").trim();
  if (ui) {
    if (!ui.startsWith("http://") && !ui.startsWith("https://")) return tr("process.form.badUi");
    entry.ui = ui;
  }
  return entry;
}

/* One argument per line (SPEC §process.panel): an EMPTY LINE IS AN EMPTY
 * ARGUMENT. Only the single blank line a textarea keeps after its final Enter
 * is trimmed away, so this parser runs on EDITED text only - an untouched
 * field takes formArgv's byte-equal reuse instead, which is what lets a
 * config-authored TRAILING empty argument (["--pass", ""]) survive an
 * Edit+Save that never touched args rather than be eaten by that trim. An
 * argument that itself contains a line break cannot round-trip through this
 * format, so it refuses the save (as the returned sentence) instead of
 * silently becoming two arguments. Start and stop use this exact same argv
 * contract. */
function argv(text: string): string[] | string {
  if (text.includes("\r")) return tr("process.form.badArgNewline");
  const lines = text.split("\n");
  if (lines.length && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/* A textarea still holding byte-for-byte the text it was filled with was
 * never touched: hand back the argv it was filled from, skipping the line
 * parser whose trailing-blank-line trim exists for a user's final Enter and
 * would otherwise drop a config-authored last empty argument. Any edit at
 * all falls back to the parser and its pinned trim. */
function formArgv(session: EditorSession, controlId: string, fill: ArgFill | null): string[] | string {
  const text = control<HTMLInputElement | HTMLTextAreaElement>(session, controlId).value;
  if (fill && text === fill.text) return fill.argv;
  return argv(text);
}

/* A whole-array PUT merges unrelated changes, not changes to the edited target.
 * Compare against what the user opened before EVERY PUT, including the retry. */
async function saveForm(session: EditorSession): Promise<void> {
  if (!owns(session) || session.saving) return;
  const read = readForm(session);
  if (typeof read === "string") { showFormErr(session, read); return; }
  showFormErr(session, null);
  showFormWarnings(session, []);
  session.saving = true;
  const save = control<HTMLButtonElement>(session, "prFormSave");
  save.disabled = true;
  try {
    for (let attempt = 0; attempt < 2; attempt++) {
      const latest = await readConfig(session);
      if (!latest || !owns(session)) return;
      const apps = latest.config?.apps || [];
      const target = apps.find((entry) => entry.name === read.name);
      if (!session.original && target) {
        showFormErr(session, tr("process.form.duplicateName", { name: read.name }));
        return;
      }
      if (session.original && (!target || fingerprint(target) !== session.baseline)) {
        showFormErr(session, tr("process.form.targetChanged", { name: read.name }));
        return;
      }
      const next = session.original ? apps.map((entry) => entry.name === read.name ? read : entry) : [...apps, read];
      const result = await request<SaveAnswer>(session, "/api/plugins/process/config", {
        method: "PUT",
        body: JSON.stringify({ revision: latest.revision || 0, config: { ...latest.config, apps: next } }),
      });
      if (!result || !owns(session)) return;
      const { response, body } = result;
      if (response.status === 409) {
        if (attempt === 0) continue;
        showFormErr(session, tr("process.form.configChanged"));
        return;
      }
      if (!response.ok) {
        showFormErr(session, typeof body.error === "string" ? body.error : tr("process.form.badSave"));
        return;
      }
      const isNew = !session.original;
      const saved = body.config?.apps?.find((entry) => entry.name === read.name) || read;
      session.original = saved;
      session.baseline = fingerprint(saved);
      control<HTMLInputElement>(session, "pr-f-name").value = saved.name;
      control<HTMLInputElement>(session, "pr-f-name").readOnly = true;
      toast(isNew ? tr("process.form.savedStart", { name: read.name }) : tr("process.form.saved", { name: read.name }));
      const warnings = Array.isArray(body.warnings) ? body.warnings.filter((w: unknown): w is string => typeof w === "string") : [];
      if (warnings.length) {
        showFormWarnings(session, warnings);
        await session.onSaved?.();
        return;
      }
      const onSaved = session.onSaved;
      cancel(session);
      await onSaved?.();
      return;
    }
  } catch (e) {
    if (owns(session)) showFormErr(session, tr("util.requestFailedGatewayRunning"));
  } finally {
    session.saving = false;
    if (owns(session)) save.disabled = false;
  }
}

export async function openForm(name: string | null, onSaved?: () => Promise<void>): Promise<void> {
  disposeForm();
  const cancelButton = btn(tr("process.action.cancel"), { kind: "ghost", id: "prFormCancel" });
  const pending = sheet({
    title: name ? tr("process.form.editTitle", { name }) : tr("process.form.addTitle"),
    body: [note(tr("process.form.loading"), { busy: true }), hint("", { id: "pr-f-err", bad: true, hidden: true, live: true })],
    foot: [cancelButton],
  });
  const session: EditorSession = {
    host: $("sheet"), root: pending, controller: new AbortController(), original: null,
    baseline: null, saving: false, picker: { parent: null, sep: "\\", sequence: 0 },
    filled: { args: null, stopArgs: null }, onSaved,
    observer: new MutationObserver(() => { if (active === session && !owns(session)) disposeForm(); }),
  };
  active = session;
  showSheet(pending);
  session.observer.observe(session.host, { childList: true, attributes: true, attributeFilter: ["hidden"] });
  cancelButton.onclick = (): void => cancel(session);
  wireKeys(session);
  const latest = await readConfig(session);
  if (!latest || !owns(session)) return;
  const app = name ? (latest.config?.apps || []).find((entry) => entry.name === name) || null : null;
  if (name && !app) {
    toast(tr("process.form.gone", { name }), true);
    cancel(session);
    return;
  }
  session.original = app;
  session.baseline = app ? fingerprint(app) : null;
  session.root = formSheet(session, app);
  showSheet(session.root);
  control(session, "prFormCancel").onclick = (): void => cancel(session);
  control(session, "prFormSave").onclick = (): void => { void saveForm(session); };
  control(session, "pr-f-browse").onclick = (): void => togglePicker(session);
  const pickPath = control<HTMLInputElement>(session, "prPickPath");
  control(session, "prPickGo").onclick = (): void => { void pickerLoad(session, pickPath.value.trim()); };
  pickPath.onkeydown = (ev): void => {
    if (ev.key === "Enter") { ev.preventDefault(); ev.stopPropagation(); void pickerLoad(session, pickPath.value.trim()); }
  };
  control(session, "prPickUp").onclick = (): void => { void pickerLoad(session, session.picker.parent); };
  wireKeys(session);
  control<HTMLInputElement>(session, app ? "pr-f-program" : "pr-f-name").focus();
}

function wireKeys(session: EditorSession): void {
  session.root.onkeydown = (event): void => {
    if (!owns(session)) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      // A visible picker is a LAYER over the form, not the form: Escape closes
      // that layer first and stops there - the one-layer-at-a-time rule
      // stackSheet gives its own stacked picker - so one Escape never
      // discards everything typed into the sheet underneath. Bumping the
      // sequence refuses any in-flight listing, exactly like togglePicker.
      const pick = session.root.querySelector<HTMLElement>("#prPick");
      if (pick && !pick.hidden) { pick.hidden = true; session.picker.sequence++; return; }
      cancel(session);
    }
    else if (event.key === "Enter" && event.target instanceof HTMLInputElement && session.root.querySelector("#prFormSave")) {
      event.preventDefault();
      event.stopPropagation();
      void saveForm(session);
    }
  };
}

/* Delete also captures its callback and request lifetime. It never closes a sheet;
 * leaving the page or opening another editor invalidates both reads and answers. */
export async function deleteApp(name: string, onSaved?: () => Promise<void>): Promise<void> {
  if (!confirm(tr("process.confirm.delete", { name }))) return;
  deletion?.abort();
  const controller = new AbortController();
  deletion = controller;
  const host = $("sheet");
  const root = host.firstElementChild;
  const hidden = host.hidden;
  const accepts = (): boolean => deletion === controller && !controller.signal.aborted
    && host.isConnected && host.firstElementChild === root && host.hidden === hidden;
  try {
    const get = await api("/api/plugins/process/config", { signal: controller.signal });
    if (!accepts()) return;
    const latest = await get.json().catch(() => ({})) as ConfigAnswer & SaveAnswer;
    if (!accepts()) return;
    if (!get.ok) { toast(latest.error || tr("util.httpN", { n: get.status }), true); return; }
    const apps = latest.config?.apps || [];
    if (!apps.some((entry) => entry.name === name)) { toast(tr("process.form.gone", { name }), true); return; }
    const put = await api("/api/plugins/process/config", {
      method: "PUT", signal: controller.signal,
      body: JSON.stringify({ revision: latest.revision || 0, config: { ...latest.config, apps: apps.filter((entry) => entry.name !== name) } }),
    });
    if (!accepts()) return;
    const result = await put.json().catch(() => ({})) as SaveAnswer;
    if (!accepts()) return;
    if (!put.ok) {
      toast(put.status === 409 ? tr("process.form.configChanged") : result.error || tr("process.form.badSave"), true);
      return;
    }
    toast(tr("process.deleted", { name }));
    await onSaved?.();
  } catch (e) {
    if (accepts()) toast(tr("util.requestFailedGatewayRunning"), true);
  } finally {
    if (deletion === controller) deletion = null;
  }
}
