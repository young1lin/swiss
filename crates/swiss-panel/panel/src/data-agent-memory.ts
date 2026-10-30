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

/* ================================================================================================
   The Data agent's Memory (SPEC §data.agent Memory): what earlier conversations on one
   connection saved about where its data lives and how to reach it - the live room's bound user
   in live_room, then that user by id - so the next conversation walks the route instead of
   finding it again. Not swiss memory: a folder of markdown per connection under the home
   (llm/db-memory/<connection>/), one note a file, MEMORY.md the index swiss rewrites from them.
   A new conversation's opening carries the index; the agent reads, saves and deletes notes
   through its tools.

   Here the reader sees the same notes: the list (GET /api/llm/db/memory), one note whole
   (GET /api/llm/db/memory/note), a new one or an edit in a sheet (POST /api/llm/db/memory/save)
   and a delete (POST /api/llm/db/memory/delete). The view stands in the pane's column the way
   History does; when the agent saves or deletes a note (a db frame of kind memory) the view in
   front reads again (dbMemoryRefresh).
   ================================================================================================ */
import { fill, h } from "./h.js";
import { tr } from "./i18n.js";
import { targetEl, toast } from "./util.js";
import { btn, closeSheet, field, flow, formActions, formCap, hint, markdownNode, note, relTime, sheet, showSheet } from "./ui/index.js";
import type { FlowStep } from "./ui/index.js";
import { call } from "./views/llm-core.js";

/** One note as the index lists it. */
interface MemoryHead { name: string; description: string; updated: string; chars: number }

/** One note whole. */
interface MemoryNote extends MemoryHead { body: string }

/** The server's bounds (swiss-plugins llm_db_memory): the box takes no more than it keeps. */
const NAME_MAX = 64;
const DESCRIPTION_MAX = 200;
const BODY_MAX = 8000;

/** Each view's read-again, by its node: a memory frame reaches the one in front. */
const reloads = new WeakMap<HTMLElement, () => void>();

const memoryPath = (conn: string, name?: string): string =>
  "/api/llm/db/memory" + (name ? "/note" : "") + "?conn=" + encodeURIComponent(conn) + (name ? "&name=" + encodeURIComponent(name) : "");

/** A saved day, as relTime reads it; the raw words when they do not parse. */
function savedAt(updated: string): string {
  const at = Date.parse(updated);
  return Number.isNaN(at) ? updated : relTime(at);
}

/** One note in a quiet line: its name, what it says it holds, when it was saved. */
function noteStep(n: MemoryHead): FlowStep {
  return {
    id: n.name,
    lead: "file",
    title: n.name,
    mono: true,
    summary: n.description,
    meta: savedAt(n.updated),
    data: { dbmem: n.name },
  };
}

/** The connection's Memory: its notes, one opened whole, and the way to add, edit or delete
 *  one. `onBack` takes the column back to the conversation. */
function dbMemoryView(conn: string, onBack: () => void): HTMLElement {
  const view = h("div", { id: "dbAgentMem" });
  // The note opened whole, or null while the list stands.
  let open: string | null = null;
  // A newer paint makes an older read's answer stale.
  let gen = 0;

  const showList = async (): Promise<void> => {
    open = null;
    const mine = ++gen;
    const back = btn(tr("dataAgent.back"), { icon: "chevron-left", kind: "ghost", data: { dbmemback: "" } });
    const add = btn(tr("dataAgent.memNew"), { icon: "plus", kind: "ghost", data: { dbmemnew: "" } });
    const about = hint(tr("dataAgent.memHint"));
    const list = h("div", { data: { dbmemlist: "" } }, note(tr("dataAgent.loading"), { busy: true }));
    back.onclick = onBack;
    add.onclick = (): void => { editSheet(conn, null, (name) => { void showNote(name); }); };
    list.onclick = (e): void => {
      const step = targetEl(e)?.closest<HTMLElement>("[data-dbmem]");
      if (step?.dataset.dbmem) void showNote(step.dataset.dbmem);
    };
    fill(view, formActions(back, h("span", { class: "grow" }), add, formCap(tr("dataAgent.memory"))), about, list);
    const r = await call<{ dir: string; notes: MemoryHead[] }>("GET", memoryPath(conn));
    if (mine !== gen) return;
    if (!r.data) { fill(list, hint(r.err, { bad: true })); return; }
    fill(about, tr("dataAgent.memHint"), " ", tr("dataAgent.memDir"), " ", h("code", { title: r.data.dir }, r.data.dir));
    fill(list, r.data.notes.length ? flow(r.data.notes.map(noteStep)) : hint(tr("dataAgent.memEmpty")));
  };

  // `gone`: a read again after the agent's change - a note no longer there goes back to the list.
  const showNote = async (name: string, gone = false): Promise<void> => {
    open = name;
    const mine = ++gen;
    const back = btn(tr("dataAgent.back"), { icon: "chevron-left", kind: "ghost", data: { dbmemlistback: "" } });
    const edit = btn(tr("dataAgent.memEdit"), { kind: "ghost", disabled: true, data: { dbmemedit: name } });
    const drop = btn(tr("dataAgent.memDelete"), { kind: "ghost", disabled: true, data: { dbmemdelete: name } });
    const said = h("div", { data: { dbmemnote: name } }, note(tr("dataAgent.loading"), { busy: true }));
    back.onclick = (): void => { void showList(); };
    fill(view, formActions(back, h("code", { title: name }, name), h("span", { class: "grow" }), edit, drop), said);
    const r = await call<MemoryNote>("GET", memoryPath(conn, name));
    if (mine !== gen) return;
    if (!r.data) {
      if (gone) void showList();
      else fill(said, hint(r.err, { bad: true }));
      return;
    }
    const got = r.data;
    edit.disabled = false;
    drop.disabled = false;
    edit.onclick = (): void => { editSheet(conn, got, (saved) => { void showNote(saved); }); };
    drop.onclick = (): void => { void forget(name); };
    fill(said, hint([got.description, " · ", tr("dataAgent.memSaved", { when: savedAt(got.updated) })]), markdownNode(got.body));
  };

  const forget = async (name: string): Promise<void> => {
    if (!confirm(tr("dataAgent.memDeleteConfirm", { name }))) return;
    const r = await call<{ deleted: boolean }>("POST", "/api/llm/db/memory/delete", { conn, name });
    if (!r.data) { toast(r.err, true); return; }
    if (open === name) void showList();
  };

  reloads.set(view, (): void => { void (open ? showNote(open, true) : showList()); });
  void showList();
  return view;
}

/** The sheet that adds a note (`was` null) or edits one: its name only when new - an edit
 *  keeps the file - then its one-line description and its markdown body. `done` hears the
 *  saved name. */
function editSheet(conn: string, was: MemoryNote | null, done: (name: string) => void): void {
  const name = h("input", { id: "dbMemName", autocomplete: "off", spellcheck: false, maxLength: NAME_MAX, placeholder: tr("dataAgent.memNamePh") });
  const desc = h("input", { id: "dbMemDesc", autocomplete: "off", maxLength: DESCRIPTION_MAX, value: was ? was.description : "", placeholder: tr("dataAgent.memDescPh") });
  const body = h("textarea", { id: "dbMemBody", spellcheck: false, maxLength: BODY_MAX, placeholder: tr("dataAgent.memBodyPh"), style: "min-height:16em;width:100%" });
  body.value = was ? was.body : "";
  const why = hint("", { id: "dbMemWhy", bad: true, hidden: true });
  const save = btn(tr("dataAgent.memSave"), { kind: "primary", id: "dbMemSave" });
  const cancel = btn(tr("dataAgent.memCancel"), { id: "dbMemCancel" });
  cancel.onclick = closeSheet;
  save.onclick = async (): Promise<void> => {
    save.disabled = true;
    const r = await call<{ name: string; replaced: boolean }>("POST", "/api/llm/db/memory/save",
      { conn, name: was ? was.name : name.value, description: desc.value, body: body.value });
    save.disabled = false;
    if (!r.data) { fill(why, r.err); why.hidden = false; return; }
    closeSheet();
    done(r.data.name);
  };
  showSheet(sheet({
    title: tr(was ? "dataAgent.memEditTitle" : "dataAgent.memNewTitle"),
    sub: was ? was.name : conn,
    body: [
      was ? null : field({ label: tr("dataAgent.memName"), control: name, required: true, hint: tr("dataAgent.memNameHint") }),
      field({ label: tr("dataAgent.memDesc"), control: desc, required: true }),
      field({ label: tr("dataAgent.memBody"), control: body, hint: tr("dataAgent.memBodyHint") }),
      why,
    ],
    foot: [h("span", { class: "grow" }), cancel, save],
  }));
  (was ? desc : name).focus();
}

/** A memory frame: the agent saved or deleted a note. The view in front, when it is one, reads
 *  again - the open note too, back to the list when it is gone. */
function dbMemoryRefresh(node: HTMLElement | null): void {
  if (node) reloads.get(node)?.();
}

export { dbMemoryRefresh, dbMemoryView };
