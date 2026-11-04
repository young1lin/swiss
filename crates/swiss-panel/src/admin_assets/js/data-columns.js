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

/* Column geometry the table and result grids share (SPEC §data.grid, §data.console): the header's
   resize grip - a drag sets a column's width, a double-click fits it to its content - the widths a
   result's columns were left at, and the finder that brings one column of a wide answer into
   sight. A table's widths are its grid config (data-grid.ts); a result has no table to file them
   under, so they are filed by the connection and the column list that answered: the same SELECT
   run again, or any query with the same columns, comes back at the widths it was left at. */
import { tr } from "./i18n.js";
import { popupMenu } from "./ui/menu.js";
import { el } from "./util.js";

export const DB_COL_MIN = 48; // narrower than the header's own name line and nothing reads
export const DB_COL_MAX = 1200; // wider than the grid itself - a runaway drag helps nobody

const RESULT_COLS_KEY = "swiss.dbResultCols";
const RESULT_COLS_KEEP = 50; // results remembered, the most recently resized kept

                                                                            

let stopGridTrack                          ;

/** Keep the vertical scrollbar below a row grid's header (SPEC §data.grid). All painters replace
 *  the same wrap; nested tables and Activity (no selection controls) own no inset. */
export function dbGridTrack(wrap                    )       {
  stopGridTrack?.();
  stopGridTrack = undefined;
  if (!wrap || typeof MutationObserver !== "function" || typeof ResizeObserver !== "function") return;
  let head                                 = null;
  const measure = ()       => {
    const height = head ? Math.ceil(head.getBoundingClientRect().height) : 0;
    const value = height + "px";
    if (wrap.style.getPropertyValue("--db-head-h") !== value) wrap.style.setProperty("--db-head-h", value);
  };
  const sizes = new ResizeObserver(measure);
  const watch = ()       => {
    const first = wrap.firstElementChild;
    const tableHead = first?.matches("table.db-grid") ? (first                    ).tHead : null;
    const next = tableHead?.querySelector(".db-selbox") ? tableHead : null;
    if (next !== head) {
      sizes.disconnect();
      head = next;
      if (head) sizes.observe(head);
    }
    measure();
  };
  const repaints = new MutationObserver(watch);
  repaints.observe(wrap, { childList: true });
  watch();
  stopGridTrack = ()       => { repaints.disconnect(); sizes.disconnect(); };
}

/** A result's key: FNV-1a over the connection and its column names, short for a long list. */
export function dbResultColsSig(conn        , columns          )         {
  const s = conn + "\u0001" + columns.join("\u0001");
  let x = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    x ^= s.charCodeAt(i);
    x = Math.imul(x, 0x01000193) >>> 0;
  }
  return x.toString(36);
}

function loadResultCols()             {
  try {
    const j          = JSON.parse(localStorage.getItem(RESULT_COLS_KEY) || "{}");
    return j && typeof j === "object" && !Array.isArray(j) ? j               : {};
  } catch (e) { return {}; }
}

/** The widths one result's columns were left at. A corrupt entry or a width outside the clamp
 *  is no width, as a table's grid config reads it. */
export function dbResultWidths(sig        )                         {
  const out                         = {};
  const e = loadResultCols()[sig]                                  ;
  if (!e || typeof e.w !== "object") return out;
  for (const k of Object.keys(e.w)) {
    const n = Number(e.w[k]);
    if (isFinite(n) && n >= DB_COL_MIN && n <= DB_COL_MAX) out[k] = Math.round(n);
  }
  return out;
}

/** File one column's width under its result; past RESULT_COLS_KEEP results the least recently
 *  resized goes. A blocked store keeps it for this page only. */
export function dbResultWidthSave(sig        , column        , w        )       {
  const all = loadResultCols();
  const had = all[sig]                                  ;
  const e = had && typeof had.w === "object" ? had : { w: {}, at: 0 };
  e.w[column] = w;
  e.at = Date.now();
  all[sig] = e;
  const keys = Object.keys(all).sort((a, b) => (Number(all[b].at) || 0) - (Number(all[a].at) || 0));
  for (const k of keys.slice(RESULT_COLS_KEEP)) delete all[k];
  try { localStorage.setItem(RESULT_COLS_KEY, JSON.stringify(all)); } catch (e2) { /* this page only */ }
}

/** Set a column's width on its cells. Each gets width AND maxWidth: under table-layout: auto a
 *  width alone is only a hint the nowrap content outvotes. */
export function dbColWidth(cells               , w        )       {
  for (const c of cells) {
    c.style.width = w + "px";
    c.style.maxWidth = w + "px";
  }
}

/** A column's cells: its header, then the cell at its place in every body row. */
export function dbColCells(th             )                {
  const row = th.parentElement;
  const table = th.closest("table");
  if (!row || !table) return [th];
  const at = Array.prototype.indexOf.call(row.children, th);
  const cells = [th];
  table.querySelectorAll("tbody tr").forEach((tr) => {
    const cell = tr.children[at]                           ;
    if (cell) cells.push(cell);
  });
  return cells;
}

/** The width that shows a column whole: its widest cell, header included, clamped. The limits a
 *  drag or the stylesheet's cap put on the cells are lifted while it measures, or it would only
 *  measure them. */
export function dbColFit(cells               )         {
  for (const c of cells) {
    c.style.width = "";
    c.style.maxWidth = "none";
  }
  let widest = 0;
  for (const c of cells) widest = Math.max(widest, c.scrollWidth);
  // One pixel over the measure, or a fractional width rounds down into the ellipsis.
  const w = Math.max(DB_COL_MIN, Math.min(DB_COL_MAX, Math.ceil(widest) + 1));
  dbColWidth(cells, w);
  return w;
}

/** Drag a column's right edge: the width follows the pointer across every cell of the column
 *  (captured at the press - no repaint per move) and is handed to `save` once, on release, if it
 *  changed. */
export function dbColDrag(e            , th             , cells               , save                     )       {
  e.preventDefault();
  e.stopPropagation();
  const startX = e.clientX;
  const startW = th.getBoundingClientRect().width;
  let lastW = startW;
  function move(ev            )       {
    lastW = Math.max(DB_COL_MIN, Math.min(DB_COL_MAX, Math.round(startW + ev.clientX - startX)));
    dbColWidth(cells, lastW);
  }
  function up()       {
    document.removeEventListener("mousemove", move);
    document.removeEventListener("mouseup", up);
    if (lastW !== Math.round(startW)) save(lastW);
  }
  document.addEventListener("mousemove", move);
  document.addEventListener("mouseup", up);
}

/** The header's resize grip, hugging its right edge: a drag sizes the column, a double-click fits
 *  it to its content, and either width goes to `save`. The grip owns its mousedown, so a drag
 *  never sorts the column it sizes (a table header's click sorts; #pane's delegated click
 *  passes over the grip). */
export function dbColGrip(th             , save                     )              {
  const grip = el("span", "db-col-grip");
  grip.title = tr("dataGrid.dragResize");
  grip.onmousedown = (ev            )       => { dbColDrag(ev, th, dbColCells(th), save); };
  grip.ondblclick = (ev            )       => {
    ev.preventDefault();
    ev.stopPropagation();
    save(dbColFit(dbColCells(th)));
  };
  return grip;
}

/** Bring a column into sight: scrolled to the grid's left edge when any of it is out of view,
 *  then its header underlined a moment, so the eye lands where the jump went. */
export function dbColReveal(th             )       {
  const box = th.closest             (".db-grid-wrap");
  if (box) {
    const r = th.getBoundingClientRect();
    const b = box.getBoundingClientRect();
    if (r.left < b.left || r.right > b.right) box.scrollLeft += r.left - b.left;
  }
  th.classList.add("db-hit");
  setTimeout(() => { th.classList.remove("db-hit"); }, 1500);
}

/** One finder row: a column's name, and what choosing it does. */
;                           
               
                                                                                            
                   
                 
 

/** The column finder (SPEC §data.console, §data.grid): every column of the grid in its order, a
 *  find box over them, and a row's choice brings its column into sight. */
export function dbColFinder(anchor             , cols             )       {
  popupMenu(anchor.getBoundingClientRect(), cols.map((c) => ({
    label: c.name, fn: c.go,
    icon: c.hidden ? "eye-off" : undefined,
    title: c.hidden ? tr("dataGrid.hiddenColumn") : undefined,
  })), { filter: tr("dataGrid.findColumn") });
}
