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

import type { ApiDbColumn } from "./types/api.js";
import type { DbDdlPayload, DdlRow, DdlSheetState } from "./types/state.js";
import { $, api, apiJson, el, toast } from "./util.js";
import { fill, h } from "./h.js";
import type { HChild } from "./h.js";
import { dbHighlightNodes } from "./data-filters.js";
import { dbLoadData } from "./data-grid.js";
import { dbLoadTables } from "./data-view.js";
import { dbOpenTab } from "./data-tabs.js";
import { dbLoadDetail } from "./data-structure.js";
import { tk, tr } from "./i18n.js";
import { btn, iconBtn } from "./ui/button.js";
import { checkField, field, hint, pair } from "./ui/form.js";
import { closeSheet, sheet, showSheet } from "./ui/sheet.js";

/* ================================================================================================
   docs/22 W4.6 — the minimal DDL set: CREATE TABLE, ADD COLUMN, CREATE INDEX, one sheet per
   op. The SQL preview is fetched live from POST /api/db/:name/ddl-preview and Commit posts
   the SAME (op, payload) to /api/db/:name/ddl, which builds the statements with the very
   function the preview used (swiss-host's build_ddl_create) — what the sheet shows is what
   Commit runs, byte for byte (pgAdmin's msql flow: one SQL producer serves preview and save).
   Identifiers are vetted server-side against the whitelist; refusals arrive as the server's
   own message and are painted in the preview area, not paraphrased.
   ================================================================================================ */

/* Common types first, dialect extras after — the datalist is suggestion, not constraint: the
   type stays a free-text input because real type spells (`numeric(12,4)`, `enum(...)`) are
   open-ended. Pure so the list can be pinned without a DOM. */
const DB_DDL_TYPES_COMMON = [
  "int", "bigint", "varchar(255)", "text", "date", "datetime", "timestamp",
  "decimal(10,2)", "double", "float", "json", "blob",
];
const DB_DDL_TYPES_MYSQL = [
  "tinyint", "smallint", "mediumint", "int unsigned", "char(36)", "varbinary(16)",
  "tinytext", "mediumtext", "longtext", "time", "year", "enum('a','b')",
];
const DB_DDL_TYPES_PG = [
  "smallint", "serial", "character varying(255)", "char(1)", "boolean", "numeric(10,2)",
  "real", "double precision", "uuid", "jsonb", "bytea", "inet", "timestamptz", "interval",
  "int[]",
];

function dbDdlTypeOptions(dialect: string): string[] {
  return DB_DDL_TYPES_COMMON.concat(dialect === "pg" ? DB_DDL_TYPES_PG : DB_DDL_TYPES_MYSQL);
}

/** The pgAdmin three-bucket diff (utils/__init__.py's collection diff): the form's rows
 *  against the columns the table already has — added (new names), changed (same name,
 *  different facts), removed (old names the form no longer lists). The W4.6 minimal set
 *  commits ONLY the added bucket; the other two are classified anyway so nothing can
 *  silently pretend it ran. Rows without a name are nothing yet. Pure. */
function dbDdlDiffColumns(oldCols: ApiDbColumn[] | null | undefined, rows: DdlRow[] | null | undefined): { added: DdlRow[]; changed: DdlRow[]; removed: ApiDbColumn[] } {
  const oldByName: Record<string, ApiDbColumn> = {};
  (oldCols || []).forEach((c: ApiDbColumn): void => { oldByName[c.name] = c; });
  const names: Record<string, boolean> = {};
  const added: DdlRow[] = [];
  const changed: DdlRow[] = [];
  (rows || []).forEach((r: DdlRow): void => {
    const name = (r.name || "").trim();
    if (!name) return;
    names[name] = true;
    const o = oldByName[name];
    if (!o) { added.push(r); return; }
    const same = (r.type || "").trim() === (o.dataType || "").trim()
      && !!r.nullable === !!o.nullable
      && (r.default || "").trim() === (o.defaultValue == null ? "" : String(o.defaultValue)).trim()
      && (r.comment || "").trim() === (o.comment == null ? "" : String(o.comment)).trim();
    if (!same) changed.push(r);
  });
  const removed = (oldCols || []).filter((c: ApiDbColumn): boolean => { return !names[c.name]; });
  return { added: added, changed: changed, removed: removed };
}

/** The (op, payload) body both W4.6 endpoints take, from the sheet's form state. Returns null
 *  when there is nothing to preview yet — the sheet shows its quiet hint instead of calling
 *  the server. Only rows that carry a name ride along; a half-filled row still goes, so the
 *  server's refusal (shown verbatim) names what is missing. Pure over the form object. */
function dbDdlFormPayload(kind: string, form: { schema: string; table: string; comment: string; columns: DdlRow[]; oldColumns: ApiDbColumn[]; index: string; indexColumns: string[]; unique: boolean }): DbDdlPayload | null {
  const col = (r: DdlRow): { name?: string; type?: string; nullable?: boolean; default?: string; comment?: string } => {
    const o: { name?: string; type?: string; nullable?: boolean; default?: string; comment?: string } = { name: r.name, type: r.type, nullable: !!r.nullable };
    if (r.default) o.default = r.default;
    if (r.comment) o.comment = r.comment;
    return o;
  };
  const base: DbDdlPayload = {};
  if (form.schema) base.schema = form.schema;
  if (kind === "index") {
    const cols: string[] = (form.indexColumns || []).filter(Boolean);
    if (!form.table || !form.index || !cols.length) return null;
    base.table = form.table;
    base.index = form.index;
    base.columns = cols;
    if (form.unique) base.unique = true;
    return base;
  }
  if (!form.table) return null;
  base.table = form.table;
  const rows: DdlRow[] = (form.columns || []).filter((r: DdlRow) => { return (r.name || "").trim(); });
  if (kind === "table") {
    if (!rows.length) return null;
    base.columns = rows.map(col);
    if (form.comment) base.comment = form.comment;
    return base;
  }
  // add_column: the added bucket of the diff IS the commit; changed/removed stay out of the
  // minimal set (docs/22 §9) and are only classified, never run.
  const added = dbDdlDiffColumns(form.oldColumns || [], rows).added;
  if (!added.length) return null;
  base.columns = added.map(col);
  return base;
}

/** A conventional index name (adminer's spelling): table, the column run, _idx — lower-cased
 *  and capped under the identifier whitelist's 64, so the suggestion itself always passes it.
 *  Pure. */
function dbDdlIndexSuggestion(table: unknown, cols: string[]): string {
  const run = cols.length ? "_" + cols.join("_") : "";
  return (String(table) + run + "_idx").toLowerCase().slice(0, 63);
}

/* --- the sheet ---------------------------------------------------------------------------------- */

const DDL_OP = { table: "create_table", column: "add_column", index: "create_index" };
const DDL_TITLE = { table: tk("dataDdl.newTable"), column: tk("dataDdl.addColumn"), index: tk("dataDdl.newIndex") };
/* tk()-marked "… in {t}" titles (docs/38 L7): chosen by kind, painted through tr(). */
const DDL_IN = { table: tk("dataDdl.newTableT"), column: tk("dataDdl.addColumnT"), index: tk("dataDdl.newIndexT") };
/* tk()-marked header words (docs/38 L7): painted through tr(hd) below. */
const DDL_HEAD = [tk("dataDdl.name"), tk("dataDdl.type"), tk("dataDdl.null"), tk("dataDdl.default"), tk("dataDdl.comment"), ""];
const DDL_QUIET = {
  table: tk("dataDdl.quietTable"),
  column: tk("dataDdl.quietColumn"),
  index: tk("dataDdl.quietIndex"),
};

/** One sheet's whole mutable context; rebuilt per open, nulled on close. */
let S: DdlSheetState | null = null;

/** Open one W4.6 sheet. ctx = { kind, dialect, conn, schema, schemas, table, columns } —
 *  kind "table" creates fresh (schema select for pg), "column"/"index" prefill the table's
 *  old state (docs/22 W4.6: same form, prefilled, commit diffs). */
function openDbDdlSheet(kind: "table" | "column" | "index", ctx: { dialect: string; conn: string; schema?: string; schemas?: string[]; table?: string; columns?: ApiDbColumn[] }): void {
  closeDbDdlSheet();
  let rows: DdlRow[] = [];
  if (kind === "column") {
    rows = (ctx.columns || []).map((c: ApiDbColumn): DdlRow => {
      return {
        name: c.name, type: c.dataType, nullable: c.nullable,
        default: c.defaultValue == null ? "" : String(c.defaultValue),
        comment: c.comment == null ? "" : String(c.comment),
        isNew: false,
      };
    });
    rows.push({ name: "", type: "", nullable: true, default: "", comment: "", isNew: true });
  } else {
    rows = [{ name: "", type: "", nullable: true, default: "", comment: "", isNew: true }];
  }
  S = {
    kind: kind, dialect: ctx.dialect, conn: ctx.conn,
    schema: ctx.schema || "", schemas: ctx.schemas || [],
    table: ctx.table || "", oldColumns: ctx.columns || [],
    rows: rows,
    indexName: kind === "index" ? dbDdlIndexSuggestion(ctx.table || "t", []) : "",
    indexCols: [], unique: false, comment: "",
    lastSql: null, lastPayload: null, seq: 0, timer: null,
  };
  paintDbDdlSheet();
  void refreshDbDdlPreview();
}

function closeDbDdlSheet() {
  if (S && S.timer) clearTimeout(S.timer);
  S = null;
  closeSheet();
}

/** Rebuild the whole sheet DOM from S. Called on open and on row add/remove only — keystrokes
 *  write straight into S (and schedule a preview) so an input never loses focus to a re-render.
 *  The library's sheet and form (docs/46 P7): showSheet unhides the host before it paints; the
 *  per-open wiring (wireDbDdlSheet) stays, per the sheet idiom. */
function paintDbDdlSheet(): void {
  const S_ = S!;
  const kind = S_.kind;
  const title = S_.table ? tr(DDL_IN[kind], { t: S_.table }) : tr(DDL_TITLE[kind]);
  const body: (HChild | null)[] = [];
  if (kind === "table" && S_.dialect === "pg") {
    // docs/22 W1.1: a Postgres catalog is many schemas, so the new table says where it goes
    // (swiss-ui-design rule 6) — a select, prefilled from the list's active schema filter.
    const schemas = S_.schemas.slice();
    if (!schemas.includes(S_.schema)) schemas.unshift(S_.schema);
    body.push(field({
      label: tr("dataDdl.schema"),
      control: h("select", { id: "ddl-schema" }, schemas.map((s: string): HChild => {
        return h("option", { value: s, selected: s === S_.schema }, s);
      })),
    }));
  }
  if (kind === "table") {
    body.push(pair(
      field({ label: tr("dataDdl.tableName"), control: h("input", { id: "ddl-table", autocomplete: "off", spellcheck: false, placeholder: tr("dataDdl.events") }) }),
      field({ label: tr("dataDdl.commentOptional"), control: h("input", { id: "ddl-comment", autocomplete: "off", placeholder: tr("dataDdl.whatTableHolds") }) })));
  }
  if (kind === "index") {
    body.push(
      pair(
        field({ label: tr("dataDdl.indexName"), control: h("input", { id: "ddl-index", autocomplete: "off", spellcheck: false, value: S_.indexName }) }),
        checkField({ label: tr("dataDdl.unique"), control: h("input", { type: "checkbox", id: "ddl-unique" }) as HTMLInputElement })),
      // A group: the caption names the checks and clicks none of them.
      field({ group: true, label: tr("dataDdl.columns"), control: h("div", { id: "ddl-cols", class: "db-ddl-colpick" }) }));
  }
  if (kind !== "index") {
    body.push(field({ group: true, label: tr("dataDdl.columns"), control: h("div", null,
      h("table", { class: "db-ddl-grid", id: "ddl-grid" },
        h("colgroup",
          h("col", { style: "width:22%" }), h("col", { style: "width:24%" }), h("col", { style: "width:56px" }),
          h("col", { style: "width:20%" }), h("col", { style: "width:28%" }), h("col", { style: "width:26px" })),
        h("thead", null, h("tr", null,
          DDL_HEAD.map((hd: string): HChild => {
            return h("th", null, hd ? tr(hd) : hd);
          }))),
        h("tbody")),
      btn(tr("dataDdl.addColumn2"), { kind: "ghost", id: "ddl-add-row" }),
      h("datalist", { id: "ddl-types" }, dbDdlTypeOptions(S_.dialect).map((t: string): HChild => {
        return h("option", { value: t });
      }))) }));
  }
  body.push(
    hint(tr("dataDdl.sqlPreviewCommitRuns"), { id: "ddl-hint" }),
    h("pre", { class: "db-ddl", id: "ddl-pre" }));
  showSheet(sheet({
    title,
    titleId: "ddl-title",
    label: tr(DDL_TITLE[kind]),
    body,
    foot: [
      btn(tr("dataDdl.cancel"), { id: "ddl-cancel" }),
      btn(tr(kind === "table" ? "dataDdl.createTable" : kind === "column" ? "dataDdl.addColumn2" : "dataDdl.createIndex"), { kind: "primary", id: "ddl-commit", disabled: true }),
    ],
  }));
  wireDbDdlSheet();
  // The mini-grid belongs to the table and column kinds only — the index sheet has no
  // #ddl-grid, and rendering rows into it would throw before the picker and the quiet
  // hint ever paint (caught live: the index sheet opened blank).
  if (kind !== "index") renderDbDdlRows();
  if (kind === "index") renderDbDdlColPick();
  paintDbDdlPreviewQuiet();
}

/* Every input writes into S directly — S is the model; the DOM is a view of it. */
function wireDbDdlSheet(): void {
  const S_ = S!;
  const kind = S_.kind;
  $("ddl-cancel").onclick = closeDbDdlSheet;
  $("sheet").onclick = (e) => { if (e.target === $("sheet")) closeDbDdlSheet(); };
  // Enter submits the primary from any text input, the way every one-input sheet does.
  $("sheet").onkeydown = (e: KeyboardEvent): void => {
    const tgt = e.target as HTMLInputElement | null;
    if (e.key === "Enter" && tgt && tgt.tagName === "INPUT" && tgt.type !== "checkbox") {
      e.preventDefault();
      void commitDbDdl();
    }
  };
  const retitle = (): void => {
    if (S?.dialect === "pg" && kind === "table") {
      $("ddl-title").textContent = tr("dataDdl.newTableT2", { t: $<HTMLSelectElement>("ddl-schema").value });
    }
  };
  if (kind === "table") {
    if (S_.dialect === "pg") {
      $<HTMLSelectElement>("ddl-schema").onchange = (e) => {
        S!.schema = (e.currentTarget as HTMLSelectElement).value;
        retitle();
        scheduleDbDdlPreview();
      };
      retitle(); // say where the table goes from the first paint, not only after a change
    }
    // The name and comment wire for EVERY dialect — mysql tables need the model write just
    // as much as pg ones (caught live: the handlers once sat in the pg-only branch and a
    // mysql sheet never left its quiet hint).
    $<HTMLInputElement>("ddl-table").oninput = (e) => { S!.table = (e.currentTarget as HTMLInputElement).value; scheduleDbDdlPreview(); };
    $<HTMLInputElement>("ddl-table").value = S_.table;
    $<HTMLInputElement>("ddl-comment").oninput = (e) => { S!.comment = (e.currentTarget as HTMLInputElement).value; scheduleDbDdlPreview(); };
    $<HTMLInputElement>("ddl-table").focus();
  } else if (kind === "index") {
    // the name input's own handler lives with the column picker it tracks (below)
    $<HTMLInputElement>("ddl-unique").onchange = (e) => { S!.unique = (e.currentTarget as HTMLInputElement).checked; scheduleDbDdlPreview(); };
    $<HTMLInputElement>("ddl-index").focus();
    $<HTMLInputElement>("ddl-index").select();
  }
  // Both grid kinds can grow a row — table and column alike (the wiring once sat in an
  // else branch the table kind never reached, caught live on 19998).
  if (kind !== "index") {
    $<HTMLButtonElement>("ddl-add-row").onclick = (): void => {
      S?.rows.push({ name: "", type: "", nullable: true, default: "", comment: "", isNew: true });
      renderDbDdlRows();
      const last = $("ddl-grid").querySelectorAll<HTMLInputElement>("tbody tr:last-child input[data-k=name]")[0];
      if (last) last.focus();
    };
  }
  $<HTMLButtonElement>("ddl-commit").onclick = (): void => { void commitDbDdl(); };
}

/** The mini-grid's rows. Old (prefilled) rows render disabled — only ADD is in the W4.6
 *  minimal set — so the diff's added bucket is exactly the editable rows. */
function renderDbDdlRows(): void {
  const body = $("ddl-grid").querySelector<HTMLElement>("tbody");
  body!.textContent = "";
  S?.rows.forEach((r: DdlRow, i: number): void => {
    const tri = el("tr");
    const mk = (k: string, value: string, ph?: string): HTMLTableCellElement => {
      const td = el("td");
      const input = document.createElement("input");
      input.type = "text";
      input.value = value == null ? "" : value;
      input.dataset.k = k;
      input.dataset.i = String(i);
      if (ph) input.placeholder = ph;
      if (k === "type") input.setAttribute("list", "ddl-types");
      if (!r.isNew) input.disabled = true;
      input.oninput = (e) => {
        r[k as "name" | "type" | "default" | "comment"] = (e.currentTarget as HTMLInputElement).value;
        scheduleDbDdlPreview();
      };
      td.appendChild(input);
      return td;
    };
    tri.appendChild(mk("name", r.name, "id"));
    tri.appendChild(mk("type", r.type, "int"));
    const tdn = el("td");
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = !!r.nullable;
    cb.dataset.i = String(i);
    cb.setAttribute("aria-label", tr("dataDdl.nullable"));
    if (!r.isNew) cb.disabled = true;
    cb.onchange = (e) => { r.nullable = (e.currentTarget as HTMLInputElement).checked; scheduleDbDdlPreview(); };
    tdn.appendChild(cb);
    tri.appendChild(tdn);
    tri.appendChild(mk("default", r.default, "0"));
    tri.appendChild(mk("comment", r.comment, ""));
    const tdx = el("td");
    // Ghost: a remove on every row of a form grid is quiet until the pointer reaches it.
    const rm = iconBtn("x", tr("dataDdl.removeColumn"), {
      ghost: true, data: { i: String(i) }, disabled: !r.isNew, title: r.isNew ? undefined : tr("dataDdl.onlyNewColumnsCan"),
    });
    rm.onclick = (): void => {
      S?.rows.splice(i, 1);
      renderDbDdlRows();
      scheduleDbDdlPreview();
    };
    tdx.appendChild(rm);
    tri.appendChild(tdx);
    body?.appendChild(tri);
  });
}

/** The index sheet's column picker: one check per existing column, in the table's order. */
function renderDbDdlColPick(): void {
  const box = $("ddl-cols");
  box.textContent = "";
  S?.oldColumns.forEach((c: ApiDbColumn): void => {
    const label = document.createElement("label");
    label.className = "check";
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.value = c.name;
    cb.onchange = (e) => {
      const t = e.currentTarget as HTMLInputElement;
      const S_ = S!;
      const at = S_.indexCols.indexOf(c.name);
      if (t.checked && at < 0) S_.indexCols.push(c.name);
      if (!t.checked && at >= 0) S_.indexCols.splice(at, 1);
      // The suggestion follows the picks until the user edits it by hand.
      if (!S_.indexTouched) {
        S_.indexName = dbDdlIndexSuggestion(S_.table, S_.indexCols);
        const name = $<HTMLInputElement>("ddl-index");
        if (name) name.value = S_.indexName;
      }
      scheduleDbDdlPreview();
    };
    label.appendChild(cb);
    label.appendChild(document.createTextNode(c.name));
    box.appendChild(label);
  });
  const nameInput = $<HTMLInputElement>("ddl-index");
  if (nameInput) {
    nameInput.oninput = (e) => { const ddl = S!;
    ddl.indexName = (e.currentTarget as HTMLInputElement).value; ddl.indexTouched = true; scheduleDbDdlPreview(); };
  }
}

/* --- the preview: live, server-built, error text verbatim ---------------------------------------- */

function paintDbDdlPreviewQuiet(): void {
  const pre = $("ddl-pre");
  if (!pre) return;
  const S_ = S!;
  pre.textContent = tr(DDL_QUIET[S_.kind]);
  pre.classList.add("db-ddl-quiet");
  $<HTMLButtonElement>("ddl-commit").disabled = true;
  S_.lastSql = null;
  S_.lastPayload = null;
}

function paintDbDdlPreviewSql(sql: string): void {
  const pre = $("ddl-pre");
  pre.classList.remove("db-ddl-quiet");
  fill(pre, dbHighlightNodes(sql));
  $<HTMLButtonElement>("ddl-commit").disabled = false;
}

function paintDbDdlPreviewError(message: string): void {
  const pre = $("ddl-pre");
  pre.classList.remove("db-ddl-quiet");
  // The server's own refusal, unpainted: it names the field and the rule (kept verbatim from the error text).
  pre.textContent = message;
  $<HTMLButtonElement>("ddl-commit").disabled = true;
  const ddl_ = S!;
  ddl_.lastSql = null;
  ddl_.lastPayload = null;
}

function scheduleDbDdlPreview(): void {
  if (!S) return;
  clearTimeout(S.timer!);
  S.timer = setTimeout((): void => { void refreshDbDdlPreview(); }, 250);
}

/** Read the form, ask the server for the statements, paint them. The payload read here is
 *  the very object Commit posts — one builder, one input, preview = commit. */
async function refreshDbDdlPreview(): Promise<void> {
  // Escape closes the sheet through main.js's own closeSheet(), which this module cannot
  // see — a still-pending timer then finds its DOM gone. The guard turns that into a no-op.
  if (!S || !$("ddl-pre")) return;
  const form = {
    schema: S.schema, table: S.table, comment: S.comment,
    columns: S.rows, oldColumns: S.oldColumns,
    index: S.indexName, indexColumns: S.indexCols, unique: S.unique,
  };
  const payload = dbDdlFormPayload(S.kind, form);
  if (!payload) { paintDbDdlPreviewQuiet(); return; }
  const seq = ++S.seq;
  let r: Response;
  try {
    r = await api("/api/db/" + encodeURIComponent(S.conn) + "/ddl-preview", {
      method: "POST",
      body: JSON.stringify({ op: DDL_OP[S.kind], payload: payload }),
    });
  } catch (e) {
    if (S && seq === S.seq) paintDbDdlPreviewError(tr("dataDdl.previewGatewayDown"));
    return;
  }
  if (!S || seq !== S.seq) return; // a newer keystroke already superseded this answer
  const j = await r.json().catch(() => { return {}; });
  if (!r.ok) {
    paintDbDdlPreviewError(j.error || "HTTP " + r.status);
    return;
  }
  S.lastSql = j.sql || "";
  S.lastPayload = payload;
  if (!S.lastSql) { paintDbDdlPreviewQuiet(); return; }
  paintDbDdlPreviewSql(S.lastSql);
}

/** Commit posts the previewed (op, payload) to /ddl. Refreshed first so the text on screen
 *  and the payload posted can never be two different drafts (the 250 ms debounce window). */
async function commitDbDdl(): Promise<void> {
  if (!S) return;
  await refreshDbDdlPreview();
  if (!S || !S.lastSql || !S.lastPayload) return;
  const btn = $<HTMLButtonElement>("ddl-commit");
  btn.disabled = true;
  const j = await apiJson("/api/db/" + encodeURIComponent(S.conn) + "/ddl", {
    method: "POST",
    body: JSON.stringify({ op: DDL_OP[S.kind], payload: S.lastPayload }),
  });
  if (!S) return; // the sheet closed while the request was in flight
  if (!j) { btn.disabled = false; return; } // apiJson already toasted the refusal
  const kind = S.kind;
  const table: string = S.lastPayload.table!;
  const schema = S.lastPayload.schema || "";
  closeDbDdlSheet();
  if (kind === "table") {
    toast(tr("dataDdl.tableCreated", { table: table }));
    await dbLoadTables();
    dbOpenTab({ kind: "table", table: table, schema: schema });
  } else if (kind === "column") {
    toast(tr("dataDdl.columnAdded", { table: table }));
    void dbLoadDetail();
    void dbLoadData(true);
  } else {
    toast(tr("dataDdl.indexCreated", { table: table }));
    void dbLoadDetail();
  }
}

export {
  DB_DDL_TYPES_COMMON, DB_DDL_TYPES_MYSQL, DB_DDL_TYPES_PG,
  closeDbDdlSheet, dbDdlDiffColumns, dbDdlFormPayload, dbDdlIndexSuggestion,
  dbDdlTypeOptions, openDbDdlSheet,
};
