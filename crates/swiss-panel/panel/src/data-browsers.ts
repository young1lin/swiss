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

import type { ApiDbConnectionRow, ApiDbRedisKeysResponse, ApiDbRedisValue } from "./types/api.js";
import type { DbRedisEdits, DbRedisTypeCfg } from "./types/state.js";
import { $, apiJson, dbReqGuard, el, emptyHtml, errText, esc, icon, toast } from "./util.js";
import { closeSheet } from "./add-sheet.js";
import { renderDbFilters } from "./data-filters.js";
import { renderDbGrid } from "./data-grid.js";
import { renderDbBar } from "./data-sql.js";
// Cycle with data-edit.js (it reads dbIsRedis/dbLoadKeys from here): function declarations,
// runtime-only use — the same shape as the data-sql import above.
import { dbTypedConfirm } from "./data-edit.js";
import { renderDbTables } from "./data-view.js";
import { popupMenu } from "./menu.js";
import { dbView } from "./db-state.js";

/* --- redis key browser -------------------------------------------------------------------------- */
/* A redis connection in the picker swaps the table list for a SCAN-paged key list, and the
   right pane for a type-aware value view (docs/22 W3.3): a string edits in place (one SET)
   and the container types render as typed tables whose edits buffer locally and Commit as
   ONE guarded pipeline round trip — HSET/HDEL for hashes, ZADD/ZREM for zsets, LSET/RPUSH
   for lists, SADD/SREM for sets. Every buffered command is field-addressed, so no MULTI: a
   refused or failed command stops the run and the operator re-commits the rest. */

function dbIsRedis(): boolean {
  const d = dbView();
  const c = d.conns.find((x: ApiDbConnectionRow): boolean => { return x.name === d.conn; });
  return !!c && c.dialect === "redis";
}

// One SCAN page in flight (docs/22 closeout audit): More re-entering while a page loads
// re-sent the SAME cursor and appended that page twice. A reset (grep, type filter, refresh)
// still goes through — it restarts the walk from cursor 0 and supersedes.
let dbKeysLoading = false;

// One /keys request chain (docs/22 closeout B3): a slow More answer landing after a reset
// spliced its old-cursor page into the FRESH walk and dragged the cursor backward, so later
// pages shifted under the user (a key quietly went missing from the list). Responses apply
// only while their token is still the newest request.
const dbKeysReq = dbReqGuard();

async function dbLoadKeys(reset: boolean | undefined): Promise<void> {
  const d = dbView();
  if (!d.conn) return;
  if (!reset && dbKeysLoading) return; // the More double-click: the page is already on its way
  if (reset) { d.redis = null; d.redisKey = null; }
  let q = "/api/db/" + encodeURIComponent(d.conn) + "/keys?count=200";
  if (d.grep) q += "&pattern=" + encodeURIComponent(d.grep);
  if (d.redisType) q += "&type=" + encodeURIComponent(d.redisType);
  if (d.redis && d.redis.cursor && d.redis.cursor !== "0") q += "&cursor=" + encodeURIComponent(d.redis.cursor);
  const token = dbKeysReq.issue();
  let j: ApiDbRedisKeysResponse | null;
  dbKeysLoading = true;
  try {
    j = await apiJson<ApiDbRedisKeysResponse>(q);
  } finally {
    dbKeysLoading = false;
  }
  if (!dbKeysReq.accepts(token)) return; // superseded: a newer walk owns the list
  if (!j) {
    // docs/22 closeout B1: apiJson already toasted the server's own text, but a transient
    // toast over a list that still says "no keys" reads as an empty keyspace. Mark the
    // failure so the list paints it (renderDbTables), keeping the last good page.
    d.redisError = true;
    renderDbTables();
    return;
  }
  d.redisError = false;
  d.redis = {
    keys: (d.redis && !reset ? d.redis?.keys : []).concat(j.keys || []),
    cursor: j.cursor,
    done: !!j.done,
    total: j.total,
  };
  renderDbTables();
  renderDbFilters(); // the shown/keyspace readout rides on the pattern bar
}

// One /key request chain: a slow answer for the key the user just left must be dropped,
// or it would flash the PREVIOUS key's value into the pane (docs/22 closeout audit).
const dbValueReq = dbReqGuard();

async function dbLoadRedisValue(key: string): Promise<void> {
  const d = dbView();
  // A fresh key selection is a navigation: drop the command result that owned the pane,
  // or the grid guard would keep rendering it and the value would never show.
  d.sqlResult = null;
  d.sqlResults = null; d.sqlTab = 0; // docs/22 W4.3: key navigation closes every result tab
  d.sqlBusy = false;
  d.redisKey = key;
  d.redisValue = null; // drop the previous key's value — never flash stale data
  d.redisEdits = null; // and its buffered edits — a different key cannot adopt them
  renderDbGrid();
  const token = dbValueReq.issue();
  const j = await apiJson<ApiDbRedisValue>("/api/db/" + encodeURIComponent(d.conn!) + "/key?key=" + encodeURIComponent(key));
  if (!dbValueReq.accepts(token)) return; // superseded: a newer key owns the pane
  if (!j) { d.redisKey = null; renderDbGrid(); return; }
  d.redisValue = j;
  renderDbGrid();
}

/* --- the typed value view (docs/22 W3.3) --------------------------------------------------------- */

/* The plan for one container type. `cols` names the columns, `edit` the cells a double-click
   edits on an existing row, `ins` the cells a buffered insert carries, `thing` the word the
   add button and the refusals use. `deletable` is false only for list: redis has no command
   that removes one item by index without rewriting the tail, so the buffer offers nothing it
   cannot honor (dbgate's list changeset stops at LSET/RPUSH for the same reason). */
const DB_REDIS_TYPES: Record<string, DbRedisTypeCfg> = {
  hash: { cols: ["field", "value"], edit: ["value"], ins: ["field", "value"], thing: "field", deletable: true, add: "+ Field" },
  zset: { cols: ["member", "score"], edit: ["score"], ins: ["member", "score"], thing: "member", deletable: true, add: "+ Member" },
  list: { cols: ["index", "value"], edit: ["value"], ins: ["value"], thing: "item", deletable: false, add: "+ Row" },
  set: { cols: ["member"], edit: [], ins: ["member"], thing: "member", deletable: true, add: "+ Member" },
};

/** A zset score is a number or redis' own inf spellings — anything else refuses Commit
 *  rather than shipping a command the server would bounce. Pure. */
function dbRedisValidScore(s: string): boolean {
  return /^-?(\d+\.?\d*|\.\d+)$/.test(s) || s === "inf" || s === "-inf";
}

/** The buffer for the key in view, rebuilt when the key or its type changed. Null when the
 *  view holds no editable container. */
function dbRedisEdits(): DbRedisEdits | null {
  const d = dbView();
  const v = d.redisValue;
  if (!d.redisKey || !v || !DB_REDIS_TYPES[v.type]) return null;
  if (!d.redisEdits || d.redisEdits.key !== d.redisKey || d.redisEdits.type !== v.type) {
    d.redisEdits = { key: d.redisKey!, type: v.type, updates: {}, deletes: {}, inserts: [] };
  }
  return d.redisEdits;
}

/** How many changes the bar counts. Pure over the buffer. */
function dbRedisPendingCount(): number {
  const b = dbView().redisEdits;
  if (!b) return 0;
  return Object.keys(b.updates).length + Object.keys(b.deletes).length + b.inserts.length;
}

/** Fold the buffered edits into the exact command list Commit posts — updates, inserts, then
 *  deletes, the order dbgate's ChangeSetRedis uses, so an author comparing the two reads the
 *  same script. Scores and list indexes go as NUMBERS (the wire form); a blank address or a
 *  bad score throws so Commit refuses instead of silently dropping a half-filled row. Pure:
 *  the bar's preview and the POST body are both built from this list, so what the operator
 *  reads is exactly what runs. */
function dbRedisCommands(key: string, type: string, buf: { updates: Record<string, unknown>; deletes: Record<string, unknown>; inserts: Record<string, unknown>[] }): { verb: string; args: unknown[] }[] {
  // A zset score goes on the wire as a number — except redis' own inf spellings, which JSON
  // cannot number and travel as the strings ZADD accepts verbatim.
  const scoreArg = (s: string): string | number => { return s === "inf" || s === "-inf" ? s : Number(s); };
  const addrOf: Record<string, string> = { hash: "field", zset: "member", set: "member" };
  const addr = addrOf[type];
  let i: number, ins: Record<string, unknown>;
  for (i = 0; i < buf.inserts.length; i++) {
    ins = buf.inserts[i];
    if (addr && !String(ins[addr] == null ? "" : ins[addr]).trim()) {
      throw new Error("a new " + addr + " needs a name");
    }
    if (type === "zset" && !dbRedisValidScore(String(ins.score == null ? "" : ins.score))) {
      throw new Error('not a valid score for "' + ins.member + '": use a number, or inf / -inf');
    }
  }
  if (type === "zset") {
    Object.keys(buf.updates).forEach((m: string): void => {
      if (!dbRedisValidScore(String(buf.updates[m]))) {
        throw new Error('not a valid score for "' + m + '": use a number, or inf / -inf');
      }
    });
  }
  const out: { verb: string; args: unknown[] }[] = [];
  const push = (verb: string, args: unknown[]): void => { out.push({ verb: verb, args: args }); };
  Object.keys(buf.updates).forEach((a: string): void => {
    if (type === "hash") push("HSET", [key, a, buf.updates[a]]);
    else if (type === "zset") push("ZADD", [key, scoreArg(buf.updates[a] as string), a]);
    else if (type === "list") push("LSET", [key, Number(a), buf.updates[a]]);
  });
  for (i = 0; i < buf.inserts.length; i++) {
    ins = buf.inserts[i];
    if (type === "hash") push("HSET", [key, ins.field, ins.value]);
    else if (type === "zset") push("ZADD", [key, scoreArg(ins.score as string), ins.member]);
    else if (type === "list") push("RPUSH", [key, ins.value]);
    else if (type === "set") push("SADD", [key, ins.member]);
  }
  Object.keys(buf.deletes).forEach((a) => {
    if (type === "hash") push("HDEL", [key, a]);
    else if (type === "zset") push("ZREM", [key, a]);
    else if (type === "set") push("SREM", [key, a]);
  });
  return out;
}

/** One command as the preview shows it — dbgate's convertRedisCallListToScript shape: verb,
 *  then args with STRINGS double-quoted (inner quotes escaped) and numbers plain. Pure. */
function dbRedisCommandText(cmd: { verb: string; args: unknown[] }): string {
  return cmd.verb + " " + cmd.args.map((a: unknown): string => {
    return typeof a === "number" ? String(a) : '"' + String(a).replace(/"/g, '\\"') + '"';
  }).join(" ");
}

/** One row of the typed table in a uniform shape: `addr` is what commands address (field /
 *  member / index string), `cells` the per-column text with buffered edits applied, the
 *  flags the pending state the row paints. Pure. */
function dbRedisEntries(v: ApiDbRedisValue, buf: DbRedisEdits): { addr: string; deleted: boolean; updated: boolean; cells: Record<string, unknown> }[] {
  const out: { addr: string; deleted: boolean; updated: boolean; cells: Record<string, unknown> }[] = [];
  const val = v.value as Record<string, unknown> || {};
  if (v.type === "hash" || v.type === "zset") {
    Object.keys(val).forEach((a: string): void => {
      const upd = buf.updates[a];
      out.push({
        addr: a,
        deleted: !!buf.deletes[a],
        updated: upd != null,
        cells: v.type === "zset"
          ? { member: a, score: upd != null ? String(upd) : String(val[a]) }
          : { field: a, value: upd != null ? upd : String(val[a]) },
      });
    });
  } else if (v.type === "list") {
    (v.value as unknown[] || []).forEach((item: unknown, i: number): void => {
      const a = String(i);
      const upd = buf.updates[a];
      out.push({ addr: a, deleted: false, updated: upd != null, cells: { index: a, value: upd != null ? upd : String(item) } });
    });
  } else if (v.type === "set") {
    (v.value as unknown[] || []).forEach((m: unknown): void => {
      out.push({ addr: String(m), deleted: !!buf.deletes[m as string], updated: false, cells: { member: String(m) } });
    });
  }
  return out;
}

function dbRenderRedisValue(wrap: HTMLElement): void {
  const d = dbView();
  if (!d.redisKey) {
    // The shared empty state (docs/18 V7).
    wrap.innerHTML = emptyHtml({ icon: "database", title: "Select a key", hint: "Pick a key on the left to view its value." });
    return;
  }
  const v = d.redisValue;
  if (!v) { wrap.appendChild(el("div", "db-hint", "Loading " + d.redisKey + "…")); return; }
  const meta = el("div", "db-detail-meta");
  meta.appendChild(document.createTextNode(v.key + " · " + v.type + " · "));
  meta.appendChild(dbRedisTtl(v));
  if (v.length != null) meta.appendChild(document.createTextNode(" · " + v.length + " entries"));
  if (v.truncated) meta.appendChild(document.createTextNode(" · truncated"));
  meta.appendChild(el("span", "grow"));
  const cfg = DB_REDIS_TYPES[v.type];
  if (cfg) {
    // One add action per view; it buffers a row, never touches redis directly.
    const add = el("button", "btn", cfg.add) as HTMLButtonElement;
    add.title = "Buffer a new " + cfg.thing + " — applied only on Commit";
    add.onclick = (): void => {
      const b = dbRedisEdits();
      if (!b) return;
      const ins: Record<string, unknown> = {};
      cfg.ins.forEach((c) => { ins[c] = c === "score" ? "0" : ""; });
      b.inserts.push(ins);
      renderDbGrid();
      renderDbBar();
    };
    meta.appendChild(add);
  }
  // docs/22 W1.3: the key's own actions ride the value header. Rename takes a one-input
  // sheet, Delete is last and red and demands the key name typed back — the same confirm
  // vocabulary as the table DDL ops. Everything runs through the guarded /command console.
  const more = el("button", "btn icon") as HTMLButtonElement;
  more.type = "button";
  more.innerHTML = icon("ellipsis");
  more.title = "Rename or delete this key";
  // stopPropagation or the document-level click closer (connect.js) eats the menu the same
  // click opened — every other popupMenu trigger does the same.
  more.onclick = (e: MouseEvent): void => { e.stopPropagation(); dbRedisKeyMenu(more); };
  meta.appendChild(more);
  wrap.appendChild(meta);
  if (v.type === "none") {
    wrap.appendChild(el("div", "db-hint", "Key not found — it may have expired."));
    return;
  }
  if (v.note) wrap.appendChild(el("div", "db-hint", v.note as string));
  if (cfg) { dbRedisTypedTable(wrap, v, cfg); return; }
  if (v.type === "string") { dbRedisStringEditor(wrap, v); return; }
  // Stream and module types stay read-only — their commands have no field grid (docs/22 W3.3).
  const pre = el("pre", "db-ddl");
  pre.style.position = "static";
  pre.style.margin = "var(--s2)";
  pre.textContent = typeof v.value === "string" ? v.value : JSON.stringify(v.value, null, 2);
  wrap.appendChild(pre);
}

/** The TTL readout is itself the control (docs/22 W3.3): click to edit in place, Enter runs
 *  EXPIRE — or PERSIST when emptied — through the guarded console, then the key re-reads. */
function dbRedisTtl(v: ApiDbRedisValue): HTMLElement {
  const d = dbView();
  const btn = el("button", "db-ttl", v.ttl! < 0 ? "no expiry" : v.ttl! + "s") as HTMLButtonElement;
  btn.type = "button";
  btn.title = "Change the TTL — Enter applies EXPIRE, empty removes it (PERSIST)";
  btn.onclick = () => {
    const input = el("input", "db-ttl-in") as HTMLInputElement;
    input.value = v.ttl! < 0 ? "" : String(v.ttl);
    input.placeholder = "seconds";
    btn.replaceWith(input);
    input.focus();
    input.select();
    let ran = false;
    function apply() {
      if (ran) return;
      ran = true;
      const secs: string = input.value.trim();
      if (secs && !/^\d+$/.test(secs)) { toast("TTL must be a whole number of seconds", true); void dbLoadRedisValue(d.redisKey!); return; }
      const line = secs ? "EXPIRE " + d.redisKey + " " + secs : "PERSIST " + d.redisKey;
      void dbRedisCommand(line).then((j: unknown): void => {
        if (j) toast(secs ? "TTL set to " + secs + "s" : "TTL removed");
        void dbLoadRedisValue(d.redisKey!);
      });
    }
    input.onkeydown = (e: KeyboardEvent): void => {
      e.stopPropagation();
      if (e.key === "Enter") { e.preventDefault(); apply(); }
      else if (e.key === "Escape") { e.preventDefault(); ran = true; void dbLoadRedisValue(d.redisKey!); }
    };
    input.onblur = apply;
  };
  return btn;
}

/** The typed table — the row grid's own vocabulary (inserts first, db-dirty cells, db-del
 *  struck rows, the narrow ✕/↩ control column), pointing at the redis buffer instead of the
 *  SQL edit buffer. */
function dbRedisTypedTable(wrap: HTMLElement, v: ApiDbRedisValue, cfg: DbRedisTypeCfg): void {
  const b = dbRedisEdits();
  if (!b) return;
  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  hr.appendChild(el("th", "db-rowctl", ""));
  cfg.cols.forEach((c: string): void => {
    const th = el("th", "db-col", c);
    th.classList.add("db-rowctl"); // no sort affordance on a typed value table
    hr.appendChild(th);
  });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");

  // Buffered inserts first, so they read as "the row you are about to add".
  b.inserts.forEach((ins: Record<string, unknown>, i: number): void => {
    const tr = el("tr", "db-ins");
    tr.appendChild(dbRedisRowCtl((): void => {
      b?.inserts.splice(i, 1);
      renderDbGrid();
      renderDbBar();
    }, false, false));
    cfg.cols.forEach((c: string): void => {
      const editable = cfg.ins.indexOf(c) >= 0;
      const filled = editable && ins[c] != null && String(ins[c]) !== "";
      const td = el("td", "db-cell db-cell-edit" + (filled ? " db-dirty" : ""));
      td.textContent = ins[c] == null ? "" : String(ins[c]);
      if (editable) {
        td.title = "Double-click to edit";
        td.ondblclick = (): void => {
          dbRedisCellEdit(td, i, "insert", c, ins[c] == null ? "" : String(ins[c]));
        };
      }
      tr.appendChild(td);
    });
    tbody.appendChild(tr);
  });

  dbRedisEntries(v, b).forEach((entry: { addr: string; deleted: boolean; updated: boolean; cells: Record<string, unknown> }): void => {
    const tr = el("tr", entry.deleted ? "db-del" : "");
    tr.appendChild(dbRedisRowCtl(() => { dbRedisToggleDelete(entry.addr); }, cfg.deletable, entry.deleted));
    cfg.cols.forEach((c: string): void => {
      const editable = cfg.edit.indexOf(c) >= 0 && !entry.deleted;
      const td = el("td", "db-cell" + (entry.updated && cfg.edit.indexOf(c) >= 0 ? " db-dirty" : "") + (editable ? " db-cell-edit" : ""));
      td.textContent = entry.cells[c] == null ? "" : String(entry.cells[c]);
      td.title = td.textContent || ""; // long values truncate in the cell; the full text is one hover away
      if (editable) {
        td.ondblclick = (): void => {
          // -1, never an insert index: save() reads >= 0 as "this is a buffered insert row",
        // and a 0 here once made every existing-row edit vanish into inserts[0] (caught live).
        dbRedisCellEdit(td, -1, entry.addr, c, String(entry.cells[c]));
        };
      }
      tr.appendChild(td);
    });
    tbody.appendChild(tr);
  });

  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
}

/** The narrow ✕/↩ column the row grid uses, in the value view's words: ✕ buffers a delete
 *  (HDEL / ZREM / SREM on Commit) or removes a buffered insert, ↩ undoes a delete. A type
 *  with no honest delete command (list) gets no control at all. */
function dbRedisRowCtl(onClick: () => void, deletable: boolean, deleted: boolean): HTMLElement {
  const td = el("td", "db-rowctl");
  if (!deletable && !deleted) return td;
  const b2 = el("button", "db-act", deleted ? "↩" : "✕") as HTMLButtonElement;
  b2.type = "button";
  b2.title = deleted ? "Undo this buffered delete" : "Buffer a delete — applied only on Commit";
  b2.onclick = onClick;
  td.appendChild(b2);
  return td;
}

function dbRedisToggleDelete(addr: string): void {
  const b = dbRedisEdits();
  if (!b) return;
  if (b.deletes[addr]) delete b.deletes[addr];
  else {
    b.deletes[addr] = 1;
    delete b.updates[addr]; // a deleted entry's cell edits are moot
  }
  renderDbGrid();
  renderDbBar();
}

/* The one inline cell editor at a time — the row grid's db-inline-edit vocabulary (overlay
   sized to the cell, Enter saves, Esc cancels, an outside click commits) writing to the
   redis buffer. */
let dbRedisEditor: { ta: HTMLTextAreaElement; save: () => void } | null = null; // { ta, save } — close() must be able to unhook the dismiss too

function dbRedisCellEdit(td: HTMLElement, insertIdx: number, addr: string, col: string, current: string): void {
  dbRedisEditorClose();
  const wrap = $("dbGridWrap");
  if (!wrap) return;
  const wrapRect = wrap.getBoundingClientRect();
  const rect = td.getBoundingClientRect();
  const ta = el("textarea", "db-inline-edit") as HTMLTextAreaElement;
  ta.rows = 1;
  ta.spellcheck = false;
  ta.value = current;
  ta.style.left = (rect.left - wrapRect.left + wrap.scrollLeft) + "px";
  ta.style.top = (rect.top - wrapRect.top + wrap.scrollTop) + "px";
  ta.style.width = Math.max(rect.width + 24, 96) + "px";
  ta.style.minHeight = Math.max(rect.height, 22) + "px";
  wrap.appendChild(ta);
  dbRedisEditor = { ta: ta, save: save };
  function autoSize(): void {
    ta.style.height = "auto";
    ta.style.height = Math.min(Math.max(ta.scrollHeight, 22), 320) + "px";
  }
  function save(): void {
    const raw = ta.value;
    dbRedisEditorClose();
    const b = dbRedisEdits();
    if (!b) return;
    if (col === "score" && !dbRedisValidScore(raw)) {
      toast("A score is a number, or inf / -inf", true);
      renderDbGrid();
      return;
    }
    if (insertIdx >= 0) {
      const ins = b.inserts[insertIdx];
      if (ins) ins[col] = raw;
    } else if (raw === current) {
      delete b.updates[addr]; // back to the stored value: not a change
    } else {
      b.updates[addr] = raw;
    }
    renderDbGrid();
    renderDbBar();
  }
  ta.oninput = autoSize;
  ta.onkeydown = (e: KeyboardEvent): void => {
    e.stopPropagation();
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); save(); }
    else if (e.key === "Escape") { e.preventDefault(); dbRedisEditorClose(); renderDbGrid(); }
  };
  document.addEventListener("mousedown", dbRedisDismiss, true);
  autoSize();
  ta.focus();
  ta.select();
}

/* The outside-click half, named at module level so close() can unhook it — the same shape as
   data-edit.js's dbInlineDismiss. Enter/Esc close the editor through dbRedisEditorClose; a
   dismiss left registered after that ran a STALE save() on the next mousedown anywhere, and
   the cancelled text came back as a buffered change Commit would have written (docs/22
   closeout audit). */
function dbRedisDismiss(ev: MouseEvent): void {
  if (!dbRedisEditor) return;
  const ta = dbRedisEditor.ta;
  if (ev.target === ta || ta.contains(ev.target as Node)) return;
  dbRedisEditor.save();
}

function dbRedisEditorClose(): void {
  if (dbRedisEditor) {
    dbRedisEditor.ta.remove();
    document.removeEventListener("mousedown", dbRedisDismiss, true);
    dbRedisEditor = null;
  }
}

/** docs/22 W3.3: a string edits in place — one textarea, one Set, one SET through the
 *  pipeline (a value keeps its spaces: pipeline args bind as separate strings, which the
 *  whitespace-splitting /command console can never promise). */
function dbRedisStringEditor(wrap: HTMLElement, v: ApiDbRedisValue): void {
  const d = dbView();
  const row = el("div", "db-redis-str-row");
  const ta = el("textarea", "db-redis-str") as HTMLTextAreaElement;
  ta.rows = 3;
  ta.spellcheck = false;
  ta.value = typeof v.value === "string" ? v.value : "";
  const set = el("button", "btn primary", "Set") as HTMLButtonElement;
  set.type = "button";
  async function go(): Promise<void> {
    const value = ta.value;
    if (value === ta.dataset.orig) { toast("Unchanged"); return; }
    const j = await apiJson("/api/db/" + encodeURIComponent(d.conn!) + "/redis-pipeline", {
      method: "POST",
      body: JSON.stringify({ commands: [["SET", d.redisKey!, value]] }),
    });
    if (!j) return;
    toast("Set " + d.redisKey);
    void dbLoadRedisValue(d.redisKey!);
  }
  set.onclick = (): void => { void go(); };
  ta.onkeydown = (e: KeyboardEvent): void => {
    e.stopPropagation();
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); void go(); }
  };
  ta.dataset.orig = ta.value;
  row.appendChild(ta);
  row.appendChild(set);
  wrap.appendChild(row);
}

/** Commit the buffer: ONE pipeline round trip. The POST body is built from the very list the
 *  preview printed (dbRedisCommands), so the two cannot drift. A refusal keeps the buffer —
 *  the toast already said which command and why. */
async function dbRedisCommit(): Promise<void> {
  const d = dbView();
  const b = dbRedisEdits();
  if (!b) return;
  let cmds: { verb: string; args: unknown[] }[];
  try {
    cmds = dbRedisCommands(d.redisKey!, b.type, b);
  } catch (err) {
    toast(errText(err), true);
    return;
  }
  if (!cmds.length) return;
  if (!confirm("Commit " + cmds.length + " command" + (cmds.length > 1 ? "s" : "") + " to " + d.redisKey + "?\n" +
      "One pipelined round trip, every command guard-checked. Redis has no transaction here — a failed command stops the run.")) return;
  const j = await apiJson("/api/db/" + encodeURIComponent(d.conn!) + "/redis-pipeline", {
    method: "POST",
    body: JSON.stringify({ commands: cmds.map((c: { verb: string; args: unknown[] }): unknown[] => { return [c.verb as unknown].concat(c.args); }) }),
  });
  if (!j) return;
  toast("Committed " + cmds.length + " command" + (cmds.length > 1 ? "s" : ""));
  d.redisEdits = null;
  d.sqlPreview = false;
  const key = d.redisKey;
  // Awaited: deleting a hash's last field (or a set's last member) deletes the KEY, and the
  // re-read's "type none" answer IS the signal — checking before it answered read null and
  // the list refresh never ran, so the sidebar kept offering a key that is gone (docs/22
  // closeout audit).
  await dbLoadRedisValue(key!);
  if (d.redisKey === key && d.redisValue && d.redisValue.type === "none") void dbLoadKeys(true);
}

/** Drop the buffer without a single command — the twin of the row grid's Discard. */
function dbRedisDiscard(): void {
  const d = dbView();
  const n = dbRedisPendingCount();
  if (!n) return;
  if (!confirm("Discard " + n + " buffered change" + (n > 1 ? "s" : "") + "? Nothing has been written to redis.")) return;
  d.redisEdits = null;
  d.sqlPreview = false;
  renderDbGrid();
  renderDbBar();
}

function dbRedisKeyMenu(anchorEl: HTMLElement): void {
  popupMenu(anchorEl.getBoundingClientRect(), [
    { label: "Rename\u2026", fn: (): void => { dbRedisRenameSheet(dbView().redisKey); } },
    { sep: true },
    { label: "Delete\u2026", danger: true, fn: dbRedisDeleteKey },
  ]);
}

/** One guarded console command; null means the toast already said why. */
async function dbRedisCommand(line: string): Promise<unknown> {
  // No connection is not an error worth a toast — it is the console being asked to run
  // against nothing, which the null return already says (docs/37 R4: the assertion this
  // replaced claimed a selection the record's type never promised).
  const conn = dbView().conn;
  if (!conn) return null;
  return apiJson("/api/db/" + encodeURIComponent(conn) + "/command", {
    method: "POST",
    body: JSON.stringify({ command: line }),
  });
}

/** The one-input sheet Rename uses (docs/22 W1.3). `submit(value)` runs after the sheet
 *  closes; it owns its own reload and error handling. */
function dbRedisKeySheet(cfg: { title: string; label: string; value?: string; placeholder?: string; primary: string; submit: (v: string) => void }): void {
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="' + esc(cfg.title) + '">' +
      '<div class="sheet-head"><h2>' + esc(cfg.title) + '</h2></div>' +
      '<div class="sheet-body">' +
        '<label class="field"><span>' + esc(cfg.label) + '</span><input id="dbKeyIn" autocomplete="off"></label>' +
      "</div>" +
      '<div class="sheet-foot"><span class="grow"></span>' +
        '<button class="btn" id="dbKeyCancel">Cancel</button>' +
        '<button class="btn primary" id="dbKeyGo">' + esc(cfg.primary) + "</button></div>" +
    "</div>";
  $("sheet").hidden = false;
  const input = $<HTMLInputElement>("dbKeyIn");
  input.value = cfg.value || "";
  input.placeholder = cfg.placeholder || "";
  $("dbKeyCancel").onclick = closeSheet;
  $("sheet").onclick = (e: MouseEvent): void => { if (e.target === $("sheet")) closeSheet(); };
  function go(): void { const v: string = input.value.trim(); closeSheet(); cfg.submit(v); }
  $("dbKeyGo").onclick = go;
  input.onkeydown = (e: KeyboardEvent): void => {
    if (e.key === "Enter") { e.preventDefault(); go(); }
  };
  input.focus();
  input.select();
}

function dbRedisRenameSheet(key: string | null): void {
  // The menu that opens this only exists over a selected key, but the record says the
  // selection is nullable and that is the truth to honour — not an assertion at the call.
  if (!key) return;
  dbRedisKeySheet({
    title: "Rename key",
    label: "New name",
    value: key,
    primary: "Rename",
    submit: async (to: string): Promise<void> => {
      const d = dbView();
      if (!to || to === key) return;
      const j = await dbRedisCommand("RENAME " + key + " " + to);
      if (!j) return;
      toast("Renamed to " + to);
      d!.redisKey = to;
      await dbLoadKeys(true);
      void dbLoadRedisValue(to);
    },
  });
}

/* Deleting a key is the one destructive act the redis side has — the same typed-name confirm
   the table DROP/TRUNCATE use (a W1 audit follow-up), with the key's own words. */
function dbRedisDeleteKey(): void {
  const d = dbView();
  const key = d.redisKey!;
  dbTypedConfirm({ what: "DELETE (permanently)", name: key, kind: "key" }, (): void => {
    void dbRedisCommand("DEL " + key).then(async (j: unknown): Promise<void> => {
      if (!j) return;
      toast("Deleted " + key);
      d.redisKey = null;
      d.redisValue = null;
      d.redisEdits = null;
      await dbLoadKeys(true);
      renderDbGrid();
    });
  });
}

export {
  DB_REDIS_TYPES, dbIsRedis, dbLoadKeys, dbLoadRedisValue, dbRedisValidScore,
  dbRedisCommandText, dbRedisCommands, dbRedisCommit, dbRedisDiscard, dbRedisEntries,
  dbRedisPendingCount, dbRenderRedisValue,
};
