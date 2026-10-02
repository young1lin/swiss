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

import type { ApiDbColumn, ApiDbConnectionRow, ApiDbFkRow, ApiDbTableDetail } from "./types/api.js";
import type { DbDetailSpec } from "./types/state.js";
import { apiJson, dbReqGuard, el } from "./util.js";
import { fill, h } from "./h.js";
import { dbDialectOf, dbOkToDrop } from "./data-view.js";
import { dbOpenTab } from "./data-tabs.js";
import { openDbDdlSheet } from "./data-ddl.js";
import { dbTableMenu } from "./data-edit.js";
import { dbHighlightNodes, renderDbFilters } from "./data-filters.js";
import { renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { renderDbBar } from "./data-sql.js";
import { dbConn, dbTab } from "./db-state.js";
import { tk, tr, trn } from "./i18n.js";
import { btn } from "./ui/button.js";
import { seg } from "./ui/seg.js";

/* --- structure tabs (columns / indexes / DDL / foreign keys) ------------------------------------ */

/* tk()-marked tab labels (SPEC §panel.i18n): painted through tr(t.label) at render time. */
/* SPEC §data.tabs: the strip folds SIX panes into FOUR tabs — Data, Form,
 * Structure, DDL. Structure is one tab whose body is the three catalog tables
 * (Columns / Indexes / Foreign Keys) behind its own sub-segment; the pane ids
 * columns/indexes/fks stay, so every renderer and the detail load keep their ids. */
const DB_TABS = [
  { id: "data", label: tk("dataStructure.data") },
  { id: "form", label: tk("dataStructure.form") },
  { id: "structure", label: tk("dataStructure.structure") },
  { id: "ddl", label: tk("dataStructure.ddl") },
];

/* The Structure strip's pane ids: data | form | columns | indexes | fks | ddl — the six
 * fold into four with SPEC §data.tabs; "structure" itself resolves to columns when entered. */
const DB_PANES = ["data", "form", "columns", "indexes", "fks", "ddl"];

/** SPEC §data.tabs: the pane id a main-segment tab STANDS FOR — the three catalog pane ids
 *  all belong to the Structure tab. Pure. */
export function dbPaneToTab(pane: string): string {
  if (pane === "columns" || pane === "indexes" || pane === "fks") return "structure";
  return pane;
}

/** SPEC §data.tabs: the pane a main-segment tab OPENS — Structure lands on Columns. Pure. */
export function dbTabToPane(tab: string): string {
  return tab === "structure" ? "columns" : tab;
}

function dbSetTab(t: string): void {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "table") return; // the strip belongs to the open table tab
  const pane = dbTabToPane(t);
  if (d.pane === pane) return;
  // SPEC §data.grid: the form opens on the row the keyboard focused, and the grid's focus
  // returns to the form's row — one cursor, two presentations of it.
  if (t === "form" && d.focus) d.formIdx = d.focus.r;
  if (t === "data" && d.formIdx != null) d.focus = { r: d.formIdx, c: d.focus ? d.focus.c : 0 };
  d.pane = DB_PANES.includes(pane) ? pane : "data";
  renderDbToolbar();
  renderDbFilters();
  renderDbGrid();
  renderDbBar();
  if (pane !== "data" && pane !== "form" && c.conn && d.table) void dbLoadDetail();
}

// One /schema request chain: a slow answer for the table the user just left must be
// dropped, or it would paint the OLD table's structure (and rewrite d.schema) over the new
// one's (SPEC §data).
const dbDetailReq = dbReqGuard();

async function dbLoadDetail(): Promise<void> {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "table") return;
  if (!c.conn || !d.table) return;
  d.detailBusy = true;
  renderDbGrid();
  let q = "/api/db/" + encodeURIComponent(c.conn) + "/schema?table=" + encodeURIComponent(d.table);
  if (d.schema) q += "&schema=" + encodeURIComponent(d.schema);
  const token = dbDetailReq.issue();
  const j = await apiJson<ApiDbTableDetail>(q);
  if (!dbDetailReq.accepts(token)) return; // superseded: a newer table owns the detail
  d.detailBusy = false;
  if (!j) { d.detail = null; renderDbGrid(); return; }
  d.detail = j;
  d.schema = j.schema;
  renderDbGrid();
}

function dbRenderTabs(ctl: HTMLElement): void {
  const t = dbTab();
  const pane = t.kind === "table" ? t.pane : null;
  // Tab clicks and the Structure sub-segment answer through #pane's delegated listener via
  // their data-dtab addresses (SPEC §panel.toolchain) — no per-render handlers on the strip. The main
  // segment folds three catalog panes into Structure (SPEC §data.tabs): the selected mark reads
  // dbPaneToTab, the click sends the pane id the same handler already knew.
  ctl.appendChild(seg(DB_TABS.map((x: { id: string; label: string }) => { return { id: x.id, label: tr(x.label) }; }),
    pane != null ? dbPaneToTab(pane) : "", { key: "dtab" }));
}

/** The Structure tabs reuse the grid wrapper: Columns/Indexes/FKs render as plain tables,
 *  DDL as a monospace block. */
function renderDbDetailGrid(wrap: HTMLElement): void {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "table") return;
  if (d.detailBusy) { wrap.appendChild(el("div", "db-hint", tr("dataStructure.loading"))); return; }
  // SPEC §data.tabs: the three catalog panes are ONE tab's body now — the Structure sub-segment
  // (Columns / Indexes / Foreign Keys) heads each of the three, and its clicks ride the same
  // data-dtab address the main segment uses, so dbSetTab already knows every pane id.
  if (d.pane === "columns" || d.pane === "indexes" || d.pane === "fks") {
    if (!d.detail) { wrap.appendChild(el("div", "db-hint", tr("dataStructure.selectTableStructure"))); return; }
    wrap.appendChild(h("div", { class: "db-struct-sub" },
      seg([
        { id: "columns", label: tr("dataStructure.columns") },
        { id: "indexes", label: tr("dataStructure.indexes") },
        { id: "fks", label: tr("dataStructure.foreignKeys") },
      ], d.pane, { key: "dtab" })));
  }
  if (!d.detail) { wrap.appendChild(el("div", "db-hint", tr("dataStructure.selectTableStructure"))); return; }
  const det = d.detail;
  if (d.pane === "ddl") {
    wrap.appendChild(el("div", "db-detail-meta",
      (c.conn && c.conns.some((x: ApiDbConnectionRow): boolean => { return x.name === c.conn && x.dialect === "pg"; })
        ? tr("dataStructure.pgDdlFromCatalog")
        : tr("dataStructure.fromShowCreateTable"))));
    if (!det.ddl) { wrap.appendChild(el("div", "db-hint", tr("dataStructure.noDdl"))); return; }
    // The DDL as an editor shows it (SPEC §panel.code): the active scheme, numbered lines.
    const pre = el("pre", "db-ddl");
    fill(pre, dbHighlightNodes(dbAlignDdl(det.ddl), { lined: true }));
    wrap.appendChild(pre);
    return;
  }
  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  let spec: DbDetailSpec;
  if (d.pane === "columns") {
    spec = {
      head: [tr("dataFilters.column"), tr("dataDdl.type"), tr("dataDdl.nullable"),
        tr("dataDdl.default"), tr("dataView.sortKey"), tr("dataDdl.comment")],
      row: (c: ApiDbColumn) => {
        return [c.name, c.dataType, c.nullable ? "YES" : "NO",
          c.defaultValue == null ? "—" : String(c.defaultValue),
          c.isPrimaryKey ? "PRI" : "",
          // { text, cls } marks a cell that carries a class of its own: a real comment is the
          // quiet --text-2 the header caption and the hover card use for it (SPEC §panel.pages),
          // the "—" placeholder stays in the default cell color.
          c.comment == null ? "—" : { text: String(c.comment), cls: "db-det-comment" }];
      },
      rows: det.columns,
      meta: trn(det.columns.length, "dataStructure.nColumns.one", "dataStructure.nColumns.other") +
        " · " + tr("dataSql.primaryKey") + ": " + (det.primaryKey.join(", ") || tr("dataStructure.none")),
    };
  } else if (d.pane === "indexes") {
    spec = {
      head: [tr("dataStructure.index"), tr("dataDdl.unique"), tr("dataStructure.primary"), tr("dataStructure.columns")],
      row: (x: { name: string; unique: boolean; primary: boolean; columns: string[]; definition?: string }) => {
        return [x.name, x.unique ? "yes" : "no", x.primary ? "yes" : "no",
          (x.columns && x.columns.length ? x.columns.join(", ") : "") || (x.definition || "")];
      },
      rows: det.indexes,
      meta: trn(det.indexes.length, "dataStructure.nIndexes.one", "dataStructure.nIndexes.other"),
    };
  } else {
    spec = {
      head: [tr("dataStructure.constraint"), tr("dataFilters.column"), tr("dataStructure.references")],
      row: (f: ApiDbFkRow) => {
        // SPEC §data.grid: the target name carries its fk — the renderer draws it as a link
        // (read-only navigation; the grid header's arrow is the filtered jump).
        return [f.name, f.column, { text: f.refSchema + "." + f.refTable + " (" + f.refColumn + ")", fk: f }];
      },
      rows: det.foreignKeys,
      meta: trn(det.foreignKeys.length, "dataStructure.nForeignKeys.one", "dataStructure.nForeignKeys.other"),
    };
  }
  // SPEC §data.ddl: the tab's own create action rides the meta line (the W3.3 idiom) —
  // the same sheet family the table list's New table opens, prefilled with this table's
  // old state so the commit diffs against what is already there.
  const meta = el("div", "db-detail-meta");
  meta.appendChild(el("span", "", spec.meta));
  meta.appendChild(el("span", "grow"));
  if (d.pane === "columns" || d.pane === "indexes") {
    // data-dadd carries the sheet kind; the click handler resolves the live detail for the
    // sheet's payload (SPEC §panel.toolchain — state at event time, not render time).
    const colsTab = d.pane === "columns";
    meta.appendChild(btn(colsTab ? tr("dataStructure.addColumn") : tr("dataStructure.newIndex"), { data: { dadd: colsTab ? "column" : "index" } }));
  }
  wrap.appendChild(meta);
  spec.head.forEach((h: string): void => { hr.appendChild(el("th", "db-col", h)); });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  spec.rows.forEach((r: unknown): void => {
    /* Named trEl, not tr: this loop predates i18n, and the row element shadowed the
     * imported tr() the FK button's title needs (the 2026-10 i18n sweep hit exactly that). */
    const trEl = el("tr");
    spec.row(r).forEach((v: string | { text: unknown; cls?: string; fk?: ApiDbFkRow }): void => {
      const cell = v && typeof v === "object" ? v : { text: v, cls: "" };
      const td = el("td", "db-cell" + (cell.cls ? " " + cell.cls : ""));
      const fk_ = cell.fk!;
      if (cell.fk) {
        // SPEC §data.grid: the referenced table opens on click — no filter here, just the
        // navigation (the arrow in the grid header owns the filtered jump). The target rides
        // data attributes and the click is answered by #pane's delegated listener, which
        // matches [data-ref-table] and reads dataset.refTable. h() writes data keys verbatim,
        // so they are spelled kebab here: a camelCase key became data-reftable (HTML lowercases
        // attribute names), which neither the selector nor dataset.refTable ever saw - the
        // button was inert from the R5 conversion until the 2026-09-20 review.
        td.appendChild(h("button", {
          class: "db-fk-ref", type: "button",
          title: tr("dataStructure.openRefTable", { ref: (fk_.refSchema ? fk_.refSchema + "." : "") + fk_.refTable }),
          data: { "ref-table": fk_.refTable, "ref-schema": fk_.refSchema || "" },
        }, String(cell.text)));
      } else td.textContent = String(cell.text);
      trEl.appendChild(td);
    });
    tbody.appendChild(trEl);
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

/** #pane's delegated click for the Structure tab strip and the detail grids (SPEC §panel.toolchain).
 *  Behavior note (SPEC §panel.toolchain): the Add-column / New-index payload is built from the LIVE
 *  d.detail at event time — a detail that reloaded between render and click opens the sheet
 *  against what is on screen now, not what the button was painted with. */
function dbStructureClick(t: Element, ev: MouseEvent): boolean {
  const c = dbConn();
  const d = dbTab();
  const tabBtn = t.closest<HTMLElement>("[data-dtab]");
  if (tabBtn) { dbSetTab(tabBtn.dataset.dtab ?? ""); return true; }
  const menuBtn = t.closest<HTMLElement>("[data-tmenu]");
  if (menuBtn) {
    // stopPropagation: connect.js closes any open menu on clicks that reach document, and
    // without this the very click that opens the menu also tears it down.
    ev.stopPropagation();
    dbTableMenu(menuBtn);
    return true;
  }
  const add = t.closest<HTMLElement>("[data-dadd]");
  if (add) {
    const det = d.kind === "table" ? d.detail : null;
    if (det) {
      openDbDdlSheet(add.dataset.dadd as "column" | "index", {
        dialect: dbDialectOf(),
        conn: c.conn!,
        schema: det.schema || "",
        table: det.table,
        columns: det.columns,
      });
    }
    return true;
  }
  const ref = t.closest<HTMLElement>("[data-ref-table]");
  if (ref) {
    if (!dbOkToDrop()) return true;
    // The header writes data-ref-schema only for a cross-schema reference, so a same-schema
    // jump opens with schema null - exactly what the spec's schema field carries.
    dbOpenTab({ kind: "table", table: ref.dataset.refTable || "", schema: ref.dataset.refSchema || null });
    return true;
  }
  return false;
}

export { DB_TABS, DDL_MOD_WORDS, dbAlignDdl, dbLoadDetail, dbParseColumnLine, dbRenderTabs, dbSetTab, dbStructureClick, renderDbDetailGrid };
