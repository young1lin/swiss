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

import type { ApiDbColumn, ApiDbConnectionRow, ApiDbFkRow, ApiDbTableDetail } from "./types/api.js";
import type { DbDetailSpec, DbState } from "./types/state.js";
import { apiJson, dbReqGuard, el, state } from "./util.js";
import { dbIsRedis } from "./data-browsers.js";
import { dbDialectOf, dbOkToDrop, dbOpenTable } from "./data-view.js";
import { openDbDdlSheet } from "./data-ddl.js";
import { dbTableMenu } from "./data-edit.js";
import { dbHighlightSql, renderDbFilters } from "./data-filters.js";
import { renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { renderDbBar } from "./data-sql.js";

/* --- structure tabs (columns / indexes / DDL / foreign keys) ------------------------------------ */

const DB_TABS = [
  { id: "data", label: "Data" },
  { id: "form", label: "Form" },
  { id: "columns", label: "Columns" },
  { id: "indexes", label: "Indexes" },
  { id: "fks", label: "Foreign Keys" },
  { id: "ddl", label: "DDL" },
];

function dbSetTab(t: string): void {
  const d = state.db;
  const d_ = d!;
  if (d_.tab === t) return;
  // docs/22 W5.1: the form opens on the row the keyboard focused, and the grid's focus
  // returns to the form's row — one cursor, two presentations of it.
  if (t === "form" && d_.focus) d_.formIdx = d_.focus.r;
  if (t === "data" && d_.formIdx != null) d_.focus = { r: d_.formIdx, c: d_.focus ? d_.focus.c : 0 };
  d_.tab = t as DbState["tab"];
  renderDbToolbar();
  renderDbFilters();
  renderDbGrid();
  renderDbBar();
  if (t !== "data" && t !== "form" && d_.conn && d_.table) void dbLoadDetail();
}

// One /schema request chain: a slow answer for the table the user just left must be
// dropped, or it would paint the OLD table's structure (and rewrite d.schema) over the new
// one's (docs/22 closeout audit).
const dbDetailReq = dbReqGuard();

async function dbLoadDetail(): Promise<void> {
  const d = state.db;
  const d_ = d!;
  if (!d_.conn || !d_.table) return;
  d_.detailBusy = true;
  renderDbGrid();
  let q = "/api/db/" + encodeURIComponent(d_.conn) + "/schema?table=" + encodeURIComponent(d_.table);
  if (d_.schema) q += "&schema=" + encodeURIComponent(d_.schema);
  const token = dbDetailReq.issue();
  const j = await apiJson<ApiDbTableDetail>(q);
  if (!dbDetailReq.accepts(token)) return; // superseded: a newer table owns the detail
  d_.detailBusy = false;
  if (!j) { d_.detail = null; renderDbGrid(); return; }
  d_.detail = j;
  d_.schema = j.schema;
  renderDbGrid();
}

function dbRenderTabs(ctl: HTMLElement): void {
  const d = state.db;
  const seg = el("div", "db-tabs");
  seg.setAttribute("role", "tablist");
  DB_TABS.forEach((t: { id: string; label: string }): void => {
    const b = el("button", "", t.label);
    b.setAttribute("role", "tab");
    b.setAttribute("aria-selected", String(d?.tab === t.id));
    b.onclick = () => { dbSetTab(t.id); };
    seg.appendChild(b);
  });
  ctl.appendChild(seg);
  // The Table menu: rename / truncate / drop, guarded by typed confirms server- AND client-side.
  if (!dbIsRedis()) {
    const tblBtn = el("button", "btn", "Table \u25be");
    tblBtn.title = "Rename, truncate or drop this table";
    tblBtn.onclick = (e: MouseEvent): void => { e.stopPropagation(); dbTableMenu(tblBtn); };
    ctl.appendChild(tblBtn);
  }
}

/** The Structure tabs reuse the grid wrapper: Columns/Indexes/FKs render as plain tables,
 *  DDL as a monospace block. */
function renderDbDetailGrid(wrap: HTMLElement): void {
  const d = state.db;
  const d_ = d!;
  if (d_.detailBusy) { wrap.appendChild(el("div", "db-hint", "Loading…")); return; }
  if (!d_.detail) { wrap.appendChild(el("div", "db-hint", "Select a table on the left to see its structure.")); return; }
  const det = d_.detail;
  if (d_.tab === "ddl") {
    wrap.appendChild(el("div", "db-detail-meta",
      (d_.conn && d_.conns.some((c: ApiDbConnectionRow): boolean => { return c.name === d?.conn && c.dialect === "pg"; })
        ? "Postgres keeps DDL in migration scripts — this sketch is assembled from the catalog."
        : "From SHOW CREATE TABLE.")));
    const pre = el("pre", "db-ddl db-sql-hl");
    pre.style.position = "static"; // undo the overlay absolute positioning — this is a plain block
    pre.innerHTML = dbHighlightSql(dbAlignDdl(det.ddl || "(no DDL)"));
    wrap.appendChild(pre);
    return;
  }
  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  let spec: DbDetailSpec;
  if (d_.tab === "columns") {
    spec = {
      head: ["Column", "Type", "Nullable", "Default", "Key", "Comment"],
      row: (c: ApiDbColumn) => {
        return [c.name, c.dataType, c.nullable ? "YES" : "NO",
          c.defaultValue == null ? "—" : String(c.defaultValue),
          c.isPrimaryKey ? "PRI" : "",
          // { text, cls } marks a cell that carries a class of its own: real comments join the
          // header caption's green, the "—" placeholder stays in the default cell color.
          c.comment == null ? "—" : { text: String(c.comment), cls: "db-det-comment" }];
      },
      rows: det.columns,
      meta: det.columns.length + " columns · primary key: " + (det.primaryKey.join(", ") || "none"),
    };
  } else if (d_.tab === "indexes") {
    spec = {
      head: ["Index", "Unique", "Primary", "Columns"],
      row: (x: { name: string; unique: boolean; primary: boolean; columns: string[]; definition?: string }) => {
        return [x.name, x.unique ? "yes" : "no", x.primary ? "yes" : "no",
          (x.columns && x.columns.length ? x.columns.join(", ") : "") || (x.definition || "")];
      },
      rows: det.indexes,
      meta: det.indexes.length + " indexes",
    };
  } else {
    spec = {
      head: ["Constraint", "Column", "References"],
      row: (f: ApiDbFkRow) => {
        // docs/22 W5.2: the target name carries its fk — the renderer draws it as a link
        // (read-only navigation; the grid header's arrow is the filtered jump).
        return [f.name, f.column, { text: f.refSchema + "." + f.refTable + " (" + f.refColumn + ")", fk: f }];
      },
      rows: det.foreignKeys,
      meta: det.foreignKeys.length + " foreign keys",
    };
  }
  // docs/22 W4.6: the tab's own create action rides the meta line (the W3.3 idiom) —
  // the same sheet family the table list's New table opens, prefilled with this table's
  // old state so the commit diffs against what is already there.
  const meta = el("div", "db-detail-meta");
  meta.appendChild(el("span", "", spec.meta));
  meta.appendChild(el("span", "grow"));
  if (d_.tab === "columns" || d_.tab === "indexes") {
    const add = el("button", "btn", d_.tab === "columns" ? "Add column…" : "New index…") as HTMLButtonElement;
    add.type = "button";
    add.onclick = (): void => {
      const d_ = d!;
      openDbDdlSheet(d_.tab === "columns" ? "column" : "index", {
        dialect: dbDialectOf(),
        conn: d_.conn!,
        schema: det.schema || "",
        table: det.table,
        columns: det.columns,
      });
    };
    meta.appendChild(add);
  }
  wrap.appendChild(meta);
  spec.head.forEach((h: string): void => { hr.appendChild(el("th", "db-col", h)); });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  spec.rows.forEach((r: unknown): void => {
    const tr = el("tr");
    spec.row(r).forEach((v: string | { text: unknown; cls?: string; fk?: ApiDbFkRow }): void => {
      const cell = v && typeof v === "object" ? v : { text: v, cls: "" };
      const td = el("td", "db-cell" + (cell.cls ? " " + cell.cls : ""));
      const fk_ = cell.fk!;
      if (cell.fk) {
        // docs/22 W5.2: the referenced table opens on click — no filter here, just the
        // navigation (the arrow in the grid header owns the filtered jump).
        const ref = el("button", "db-fk-ref") as HTMLButtonElement;
        ref.type = "button";
        ref.textContent = String(cell.text);
        ref.title = "Open " + (fk_.refSchema ? fk_.refSchema + "." : "") + fk_.refTable;
        ref.onclick = () => {
          if (!dbOkToDrop()) return;
          dbOpenTable({ name: cell.fk!.refTable, schema: cell.fk?.refSchema || null as unknown as string });
        };
        td.appendChild(ref);
      } else td.textContent = String(cell.text);
      tr.appendChild(td);
    });
    tbody.appendChild(tr);
  });
  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
}
/* --- DDL pretty-printing ------------------------------------------------------------------------ */
/* Re-flow a CREATE TABLE into the aligned layout IDEA uses: every column on its own line,
   names padded to a common width, types padded to a common width, so NOT NULL / DEFAULT
   line up down the page. Works on MySQL SHOW CREATE TABLE output and the Postgres sketch
   alike — both are one-definition-per-line already; this only adds the alignment. */
const DDL_MOD_WORDS = new Set((
  "NOT NULL DEFAULT AUTO_INCREMENT PRIMARY UNIQUE REFERENCES COMMENT CHARACTER COLLATE " +
  "GENERATED CHECK CONSTRAINT NULLS FIRST AFTER STORAGE INVISIBLE VISIBLE ON USING WITH"
).split(" "));

/** One column-definition line: quoted name, then type tokens, then modifiers. Returns null
 *  for lines that are not column definitions (PRIMARY KEY, KEY, CONSTRAINT, tail, etc). */
function dbParseColumnLine(line: string): { indent: string; name: string; type: string; mods: string; comma: boolean } | null {
  const m = /^(\s*)([`"][^`"]+[`"])\s+(.*?)(,?)\s*$/.exec(line);
  if (!m) return null;
  const rest = m[3];
  const tokens = rest.split(/\s+/).filter(Boolean);
  const typeParts: string[] = [];
  let i = 0;
  while (i < tokens.length) {
    const t = tokens[i];
    const upper = t.toUpperCase().replace(/\(.*$/, "");
    if (typeParts.length && DDL_MOD_WORDS.has(upper)) break;
    typeParts.push(t);
    i++;
    // swallow a parenthesized argument list that follows the type word (varchar(32), decimal(10,2))
    if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(t) && tokens[i] && /^\(/.test(tokens[i])) {
      // merge a split "(...)" spanning several tokens (spaces inside parens are rare but legal)
      let depth = 0, merged = "";
      while (i < tokens.length) {
        merged += tokens[i];
        depth += (tokens[i].match(/\(/g) || []).length - (tokens[i].match(/\)/g) || []).length;
        i++;
        if (depth === 0 && /\)/.test(merged)) break;
        if (depth === 0 && !/^\(/.test(merged)) break;
        merged += " ";
      }
      typeParts[typeParts.length - 1] = t + merged.replace(/\s+\)/g, ")").replace(/\(\s+/g, "(");
    }
  }
  const mods = tokens.slice(i).join(" ");
  if (!typeParts.length) return null;
  return { indent: m[1], name: m[2], type: typeParts.join(" "), mods: mods, comma: m[4] === "," };
}

function dbAlignDdl(ddl: string | null): string {
  if (!ddl) return ddl!;
  const lines = ddl.split(/\r?\n/);
  const parsed = lines.map((l) => { return dbParseColumnLine(l); });
  let wName = 0, wType = 0;
  parsed.forEach((p) => {
    if (!p) return;
    if (p.name.length > wName) wName = p.name.length;
    if (p.type.length > wType) wType = p.type.length;
  });
  if (!wName) return ddl; // nothing recognized — leave the DDL exactly as the server sent it
  return lines.map((l, i) => {
    const p = parsed[i];
    if (!p) return l.replace(/^\s{4}/, "  "); // constraints and the tail: gentle 2-space re-indent
    const out = "  " + p.name.padEnd(wName + 2) + p.type.padEnd(wType + 2) + p.mods.replace(/\s+/g, " ").trim();
    return out.replace(/\s+$/, "") + ",";
  }).join("\n");
}

export { DB_TABS, DDL_MOD_WORDS, dbAlignDdl, dbLoadDetail, dbParseColumnLine, dbRenderTabs, dbSetTab, renderDbDetailGrid };
