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

             
                                                                                                                    
                                   
                        
                                                                         
                                                         
                                     
                                              
import { $, api, apiJson, dbReqGuard, el, emptyNode, toast } from "./util.js";
import { fill, h } from "./h.js";
import {
  cellText, columnsOf, idKey, mongoParse, MongoSyntaxError, parseDocument, sameValue, shellText, shellTokens, withoutId,
} from "./mongo-ejson.js";
import { dbConn, dbResetTabs, dbTab, dbTabs, freshTab } from "./db-state.js";
import { dbCurrentDatabase, dbFilterMatches, renderDbTables } from "./data-view.js";
import { dbAfterTabSwitch, dbOpenTab, renderDbTabs } from "./data-tabs.js";
import { renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { renderDbFilters } from "./data-filters.js";
import { dbCopyText } from "./data-csv.js";
import { dbOpenValueSheet } from "./data-value.js";
import { loadCollapsed, mountGroup } from "./groups.js";
import { locale, tr, trn } from "./i18n.js";
import { btn, iconBtn } from "./ui/button.js";
import { hint } from "./ui/form.js";
import { jsonCodeNode, tokenCodeNode, valueBlock } from "./ui/json-view.js";
import { popupMenu } from "./ui/menu.js";
import { seg } from "./ui/seg.js";
import { closeSheet, sheet, showSheet } from "./ui/sheet.js";
import { tag } from "./ui/status.js";
// The analysis panes live in their own module; the cycle is the accepted shape (function-level
// use only, never at module scope), the same as data-view <-> data-structure.
import {
  mongoExplainFind, mongoPaneHead, mongoPaneMore, mongoPanesClick, mongoPanesChange, mongoPanesInput, mongoPanesKeydown, renderMongoPane,
} from "./data-mongo-panes.js";

/* ================================================================================================
   The MongoDB flavour of the Data page (SPEC §data.mongo-panel).

   A mongo connection keeps the page's frame - the connection and database rows, the search box,
   the object strip, the console, Activity - and swaps what is inside it: the sidebar lists the
   database's collections and views, and a collection opens as a workspace with a query bar and
   six panes (Documents, Aggregations, Schema, Indexes, Explain, Validation; the last five live in
   data-mongo-panes.ts).

   Two rules hold everywhere below:
     - VALUES ARE SHELL SYNTAX ON SCREEN AND CANONICAL EXTENDED JSON ON THE WIRE (mongo-ejson.ts).
       The query bar, the editor and the pipeline stages read what the operator types with
       mongoParse, documents print with shellTokens, and nothing between the two ever turns an
       Int64 into a JS number.
     - A WRITE IS ADDRESSED BY THE WHOLE DOCUMENT IT WAS MADE AGAINST (SPEC §data.mongo-edits). The
       editor re-reads the document by _id before it opens (a projected page row is not the
       document), and a save whose original no longer matches answers 409 inside the sheet,
       never as a silent overwrite.
   ================================================================================================ */

/** The documents page sizes - the server's MONGO_PAGE_SIZES. */
export const MONGO_PAGE_SIZES = [10, 20, 50, 100, 200, 500];
/** A document longer than this many printed lines opens folded to them, with Show all. */
const DOC_LINES = 40;
/** UTF-8 bytes of JSON one import request carries at most (the gateway's body limit is 2 MiB). */
const IMPORT_REQUEST_BYTES = 1500000;
/** One document's UTF-8 JSON past this cannot ride any request, with the envelope, under 2 MiB. */
const IMPORT_DOC_BYTES = 2000000;
/** Documents one import request carries at most. */
const IMPORT_REQUEST_DOCS = 1000;
/** Field chips the query bar offers under the filter. */
const PATH_CHIPS = 12;
/** The query operators the filter's completion knows (the $ prefix). */
const QUERY_OPS = [
  "$eq", "$ne", "$gt", "$gte", "$lt", "$lte", "$in", "$nin", "$exists", "$type", "$regex", "$options",
  "$elemMatch", "$size", "$all", "$and", "$or", "$nor", "$not", "$expr", "$text", "$search", "$mod", "$jsonSchema",
];

/* --- the connection ------------------------------------------------------------------------------ */

/** A MongoDB connection is in front. */
export function dbIsMongo()          {
  const d = dbConn();
  const c = d.conns.find((x                    )          => x.name === d.conn);
  return !!c && c.dialect === "mongo";
}

function mongoOf(d             )                                    {
  if (!d.mongo) d.mongo = { colls: null, db: "", error: false, info: null, paths: {} };
  return d.mongo;
}

/** The database the sidebar is showing: the picked one, the connection's own, or the first
 *  user database the server listed. */
export function mongoCurrentDb()         {
  return dbCurrentDatabase(dbConn());
}

function base(conn        )         {
  return "/api/db/" + encodeURIComponent(conn) + "/mongo/";
}

async function mongoGet   (path        )                    {
  const d = dbConn();
  if (!d.conn) return null;
  return apiJson   (base(d.conn) + path);
}

async function mongoPost   (path        , body         )                    {
  const d = dbConn();
  if (!d.conn) return null;
  return apiJson   (base(d.conn) + path, { method: "POST", body: JSON.stringify(body) });
}

/** A POST whose failure the caller paints itself (a sheet keeps the error beside what was
 *  typed): the status and the server's own message, or the reply. */
async function mongoPostRaw   (path        , body         )                                                                                                   {
  const d = dbConn();
  if (!d.conn) return { ok: false, status: 0, error: tr("dataMongo.noConnection"), columns: [] };
  try {
    const r = await api(base(d.conn) + path, { method: "POST", body: JSON.stringify(body) });
    const j          = await r.json().catch(()          => ({}));
    if (r.ok) return { ok: true, body: j      };
    const o = (typeof j === "object" && j !== null ? j : {})                                                  ;
    return {
      ok: false, status: r.status,
      error: typeof o.error === "string" ? o.error : tr("util.httpN", { n: r.status }),
      columns: Array.isArray(o.conflictColumns) ? o.conflictColumns.map(String) : [],
    };
  } catch (e) {
    return { ok: false, status: 0, error: tr("util.requestFailedGatewayRunning"), columns: [] };
  }
}

/** The server's facts (version, topology, transactions, allowDestructive), once per connection. */
async function mongoLoadInfo()                {
  const d = dbConn();
  const m = mongoOf(d);
  if (m.info) return;
  const j = await mongoGet              ("info");
  if (j && dbConn() === d) { m.info = j; renderDbToolbar(); }
}

// One /collections chain: a database switch while a listing is on its way must not paint the
// old database's collections into the new one's tree.
const mongoCollReq = dbReqGuard();

/** The sidebar's catalog: the current database's collections with their statistics. `quiet`
 *  keeps the tree on screen until the answer is in (SPEC §data.sessions). */
export async function mongoLoadCollections(quiet          )                {
  const d = dbConn();
  if (!d.conn) return;
  void mongoLoadInfo();
  const db = mongoCurrentDb();
  const m = mongoOf(d);
  if (!db) {
    // The catalog has not answered yet: renderDbSide's lazy fetch brings it, and its repaint
    // lands back here through renderDbTables.
    m.colls = null;
    renderDbTables();
    return;
  }
  if (!quiet || m.db !== db) {
    m.colls = null;
    m.db = db;
    m.error = false; // a quiet refresh's failure is not this load's: it is loading until it answers
    renderDbTables();
  }
  const token = mongoCollReq.issue();
  const j = await mongoGet                       ("collections?db=" + encodeURIComponent(db));
  if (!mongoCollReq.accepts(token) || dbConn() !== d) return;
  m.error = !j;
  if (!j) {
    // A quiet refresh that failed keeps the catalog it had; a first load that failed leaves
    // none, so the tree says the listing failed instead of drawing an empty database.
    if (quiet && m.colls && m.db === db) return;
    m.colls = null;
    m.db = db;
    renderDbTables();
    renderDbToolbar();
    return;
  }
  const next = j.collections;
  if (quiet && m.colls && JSON.stringify(next) === JSON.stringify(m.colls)) return;
  m.colls = next;
  m.db = db;
  renderDbTables();
  renderDbToolbar();
}

/* --- the sidebar tree ----------------------------------------------------------------------------- */

const MONGO_SECTIONS = ["collections", "views", "system"];

function mongoSectionOf(c              )         {
  if (c.name.startsWith("system.")) return "system";
  return c.type === "view" ? "views" : "collections";
}

function mongoSectionLabel(s        )         {
  if (s === "collections") return tr("dataMongo.collections");
  if (s === "views") return tr("dataMongo.views");
  return tr("dataMongo.system");
}

function mongoSectionEmpty(s        )         {
  if (s === "collections") return tr("dataMongo.noCollections");
  if (s === "views") return tr("dataMongo.noViews");
  return tr("dataMongo.noSystem");
}

/** The list's order: name, count or size (client-side - the whole catalog is already here). */
function mongoSort(list                )                 {
  const d = dbConn();
  const dir = d.sortDir === "desc" ? -1 : 1;
  const num = (c              )         => (d.sort === "rows" ? c.count ?? -1 : d.sort === "size" ? c.size ?? -1 : 0);
  return list.slice().sort((a              , b              )         => {
    if (d.sort === "rows" || d.sort === "size") {
      const n = num(a) - num(b);
      if (n) return n * dir;
    }
    return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" }) * dir;
  });
}

/** Bytes as the sidebar's title and the head's meta say them. */
export function mongoBytes(n                           )         {
  if (n == null || !Number.isFinite(n)) return "";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return (v >= 100 || i === 0 ? Math.round(v).toLocaleString(locale()) : v.toFixed(1)) + " " + units[i];
}

function mongoCollRow(c              , selected               )              {
  const b = el("button", "db-table" + (c.name === selected ? " sel" : ""));
  const view = c.type === "view";
  const bits           = [];
  if (view) bits.push(tr("dataMongo.view"));
  else if (c.type === "timeseries") bits.push(tr("dataMongo.timeseries"));
  if (c.count != null) bits.push(tr("dataMongo.nDocs", { n: c.count.toLocaleString(locale()) }));
  if (c.size != null) bits.push(mongoBytes(c.size));
  if (c.viewOn) bits.push(tr("dataMongo.viewOn", { on: c.viewOn }));
  b.title = [c.name].concat(bits).join(" · ");
  b.dataset.mcoll = c.name;
  b.appendChild(el("span", "db-table-name", c.name));
  // The count right-aligned like a table row's; a view has none, so it says what it is.
  const meta = c.count != null ? c.count.toLocaleString(locale()) : view ? tr("dataMongo.view") : "";
  b.appendChild(el("span", "db-table-meta", meta));
  return b;
}

/** The sidebar for a mongo connection: Collections / Views / System bands over the current
 *  database, the search box filtering names client-side with the SQL tree's grammar. */
export function renderMongoTree(box             )       {
  const d = dbConn();
  const m = mongoOf(d);
  const foot = $("dbTablesPager");
  if (foot) foot.textContent = "";
  if (!m.colls) {
    box.appendChild(el("div", "db-hint", m.error ? tr("dataMongo.listFailed") : tr("dataView.loading")));
    return;
  }
  const t = dbTab();
  const selected = t.kind === "coll" && t.db === m.db ? t.coll : null;
  const shown = mongoSort(m.colls.filter((c              )          => !d.grep || dbFilterMatches(d.grep, c.name)));
  if (!shown.length) box.appendChild(el("div", "db-hint", d.grep ? tr("dataMongo.noMatchQ", { q: d.grep }) : tr("dataMongo.emptyDb")));
  const collapsed = loadCollapsed("dbtree.mongo");
  const cfg                         = {
    scope: "dbtree.mongo", density: "side",
    names: MONGO_SECTIONS.slice(), collapsed, noun: "collection",
    draggable: false, filtered: !!d.grep,
    label: mongoSectionLabel,
    canAdd: (g        )          => g === "collections" || g === "views",
    onAdd: (g        )       => { if (g === "views") openCreateCollection("createView"); else openCreateCollection("create"); },
    addTitle: (g        )         => (g === "views" ? tr("dataMongo.newView") : tr("dataMongo.newCollection")),
    moreItems: (g        )                    => (g === "collections" ? mongoSortMenu() : null),
    moreTitle: ()         => tr("dataView.sort"),
    emptyText: mongoSectionEmpty,
    reload: renderDbTables, render: renderDbTables,
    rowsById: ()                 => shown,
    groupOfRow: mongoSectionOf,
    rowNode: (c              )              => mongoCollRow(c, selected),
  };
  MONGO_SECTIONS.forEach((s        )       => {
    const rows = shown.filter((c              )          => mongoSectionOf(c) === s);
    // The system band appears only when it has something: an empty "System" on every database
    // would be noise, while empty Collections / Views bands say a true fact.
    if (s === "system" && !rows.length) return;
    box.appendChild(mountGroup(cfg, { name: s, rows }));
  });
  if (foot) {
    foot.appendChild(el("span", "", trn(m.colls.length, "dataMongo.nCollections.one", "dataMongo.nCollections.other", { n: m.colls.length.toLocaleString(locale()) })));
  }
}

function mongoSortMenu()             {
  const d = dbConn();
  const set = (key               , dir               )       => {
    if (key) d.sort = key;
    if (dir) d.sortDir = dir;
    renderDbTables();
  };
  return [
    { label: tr("dataView.sortName"), pick: true, on: d.sort !== "rows" && d.sort !== "size", fn: ()       => { set("name", null); } },
    { label: tr("dataMongo.sortCount"), pick: true, on: d.sort === "rows", fn: ()       => { set("rows", null); } },
    { label: tr("dataView.sortSize"), pick: true, on: d.sort === "size", fn: ()       => { set("size", null); } },
    { sep: true },
    { label: tr("dataView.sortAscending"), pick: true, on: d.sortDir !== "desc", fn: ()       => { set(null, "asc"); } },
    { label: tr("dataView.sortDescending"), pick: true, on: d.sortDir === "desc", fn: ()       => { set(null, "desc"); } },
  ];
}

/** The catalog row of the collection a tab shows, when the tree knows it. */
export function mongoCollInfo(t           )                      {
  const m = dbConn().mongo;
  if (!m || !m.colls || m.db !== t.db) return null;
  return m.colls.find((c              )          => c.name === t.coll) || null;
}

/** This connection allows commands that destroy data (the MCP's allowDestructive). */
export function mongoAllowsDestroy()          {
  const m = dbConn().mongo;
  return !!(m && m.info && m.info.allowDestructive);
}

/* --- the collection tab ----------------------------------------------------------------------------- */

/** The active tab when it is a collection workspace. */
export function mongoTab()                   {
  const t = dbTab();
  return t.kind === "coll" ? t : null;
}

/** The namespace a request about this tab carries. */
export function mongoNs(t           )                                     {
  return { db: t.db || "", collection: t.coll || "" };
}

/** The query as typed, read: `{filter, projection, sort}` documents or the first error. */
export function mongoQueryOf(t           )                                                                                                                                        {
  const f = parseDocument(t.qFilter);
  if (f.error) return { filter: {}, projection: null, sort: null, error: tr("dataMongo.filterError", { e: f.error }) };
  const p = parseDocument(t.qProject, null);
  if (p.error) return { filter: {}, projection: null, sort: null, error: tr("dataMongo.projectError", { e: p.error }) };
  const s = parseDocument(t.qSort, null);
  if (s.error) return { filter: {}, projection: null, sort: null, error: tr("dataMongo.sortError", { e: s.error }) };
  return { filter: f.value || {}, projection: p.value, sort: s.value, error: null };
}

// One /find chain across every collection tab: a slow page for a query the operator already
// replaced must not overwrite the page that replaced it.
const mongoFindReq = dbReqGuard();
const mongoCountReq = dbReqGuard();

/** Run the query bar's find into the tab: the page, then (in the background) the count. */
export async function mongoLoadDocs(t           , quiet          )                {
  if (!t.db || !t.coll) return;
  const q = mongoQueryOf(t);
  t.qError = q.error;
  if (q.error) { if (mongoTab() === t) { renderDbFilters(); renderDbGrid(); } return; }
  if (!quiet) { t.loading = true; if (mongoTab() === t) renderDbGrid(); }
  const token = mongoFindReq.issue();
  const body                          = { ...mongoNs(t), filter: q.filter, skip: t.qSkip, limit: t.qLimit };
  if (q.projection) body.projection = q.projection;
  if (q.sort) body.sort = q.sort;
  const j = await mongoPost                   ("find", body);
  if (!mongoFindReq.accepts(token)) return;
  t.loading = false;
  if (j) {
    const same = quiet && t.docs && JSON.stringify(t.docs) === JSON.stringify(j.docs) && t.more === j.more;
    t.docs = j.docs;
    t.more = j.more;
    t.elapsedMs = j.elapsedMs ?? null;
    mongoLearnPaths(t, j.docs);
    if (same) return;
  } else if (!t.docs) {
    t.docs = [];
  }
  if (mongoTab() === t) { renderDbToolbar(); renderDbFilters(); renderDbGrid(); }
  void mongoLoadCount(t, q.filter);
}

async function mongoLoadCount(t           , filter                       )                {
  const token = mongoCountReq.issue();
  const j = await mongoPost                    ("count", { ...mongoNs(t), filter });
  if (!mongoCountReq.accepts(token) || !j) return;
  t.total = j.total;
  t.estimated = j.estimated;
  t.countTimedOut = !!j.timedOut;
  if (mongoTab() === t) { renderDbToolbar(); renderDbGrid(); }
}

/** Field paths the query bar can complete, learned from every page this collection showed. */
function mongoLearnPaths(t           , docs                           )       {
  const m = mongoOf(dbConn());
  const key = (t.db || "") + "." + (t.coll || "");
  const known = m.paths[key] || [];
  const walk = (v         , prefix        , depth        )       => {
    if (depth > 4 || v === null || typeof v !== "object" || Array.isArray(v)) return;
    const o = v                           ;
    const first = Object.keys(o)[0];
    if (first !== undefined && first.charAt(0) === "$") return; // a wrapper is a value
    Object.keys(o).forEach((k        )       => {
      const p = prefix ? prefix + "." + k : k;
      if (!known.includes(p)) known.push(p);
      walk(o[k], p, depth + 1);
    });
  };
  docs.slice(0, 200).forEach((d                         )       => { walk(d, "", 0); });
  m.paths[key] = known.slice(0, 500);
}

/** Paths the Schema pane found are the best completion list there is. */
export function mongoTeachPaths(t           , paths          )       {
  const m = mongoOf(dbConn());
  const key = (t.db || "") + "." + (t.coll || "");
  const known = m.paths[key] || [];
  paths.forEach((p        )       => { if (!known.includes(p)) known.push(p); });
  m.paths[key] = known.slice(0, 500);
}

/* --- the head ------------------------------------------------------------------------------------- */

/** The head's left side: the collection, and what it is in one line. */
export function renderMongoHeadLeft(left             , t           )       {
  const d = dbConn();
  if (!t.coll) {
    left.appendChild(el("h2", "db-title", tr("dataMongo.collections")));
    left.appendChild(el("div", "db-meta", d.conn ? tr("dataMongo.pickCollection") : tr("dataGrid.databaseMcpRegistered")));
    return;
  }
  left.appendChild(el("h2", "db-title", t.coll));
  const info = mongoCollInfo(t);
  const bits           = [t.db || ""];
  if (info) {
    if (info.type !== "collection") bits.push(info.type);
    if (info.viewOn) bits.push(tr("dataMongo.viewOn", { on: info.viewOn }));
    if (info.count != null) bits.push(tr("dataMongo.nDocs", { n: info.count.toLocaleString(locale()) }));
    if (info.size != null) bits.push(mongoBytes(info.size));
    if (info.indexes != null) bits.push(trn(info.indexes, "dataMongo.nIndexes.one", "dataMongo.nIndexes.other", { n: info.indexes }));
    if (info.capped) bits.push(tr("dataMongo.capped"));
  }
  const srv = dbConn().mongo?.info;
  if (srv) bits.push(tr("dataMongo.serverLine", { v: srv.version, t: srv.topology }));
  left.appendChild(el("div", "db-meta", bits.filter(Boolean).join("  ·  ")));
}

/** The head's controls: the pane switch, the pane's one primary action, the overflow. */
export function renderMongoHeadCtl(ctl             , t           , more                                          )       {
  if (!t.coll) return;
  const view = mongoCollInfo(t)?.type === "view";
  ctl.appendChild(seg([
    { id: "docs", label: tr("dataMongo.paneDocs") },
    { id: "agg", label: tr("dataMongo.paneAgg") },
    { id: "schema", label: tr("dataMongo.paneSchema") },
    { id: "indexes", label: tr("dataMongo.paneIndexes"), hidden: view },
    { id: "explain", label: tr("dataMongo.paneExplain") },
    { id: "validation", label: tr("dataMongo.paneValidation"), hidden: view },
  ], t.pane, { key: "mpane", label: tr("dataMongo.panes") }));
  if (t.pane === "docs") {
    if (!view) ctl.appendChild(btn(tr("dataMongo.addDocument"), { title: tr("dataMongo.addDocumentTitle"), data: { madd: "" } }));
  } else {
    const primary = mongoPaneHead(t);
    if (primary) ctl.appendChild(primary);
  }
  ctl.appendChild(more(()             => mongoMoreItems(t)));
}

/** The workspace's overflow: the pane's own rare acts, then the documents' bulk acts and
 *  transfers, then the collection's own. */
function mongoMoreItems(t           )             {
  const view = mongoCollInfo(t)?.type === "view";
  const items             = [
    { label: tr("dataGrid.refresh"), fn: ()       => { void mongoRefresh(t); } },
  ];
  const paneItems = mongoPaneMore(t);
  if (paneItems.length) { items.push({ sep: true }); items.push(...paneItems); }
  items.push({ sep: true });
  items.push({ heading: true, label: tr("dataMongo.exportHeading"), fn: ()       => {} });
  items.push({ label: tr("dataMongo.exportJson"), fn: ()       => { void mongoExport(t, "json", false); } });
  items.push({ label: tr("dataMongo.exportJsonCanonical"), fn: ()       => { void mongoExport(t, "json", true); } });
  items.push({ label: tr("dataMongo.exportNdjson"), fn: ()       => { void mongoExport(t, "ndjson", false); } });
  items.push({ label: tr("dataMongo.exportCsv"), fn: ()       => { void mongoExport(t, "csv", false); } });
  if (!view) {
    items.push({ sep: true });
    items.push({ label: tr("dataMongo.import"), fn: ()       => { openImport(t); } });
    items.push({ label: tr("dataMongo.updateMatching"), fn: ()       => { openBulkUpdate(t); } });
    items.push({ label: tr("dataMongo.deleteMatching"), danger: true, fn: ()       => { void mongoBulkDelete(t); } });
  }
  items.push({ sep: true });
  items.push({ label: tr("dataMongo.renameCollection"), fn: ()       => { openRenameCollection(t); } });
  items.push({
    label: tr("dataMongo.dropCollection"), danger: true,
    title: mongoAllowsDestroy() ? undefined : tr("dataMongo.dropNeedsDestructive"),
    disabled: !mongoAllowsDestroy(),
    fn: ()       => { void mongoDropCollection(t); },
  });
  items.push({ sep: true });
  items.push({ label: tr("dataGrid.command"), fn: ()       => { dbOpenTab({ kind: "sql" }); } });
  items.push({ label: tr("dataView.activity"), fn: ()       => { dbOpenTab({ kind: "activity" }); } });
  return items;
}

async function mongoRefresh(t           )                {
  if (t.pane === "docs") await mongoLoadDocs(t);
  else if (t.pane === "explain") mongoExplainFind(t);
  else renderDbGrid();
  void mongoLoadCollections(true);
}

/* --- the query bar ----------------------------------------------------------------------------------- */

/** The filter, project and sort inputs (mono, shell syntax), skip/limit behind Options, the
 *  view switch, and the field chips the filter completes from. */
export function mongoFilterNodes(t           )           {
  if (!t.coll || (t.pane !== "docs" && t.pane !== "explain")) return [];
  const input = (k        , value        , ph        , title        , grow         )                   =>
    h("input", {
      type: "text", value, placeholder: ph, title, spellcheck: false, autocomplete: "off",
      style: "font-family:var(--mono)" + (grow ? ";flex:1;min-width:12em" : ";width:14em"),
      data: { mq: k },
    });
  const rows           = [
    h("div", { class: "db-filter", style: "display:flex;width:100%" },
      h("span", { class: "db-filter-hint" }, tr("dataMongo.filter")),
      input("filter", t.qFilter, "{ field: \"value\" }", tr("dataMongo.filterTitle"), true),
      btn(tr("dataMongo.find"), { kind: "primary", title: tr("dataMongo.findTitle"), data: { mfind: "" } }),
      iconBtn("x", tr("dataMongo.resetQuery"), { ghost: true, data: { mreset: "" } }),
      iconBtn(t.qMore ? "chevron-up" : "chevron-down", tr("dataMongo.options"), { ghost: true, pressed: t.qMore, data: { mopts: "" } })),
  ];
  if (t.qMore) {
    rows.push(h("div", { class: "db-filter", style: "display:flex;width:100%;flex-wrap:wrap" },
      h("span", { class: "db-filter-hint" }, tr("dataMongo.project")),
      input("project", t.qProject, "{ name: 1, _id: 0 }", tr("dataMongo.projectTitle"), true),
      h("span", { class: "db-filter-hint" }, tr("dataMongo.sort")),
      input("sort", t.qSort, "{ createdAt: -1 }", tr("dataMongo.sortTitle"), true),
      h("span", { class: "db-filter-hint" }, tr("dataMongo.skip")),
      h("input", { type: "number", min: "0", value: String(t.qSkip), style: "width:6em", data: { mq: "skip", shown: String(t.qSkip) }, title: tr("dataMongo.skipTitle") })));
  }
  if (t.qError) rows.push(hint(t.qError, { bad: true, live: true }));
  const chips = mongoPathChips(t);
  if (chips.length) {
    rows.push(h("div", { class: "db-filter", style: "flex-wrap:wrap" },
      h("span", { class: "db-filter-hint" }, tr("dataMongo.fields")),
      chips.map((p        )         => btn(p, { kind: "ghost", title: tr("dataMongo.insertField"), data: { mpath: p } }))));
  }
  if (t.pane === "docs") {
    rows.push(h("div", { class: "db-filter", style: "margin-left:auto" },
      seg([
        { id: "list", label: tr("dataMongo.viewList") },
        { id: "json", label: tr("dataMongo.viewJson") },
        { id: "table", label: tr("dataMongo.viewTable") },
      ], t.view, { key: "mview", label: tr("dataMongo.view") })));
  }
  return rows;
}

/** The word the filter's caret sits at the end of: a field-name prefix or a `$` operator. */
function mongoWordAt(text        , caret        )         {
  const m = /[$\w.]*$/.exec(text.slice(0, caret));
  return m ? m[0] : "";
}

/** The completion the filter offers right now: operators after a `$`, field paths otherwise,
 *  the paths the collection is known to hold that start with what is typed. Pure enough to
 *  pin: it reads the tab and the learned paths. */
export function mongoCompletions(t           , word        )           {
  if (word.charAt(0) === "$") return QUERY_OPS.filter((o        )          => o.startsWith(word) && o !== word);
  const key = (t.db || "") + "." + (t.coll || "");
  const paths = (dbConn().mongo?.paths[key] || []).filter((p        )          => p !== word);
  const w = word.toLowerCase();
  const hits = w ? paths.filter((p        )          => p.toLowerCase().startsWith(w)) : paths.filter((p        )          => !p.includes("."));
  return hits.slice(0, PATH_CHIPS);
}

let mongoCaret = 0;

function mongoPathChips(t           )           {
  return mongoCompletions(t, mongoWordAt(t.qFilter, Math.min(mongoCaret || t.qFilter.length, t.qFilter.length)));
}

/** Put a field path (or operator) into the filter at the caret, replacing the word it completes. */
function mongoInsertPath(t           , path        )       {
  const input = document.querySelector                  ("[data-mq=filter]");
  const text = t.qFilter;
  const caret = input && input.selectionStart != null ? input.selectionStart : text.length;
  const word = mongoWordAt(text, caret);
  let before = text.slice(0, caret - word.length);
  const after = text.slice(caret);
  let insert = path;
  if (!before.trim()) { before = "{ "; insert += ": "; }
  else if (path.charAt(0) !== "$") insert = (IDENT_PATH.test(path) ? path : JSON.stringify(path)) + ": ";
  else insert += ": ";
  t.qFilter = before + insert + after + (before === "{ " && !after.includes("}") ? " }" : "");
  mongoCaret = (before + insert).length;
  renderDbFilters();
  const again = document.querySelector                  ("[data-mq=filter]");
  if (again) { again.focus(); again.selectionStart = again.selectionEnd = mongoCaret; }
}

const IDENT_PATH = /^[A-Za-z_$][\w$]*$/;

/* --- the documents pane ------------------------------------------------------------------------------ */

/** The workspace body: the pane in front. */
export function renderMongoBody(wrap             )       {
  const t = mongoTab();
  const d = dbConn();
  if (!t) return;
  if (!d.conn) { wrap.appendChild(el("div", "db-hint", tr("dataGrid.databaseMcpRegisteredAdd"))); return; }
  if (!t.coll) {
    wrap.appendChild(emptyNode({ icon: "braces", title: tr("dataMongo.pickCollectionTitle"), hint: tr("dataMongo.pickCollectionHint") }));
    return;
  }
  if (t.pane !== "docs") { renderMongoPane(wrap, t); return; }
  if (t.loading && !t.docs) { wrap.appendChild(el("div", "db-hint", tr("dataGrid.loading"))); return; }
  const docs = t.docs || [];
  if (!docs.length) {
    wrap.appendChild(emptyNode({
      icon: "braces",
      title: t.qFilter.trim() ? tr("dataMongo.noMatches") : tr("dataMongo.noDocuments"),
      hint: t.qFilter.trim() ? tr("dataMongo.noMatchesHint") : tr("dataMongo.noDocumentsHint"),
    }));
    return;
  }
  if (t.view === "json") { renderMongoJson(wrap, docs); return; }
  if (t.view === "table") { renderMongoTable(wrap, t, docs); return; }
  const list = h("div", { style: "display:flex;flex-direction:column;gap:var(--s2);padding:var(--s2) var(--s3)" });
  docs.forEach((doc                         , i        )       => { list.appendChild(mongoDocCard(t, doc, i)); });
  wrap.appendChild(list);
}

/** One document as a labelled code block: its _id as the caption, its acts at the end of
 *  the line, the document itself in shell syntax. */
function mongoDocCard(t           , doc                         , i        )              {
  const view = mongoCollInfo(t)?.type === "view";
  const tools           = [
    iconBtn("copy", tr("dataMongo.copyDoc"), { ghost: true, data: { mdoc: String(i), mact: "copy" } }),
  ];
  if (!view) {
    tools.push(iconBtn("pencil", tr("dataMongo.editDoc"), { ghost: true, data: { mdoc: String(i), mact: "edit" } }));
    tools.push(iconBtn("plus", tr("dataMongo.cloneDoc"), { ghost: true, data: { mdoc: String(i), mact: "clone" } }));
    tools.push(iconBtn("trash", tr("dataMongo.deleteDoc"), { ghost: true, data: { mdoc: String(i), mact: "delete" } }));
  }
  const all = mongoShowAll.has(idKey(doc));
  const code = tokenCodeNode(shellTokens(doc), { all, limit: DOC_LINES });
  const total = code.lines;
  // The caption is the document's place in the result (the library uppercases captions, which
  // would misspell an id's hex); the _id itself rides the notes, as typed.
  const notes = [
    "_id" in doc ? cellText(doc._id, 80) : tr("dataMongo.noId"),
    trn(Object.keys(doc).length, "dataMongo.nFields.one", "dataMongo.nFields.other"),
  ];
  return valueBlock({ label: "#" + String(t.qSkip + i + 1), notes, tools, data: { mcard: String(i) } },
    code.node,
    !all && total > DOC_LINES ? h("div", null, btn(tr("logs.showAllLines", { n: total }), { kind: "ghost", data: { mshow: idKey(doc) } })) : null);
}

/* The documents whose Show all was pressed - by idKey, for as long as the page lives. */
const mongoShowAll = new Set        ();

/** The page as one canonical Extended JSON array - what a lossless copy takes. */
function renderMongoJson(wrap             , docs                           )       {
  const code = jsonCodeNode(docs, true);
  wrap.appendChild(h("div", { style: "padding:var(--s2) var(--s3)" },
    h("div", { class: "db-console-row" },
      btn(tr("dataMongo.copyPage"), { icon: "copy", data: { mcopypage: "" } }),
      hint(tr("dataMongo.jsonHint"))),
    code.node));
}

/** The page as a grid: the top-level fields as columns, nested values summarised, a double
 *  click opening the full value. */
function renderMongoTable(wrap             , t           , docs                           )       {
  const cols = columnsOf(docs);
  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  hr.appendChild(el("th", "db-rowctl", "#"));
  cols.forEach((c        )       => { hr.appendChild(el("th", "db-col", c)); });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  docs.forEach((doc                         , i        )       => {
    const tri = el("tr");
    const ctl = el("td", "db-rowctl");
    ctl.appendChild(iconBtn("pencil", tr("dataMongo.editDoc"), { ghost: true, data: { mdoc: String(i), mact: "edit" } }));
    ctl.appendChild(document.createTextNode(String(t.qSkip + i + 1)));
    tri.appendChild(ctl);
    cols.forEach((c        )       => {
      const has = c in doc;
      const td = el("td", "db-cell", has ? cellText(doc[c]) : "");
      td.title = has ? shellText(doc[c], { oneLine: true }).slice(0, 2000) : tr("dataMongo.missingField");
      // Per node, like the row grid's cells (data-grid.ts): the pane carries no dblclick listener.
      if (has) td.ondblclick = ()       => { dbOpenValueSheet(c, shellText(doc[c]), (t.coll || "") + " · " + cellText(doc._id, 60)); };
      tri.appendChild(td);
    });
    tbody.appendChild(tri);
  });
  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
}

/** The status line: page size, the window, the count (estimated, exact, or unknown in time),
 *  and the pager. */
export function renderMongoStatus(bar             , t           )       {
  if (!t.coll) return;
  if (t.pane !== "docs") return;
  bar.appendChild(h("select", { class: "db-pagesize", title: tr("dataGrid.rowsPage"), data: { mpagesize: "" } },
    MONGO_PAGE_SIZES.map((n        )         => h("option", { value: String(n), selected: n === t.qLimit }, String(n)))));
  const n = t.docs ? t.docs.length : 0;
  const a = n ? t.qSkip + 1 : 0;
  const b = t.qSkip + n;
  const total = t.total == null
    ? (t.countTimedOut ? tr("dataMongo.countTimedOut") : "…")
    : (t.estimated ? "~" : "") + t.total.toLocaleString(locale());
  bar.appendChild(el("span", "db-pageinfo", tr("dataMongo.window", { a: a.toLocaleString(locale()), b: b.toLocaleString(locale()), t: total })));
  bar.appendChild(iconBtn("chevron-left", tr("dataGrid.previousPage"), { disabled: t.qSkip === 0, data: { mpg: "prev" } }));
  bar.appendChild(iconBtn("chevron-right", tr("dataGrid.nextPage"), { disabled: !t.more, data: { mpg: "next" } }));
  if (t.elapsedMs != null) bar.appendChild(el("span", "db-status-note", tr("dataGrid.msMs", { ms: t.elapsedMs })));
}

/* --- delegated listeners (SPEC §panel.toolchain) ---------------------------------------------------- */

/** #pane's click for everything mongo: the tree, the query bar, the cards, the pager. */
export function mongoClick(t         , ev            )          {
  const row = t.closest             ("[data-mcoll]");
  if (row) {
    const db = mongoCurrentDb();
    if (db && row.dataset.mcoll) dbOpenTab({ kind: "coll", db, coll: row.dataset.mcoll });
    return true;
  }
  const tab = mongoTab();
  if (!tab) return false;
  const pane = t.closest             ("[data-mpane]");
  if (pane && pane.dataset.mpane) {
    const next = pane.dataset.mpane                     ;
    if (next !== tab.pane) { tab.pane = next; mongoAfterPane(tab); }
    return true;
  }
  const view = t.closest             ("[data-mview]");
  if (view && view.dataset.mview) {
    tab.view = view.dataset.mview                     ;
    renderDbFilters(); renderDbGrid();
    return true;
  }
  if (t.closest("[data-mfind]")) {
    // A new query starts from the first page - unless the operator typed a skip of their own.
    // The Options field is drawn holding the current offset (data-shown); untouched, it still does.
    const skip = document.querySelector                  ("[data-mq=skip]");
    mongoReadInputs(tab);
    if (!skip || skip.value === skip.dataset.shown) tab.qSkip = 0;
    mongoRunQuery(tab);
    return true;
  }
  if (t.closest("[data-mreset]")) {
    tab.qFilter = ""; tab.qProject = ""; tab.qSort = ""; tab.qSkip = 0; tab.qError = null;
    renderDbFilters();
    mongoRunQuery(tab);
    return true;
  }
  if (t.closest("[data-mopts]")) { mongoReadInputs(tab); tab.qMore = !tab.qMore; renderDbFilters(); return true; }
  const chip = t.closest             ("[data-mpath]");
  if (chip && chip.dataset.mpath) { mongoReadInputs(tab); mongoInsertPath(tab, chip.dataset.mpath); return true; }
  const pg = t.closest             ("[data-mpg]");
  if (pg) {
    if (pg.dataset.mpg === "prev") tab.qSkip = Math.max(0, tab.qSkip - tab.qLimit);
    else if (tab.more) tab.qSkip += tab.qLimit;
    void mongoLoadDocs(tab);
    return true;
  }
  if (t.closest("[data-madd]")) { openInsertSheet(tab, null); return true; }
  if (t.closest("[data-mcopypage]")) { dbCopyText(JSON.stringify(tab.docs || [], null, 2)); return true; }
  const show = t.closest             ("[data-mshow]");
  if (show && show.dataset.mshow) { mongoShowAll.add(show.dataset.mshow); renderDbGrid(); return true; }
  const act = t.closest             ("[data-mact]");
  if (act) {
    const doc = (tab.docs || [])[Number(act.dataset.mdoc)];
    if (!doc) return true; // a stale address: the page moved under the click
    const what = act.dataset.mact;
    if (what === "copy") dbCopyText(shellText(doc));
    else if (what === "edit") void openDocEditor(tab, doc);
    else if (what === "clone") openInsertSheet(tab, shellText(withoutId(doc                         )));
    else if (what === "delete") void mongoDeleteDoc(tab, doc);
    return true;
  }
  if (mongoPanesClick(t, ev, tab)) return true;
  return false;
}

/** Typing in the query bar keeps the text on the tab (it outlives a repaint) and clears a
 *  stale error; the chips follow the word at the caret. */
export function mongoInput(t         )          {
  const tab = mongoTab();
  if (!tab) return false;
  const q = t.closest                  ("[data-mq]");
  if (q) {
    const k = q.dataset.mq;
    if (k === "filter") { tab.qFilter = q.value; mongoCaret = q.selectionStart ?? q.value.length; mongoRepaintChips(tab); }
    else if (k === "project") tab.qProject = q.value;
    else if (k === "sort") tab.qSort = q.value;
    else if (k === "skip") tab.qSkip = Math.max(0, Math.floor(Number(q.value) || 0));
    return true;
  }
  return mongoPanesInput(t, tab);
}

/** Repaint the chip line in place: rebuilding the bar would take the caret out of the input. */
function mongoRepaintChips(tab           )       {
  const box = $("dbFilters");
  if (!box) return;
  const line = box.querySelector             ("[data-mpath]")?.parentElement || null;
  const chips = mongoPathChips(tab);
  if (!line) {
    if (!chips.length) return;
    // No chip line yet: build one after the filter row without touching the inputs.
    const row = h("div", { class: "db-filter", style: "flex-wrap:wrap" }, h("span", { class: "db-filter-hint" }, tr("dataMongo.fields")));
    chips.forEach((p        )       => { row.appendChild(btn(p, { kind: "ghost", title: tr("dataMongo.insertField"), data: { mpath: p } })); });
    const first = box.firstElementChild;
    if (first && first.nextSibling) box.insertBefore(row, first.nextSibling); else box.appendChild(row);
    return;
  }
  fill(line, h("span", { class: "db-filter-hint" }, tr("dataMongo.fields")),
    chips.map((p        )         => btn(p, { kind: "ghost", title: tr("dataMongo.insertField"), data: { mpath: p } })));
}

export function mongoChange(t         )          {
  const tab = mongoTab();
  if (!tab) return false;
  const ps = t.closest                   ("[data-mpagesize]");
  if (ps) { tab.qLimit = Number(ps.value) || 20; tab.qSkip = 0; void mongoLoadDocs(tab); return true; }
  return mongoPanesChange(t, tab);
}

/** Enter in the query bar runs the find; Tab in the filter takes the first completion. */
export function mongoKeydown(t         , ev               )          {
  const tab = mongoTab();
  if (!tab) return false;
  const q = t.closest                  ("[data-mq]");
  if (q) {
    if (ev.key === "Enter") { ev.preventDefault(); mongoReadInputs(tab); tab.qSkip = q.dataset.mq === "skip" ? tab.qSkip : 0; mongoRunQuery(tab); return true; }
    if (ev.key === "Tab" && !ev.shiftKey && q.dataset.mq === "filter") {
      const word = mongoWordAt(q.value, q.selectionStart ?? q.value.length);
      const first = mongoCompletions(tab, word)[0];
      if (word && first) { ev.preventDefault(); tab.qFilter = q.value; mongoInsertPath(tab, first); }
      return true;
    }
    return true;
  }
  return mongoPanesKeydown(t, ev, tab);
}

/** Read every query input's live value into the tab (a click may land before an input event). */
function mongoReadInputs(tab           )       {
  document.querySelectorAll                  ("[data-mq]").forEach((q                  )       => {
    if (q.dataset.mq === "filter") tab.qFilter = q.value;
    else if (q.dataset.mq === "project") tab.qProject = q.value;
    else if (q.dataset.mq === "sort") tab.qSort = q.value;
    else if (q.dataset.mq === "skip") tab.qSkip = Math.max(0, Math.floor(Number(q.value) || 0));
  });
}

/** The query bar's Find: the documents, or - with the Explain pane in front - that find's plan. */
function mongoRunQuery(tab           )       {
  if (tab.pane === "explain") mongoExplainFind(tab);
  else void mongoLoadDocs(tab);
}

/** A pane switch: the chrome, and whatever the pane has not fetched yet. */
function mongoAfterPane(tab           )       {
  renderDbToolbar(); renderDbFilters(); renderDbGrid();
  if (tab.pane === "docs" && !tab.docs) void mongoLoadDocs(tab);
}

/** What a collection tab owes the pane when it comes to the front (dbAfterTabSwitch). */
export function mongoAfterTabSwitch(t           )       {
  if (!t.coll) return;
  if (t.pane === "docs" && !t.docs) void mongoLoadDocs(t);
  else renderDbGrid();
}

/* --- the editor and the other document sheets -------------------------------------------------------- */

/** The document as the server has it now (by _id, every field), or null once reported. */
async function mongoFetchDoc(t           , doc                         )                                          {
  if (!("_id" in doc)) { toast(tr("dataMongo.noIdCannotEdit"), true); return null; }
  const j = await mongoPost                   ("find", { ...mongoNs(t), filter: { _id: doc._id }, limit: 1 });
  if (!j) return null;
  const fresh = j.docs[0];
  if (!fresh) { toast(tr("dataMongo.docGone"), true); void mongoLoadDocs(t); return null; }
  return fresh;
}

/** A text sheet: a mono textarea, an error line under it, Cancel and one primary. `save`
 *  answers null when done, or the message to paint. */
function textSheet(o                                                                                                                                            )       {
  showSheet(sheet({
    title: o.title,
    sub: o.sub,
    label: o.title,
    body: [
      hint(o.note),
      h("textarea", { id: "mgText", spellcheck: false, style: "min-height:" + (o.rows || 18) + "em;width:100%" }),
      hint("", { id: "mgErr", bad: true, live: true, hidden: true }),
    ],
    foot: [h("span", { class: "grow" }), btn(tr("dataCell.cancel"), { id: "mgCancel" }), btn(o.primary, { kind: "primary", id: "mgSave" })],
  }));
  const ta = $                     ("mgText");
  ta.value = o.text;
  const err = $("mgErr");
  let busy = false;
  const run = async ()                => {
    if (busy) return;
    busy = true;
    const said = await o.save(ta.value);
    busy = false;
    if (said === null) { closeSheet(); return; }
    err.textContent = said;
    err.hidden = false;
  };
  $("mgCancel").onclick = ()       => { closeSheet(); };
  $("mgSave").onclick = ()       => { void run(); };
  ta.onkeydown = (e               )       => {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); void run(); }
  };
  ta.oninput = ()       => { err.hidden = true; };
  ta.focus();
}

function parseFailure(e         )         {
  if (e instanceof MongoSyntaxError) return tr("dataMongo.syntaxAt", { e: e.message, l: e.line, c: e.col });
  return String(e);
}

/** Edit one document (SPEC §data.mongo-edits): re-read it whole, edit it as shell text, write it
 *  back with the read version as the optimistic lock. A save that changed nothing is a close. */
async function openDocEditor(t           , doc                         )                {
  const original = await mongoFetchDoc(t, doc);
  if (!original) return;
  textSheet({
    title: tr("dataMongo.editDocument"),
    sub: (t.coll || "") + " · " + cellText(original._id, 60),
    text: shellText(original),
    note: tr("dataMongo.editNote"),
    primary: tr("dataMongo.saveDocument"),
    save: async (text        )                         => {
      let next       ;
      try { next = mongoParse(text); } catch (e) { return parseFailure(e); }
      if (next === null || typeof next !== "object" || Array.isArray(next)) return tr("dataMongo.notADocument");
      if (sameValue(next, original)) return null;
      const r = await mongoPostRaw                     ("edits", { ...mongoNs(t), edits: [{ op: "replace", original, doc: next }] });
      if (!r.ok) return r.status === 409 ? tr("dataMongo.conflict", { e: r.error }) : r.error;
      toast(tr("dataMongo.saved"));
      void mongoLoadDocs(t, true);
      return null;
    },
  });
}

/** Insert one document, or several: a document, or an array of them, in one commit (a
 *  transaction where the deployment has them). `prefill` is a clone's body. */
function openInsertSheet(t           , prefill               )       {
  textSheet({
    title: tr("dataMongo.insertDocument"),
    sub: (t.db || "") + "." + (t.coll || ""),
    text: prefill ?? "{\n  _id: ObjectId(),\n  \n}",
    note: tr("dataMongo.insertNote"),
    primary: tr("dataMongo.insert"),
    save: async (text        )                         => {
      let v       ;
      try { v = mongoParse(text); } catch (e) { return parseFailure(e); }
      const docs = Array.isArray(v) ? v : [v];
      if (!docs.length || docs.some((d       )          => d === null || typeof d !== "object" || Array.isArray(d))) return tr("dataMongo.notADocument");
      if (docs.length > 1000) return tr("dataMongo.tooManyInserts");
      const r = await mongoPostRaw                     ("edits", { ...mongoNs(t), edits: docs.map((doc       ) => ({ op: "insert", doc })) });
      if (!r.ok) return r.error;
      toast(trn(docs.length, "dataMongo.insertedN.one", "dataMongo.insertedN.other"));
      void mongoLoadDocs(t, true);
      void mongoLoadCollections(true);
      return null;
    },
  });
}

async function mongoDeleteDoc(t           , doc                         )                {
  const original = await mongoFetchDoc(t, doc);
  if (!original) return;
  if (!confirm(tr("dataMongo.deleteDocAsk", { id: cellText(original._id, 80) }))) return;
  const r = await mongoPostRaw                     ("edits", { ...mongoNs(t), edits: [{ op: "delete", original }] });
  if (!r.ok) { toast(r.status === 409 ? tr("dataMongo.conflict", { e: r.error }) : r.error, true); return; }
  toast(tr("dataMongo.deleted"));
  void mongoLoadDocs(t, true);
  void mongoLoadCollections(true);
}

/** How many documents the current filter matches, exactly - what a bulk act states first. */
async function mongoMatchCount(t           , filter                       )                         {
  const j = await mongoPost                    ("count", { ...mongoNs(t), filter, exact: true, maxTimeMS: 30000 });
  return j ? j.total : null;
}

/** Update every document the filter matches: an update document ($set, $unset, $inc, …) or a
 *  pipeline, applied with updateMany. The sheet states the filter it acts on and its count. */
function openBulkUpdate(t           )       {
  const q = mongoQueryOf(t);
  if (q.error) { toast(q.error, true); return; }
  const where = shellText(q.filter, { oneLine: true });
  textSheet({
    title: tr("dataMongo.updateMatching"),
    sub: (t.coll || "") + " · " + where,
    text: "{\n  $set: {\n    \n  }\n}",
    note: tr("dataMongo.updateNote", { f: where }),
    primary: tr("dataMongo.applyUpdate"),
    rows: 12,
    save: async (text        )                         => {
      let u       ;
      try { u = mongoParse(text); } catch (e) { return parseFailure(e); }
      if (u === null || typeof u !== "object") return tr("dataMongo.updateShape");
      const n = await mongoMatchCount(t, q.filter);
      if (n === null) return tr("dataMongo.countFailed");
      if (!confirm(tr("dataMongo.updateAsk", { n: n.toLocaleString(locale()), f: where }))) return tr("dataMongo.cancelled");
      const r = await mongoPostRaw                                                        ("edits", {
        ...mongoNs(t), edits: [{ op: "updateMany", filter: q.filter, update: u }],
      });
      if (!r.ok) return r.error;
      const res = r.body.results[0] || {};
      toast(tr("dataMongo.updatedN", { m: String(res.matched ?? 0), n: String(res.modified ?? 0) }));
      void mongoLoadDocs(t, true);
      return null;
    },
  });
}

async function mongoBulkDelete(t           )                {
  const q = mongoQueryOf(t);
  if (q.error) { toast(q.error, true); return; }
  const where = shellText(q.filter, { oneLine: true });
  const n = await mongoMatchCount(t, q.filter);
  if (n === null) return;
  if (!n) { toast(tr("dataMongo.nothingMatches")); return; }
  const all = !Object.keys(q.filter).length;
  if (!confirm(tr(all ? "dataMongo.deleteAllAsk" : "dataMongo.deleteMatchingAsk", { n: n.toLocaleString(locale()), f: where }))) return;
  const r = await mongoPostRaw                                     ("edits", { ...mongoNs(t), edits: [{ op: "deleteMany", filter: q.filter }] });
  if (!r.ok) { toast(r.error, true); return; }
  const res = r.body.results[0] || {};
  toast(tr("dataMongo.deletedN", { n: String(res.deleted ?? 0) }));
  void mongoLoadDocs(t);
  void mongoLoadCollections(true);
}

/* --- collections ---------------------------------------------------------------------------------- */

/** New collection or view (SPEC §data.mongo): the name, and for a view its source and pipeline;
 *  a collection's options (validator, capped, timeseries…) as a document. */
export function openCreateCollection(op                         )       {
  const db = mongoCurrentDb();
  if (!db) return;
  const view = op === "createView";
  textSheet({
    title: view ? tr("dataMongo.newView") : tr("dataMongo.newCollection"),
    sub: db,
    text: view
      ? "{\n  name: \"\",\n  viewOn: \"\",\n  pipeline: [\n    { $match: {} }\n  ]\n}"
      : "{\n  name: \"\",\n  // options, all optional:\n  // capped: true, size: 1048576,\n  // validator: { $jsonSchema: { required: [\"name\"] } },\n  // timeseries: { timeField: \"ts\" }\n}",
    note: view ? tr("dataMongo.newViewNote") : tr("dataMongo.newCollectionNote"),
    primary: tr("dataMongo.create"),
    rows: 12,
    save: async (text        )                         => {
      let v       ;
      try { v = mongoParse(text); } catch (e) { return parseFailure(e); }
      if (v === null || typeof v !== "object" || Array.isArray(v)) return tr("dataMongo.notADocument");
      const spec = v;
      const name = typeof spec.name === "string" ? spec.name.trim() : "";
      if (!name) return tr("dataMongo.nameRequired");
      const body                          = { db, op, name };
      if (view) { body.viewOn = spec.viewOn; body.pipeline = spec.pipeline; }
      else {
        const options                        = {};
        Object.keys(spec).forEach((k        )       => { if (k !== "name") options[k] = spec[k]; });
        body.options = options;
      }
      const r = await mongoPostRaw                 ("collection", body);
      if (!r.ok) return r.error;
      toast(tr("dataMongo.created", { name }));
      await mongoLoadCollections(true);
      dbOpenTab({ kind: "coll", db, coll: name });
      return null;
    },
  });
}

function openRenameCollection(t           )       {
  const db = t.db || "";
  const from = t.coll || "";
  textSheet({
    title: tr("dataMongo.renameCollection"),
    sub: db + "." + from,
    text: JSON.stringify(from),
    note: tr("dataMongo.renameNote"),
    primary: tr("dataMongo.rename"),
    rows: 3,
    save: async (text        )                         => {
      let v       ;
      try { v = mongoParse(text); } catch (e) { return parseFailure(e); }
      const to = typeof v === "string" ? v.trim() : "";
      if (!to) return tr("dataMongo.nameRequired");
      if (to === from) return null;
      const r = await mongoPostRaw                 ("collection", { db, op: "rename", name: from, to });
      if (!r.ok) return r.error;
      dbTabs().forEach((x) => { if (x.kind === "coll" && x.db === db && x.coll === from) x.coll = to; });
      toast(tr("dataMongo.renamed", { name: to }));
      renderDbTabs();
      renderDbToolbar();
      void mongoLoadCollections(true);
      return null;
    },
  });
}

async function mongoDropCollection(t           )                {
  const db = t.db || "";
  const name = t.coll || "";
  const info = mongoCollInfo(t);
  const n = info && info.count != null ? info.count.toLocaleString(locale()) : "?";
  if (!confirm(tr("dataMongo.dropAsk", { name: db + "." + name, n }))) return;
  const r = await mongoPostRaw                 ("collection", { db, op: "drop", name });
  if (!r.ok) { toast(r.error, true); return; }
  toast(tr("dataMongo.dropped", { name }));
  // Every tab on the dropped collection goes with it (dbDropTableTabs' rule, SPEC §data).
  const keep = dbTabs().filter((x) => !(x.kind === "coll" && x.db === db && x.coll === name));
  if (keep.length !== dbTabs().length) {
    if (!keep.length) keep.push(freshTab("coll"));
    dbResetTabs(keep, 0);
    dbAfterTabSwitch();
  }
  await mongoLoadCollections(true);
}

/* --- export and import ------------------------------------------------------------------------------ */

/** The file name a download's Content-Disposition gives: the exact `filename*` (RFC 5987) the
 *  route sends beside an ASCII `filename`, else that stand-in, else the fallback. Pure. */
export function mongoDownloadName(disp        , fallback        )         {
  const exact = /filename\*=UTF-8''([^;]+)/i.exec(disp);
  if (exact) {
    try { return decodeURIComponent(exact[1]); } catch { /* malformed: the stand-in below */ }
  }
  const plain = /filename="([^"]+)"/.exec(disp);
  return plain ? plain[1] : fallback;
}

/** Download what the query describes (filter, sort, projection), the whole match up to the
 *  server's cap, as JSON (relaxed or canonical), NDJSON or CSV. */
async function mongoExport(t           , format                           , canonical         )                {
  const d = dbConn();
  if (!d.conn || !t.coll) return;
  const q = mongoQueryOf(t);
  if (q.error) { toast(q.error, true); return; }
  let url = base(d.conn) + "export?db=" + encodeURIComponent(t.db || "") + "&collection=" + encodeURIComponent(t.coll) +
    "&format=" + format + "&ejson=" + (canonical ? "canonical" : "relaxed");
  if (Object.keys(q.filter).length) url += "&filter=" + encodeURIComponent(JSON.stringify(q.filter));
  if (q.projection) url += "&projection=" + encodeURIComponent(JSON.stringify(q.projection));
  if (q.sort) url += "&sort=" + encodeURIComponent(JSON.stringify(q.sort));
  if (format === "csv" && t.schema && t.schema.paths.length) url += "&fields=" + encodeURIComponent(JSON.stringify(t.schema.paths));
  toast(tr("dataGrid.exporting"));
  try {
    const resp = await fetch(url);
    if (!resp.ok) {
      const j          = await resp.json().catch(()          => ({}));
      const msg = typeof j === "object" && j !== null && "error" in j && typeof j.error === "string" ? j.error : tr("dataGrid.exportFailedHttpN", { n: resp.status });
      toast(msg, true);
      return;
    }
    const blob = await resp.blob();
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = mongoDownloadName(resp.headers.get("Content-Disposition") || "", t.coll + "." + format);
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(()       => { URL.revokeObjectURL(a.href); }, 1000);
    const rows = Number(resp.headers.get("X-Export-Rows"));
    const capped = resp.headers.get("X-Export-Capped") === "1";
    toast(rows >= 0
      ? tr(capped ? "dataMongo.exportedNCapped" : "dataGrid.exportedNRows", { n: rows.toLocaleString(locale()) })
      : tr("dataGrid.exported"));
  } catch (e) {
    toast(tr("dataGrid.exportFailed"), true);
  }
}

/** Documents out of an import file's text: a JSON array, one document, or NDJSON (one per
 *  line). Read with mongoParse, so canonical, relaxed and shell syntax all come in exact.
 *  Pure: throws with the line that failed. */
export function mongoImportDocs(text        )          {
  const t = text.trim();
  if (!t) return [];
  if (t.charAt(0) === "[") {
    const v = mongoParse(t);
    if (!Array.isArray(v)) throw new Error(tr("dataMongo.importShape"));
    return v;
  }
  try {
    const one = mongoParse(t);
    return [one];
  } catch (e) {
    // Not one value: NDJSON, a document per line.
  }
  const out          = [];
  t.split(/\r?\n/).forEach((line        , i        )       => {
    if (!line.trim()) return;
    try { out.push(mongoParse(line)); } catch (e) {
      throw new Error(tr("dataMongo.importLine", { n: i + 1, e: e instanceof Error ? e.message : String(e) }), { cause: e });
    }
  });
  return out;
}

const UTF8 = new TextEncoder();

/** A document's weight on the wire: its JSON in UTF-8 bytes, not UTF-16 code units - a CJK
 *  character is one code unit but three bytes. Pure. */
function jsonBytes(d       )         {
  return UTF8.encode(JSON.stringify(d)).length;
}

/** The index of the first document too big for any import request, or -1. Pure. */
export function mongoImportTooBig(docs         )         {
  return docs.findIndex((d       )          => jsonBytes(d) > IMPORT_DOC_BYTES);
}

/** Split documents into requests under the body limit and the per-request cap. Pure. */
export function mongoImportBatches(docs         )            {
  const out            = [];
  let cur          = [];
  let bytes = 0;
  docs.forEach((d       )       => {
    const n = jsonBytes(d) + 1;
    if (cur.length && (bytes + n > IMPORT_REQUEST_BYTES || cur.length >= IMPORT_REQUEST_DOCS)) { out.push(cur); cur = []; bytes = 0; }
    cur.push(d);
    bytes += n;
  });
  if (cur.length) out.push(cur);
  return out;
}

/** The import sheet: a file (or pasted text), read here, sent in batches, reported whole. */
function openImport(t           )       {
  showSheet(sheet({
    title: tr("dataMongo.import"),
    sub: (t.db || "") + "." + (t.coll || ""),
    label: tr("dataMongo.import"),
    body: [
      hint(tr("dataMongo.importNote")),
      h("input", { type: "file", id: "mgFile", accept: ".json,.ndjson,.jsonl,application/json", style: "width:auto" }),
      h("textarea", { id: "mgImpText", spellcheck: false, placeholder: tr("dataMongo.importPaste"), style: "min-height:12em;width:100%" }),
      hint("", { id: "mgImpOut", live: true, hidden: true }),
    ],
    foot: [h("span", { class: "grow" }), btn(tr("dataCell.cancel"), { id: "mgImpCancel" }), btn(tr("dataMongo.importGo"), { kind: "primary", id: "mgImpGo" })],
  }));
  const out = $("mgImpOut");
  const say = (text        , bad         )       => {
    out.textContent = text;
    out.hidden = false;
    out.className = bad ? "hint bad" : "hint";
  };
  $                  ("mgFile").onchange = (e       )       => {
    const f = (e.currentTarget                    ).files;
    const file = f && f[0];
    if (!file) return;
    file.text().then((text        )       => { $                     ("mgImpText").value = text; say(tr("dataMongo.importRead", { name: file.name }), false); },
      ()       => { say(tr("dataMongo.importReadFailed"), true); });
  };
  $("mgImpCancel").onclick = ()       => { closeSheet(); };
  let busy = false;
  $("mgImpGo").onclick = ()       => {
    if (busy) return;
    let docs         ;
    try { docs = mongoImportDocs($                     ("mgImpText").value); } catch (e) { say(e instanceof Error ? e.message : String(e), true); return; }
    if (!docs.length) { say(tr("dataMongo.importEmpty"), true); return; }
    const big = mongoImportTooBig(docs);
    if (big >= 0) { say(tr("dataMongo.importTooBig", { n: big + 1, max: mongoBytes(IMPORT_DOC_BYTES) }), true); return; }
    busy = true;
    void (async ()                => {
      const batches = mongoImportBatches(docs);
      let inserted = 0;
      let failed = 0;
      const errors           = [];
      let offset = 0;
      for (let i = 0; i < batches.length; i++) {
        say(tr("dataMongo.importProgress", { i: i + 1, n: batches.length }), false);
        const r = await mongoPostRaw                     ("import", { ...mongoNs(t), docs: batches[i] });
        if (!r.ok) { errors.push(r.error); failed += batches[i].length; break; }
        inserted += r.body.inserted;
        failed += r.body.errorCount;
        r.body.errors.slice(0, 5).forEach((x)       => { errors.push((x.index != null ? "#" + (offset + x.index + 1) + ": " : "") + x.message); });
        offset += batches[i].length;
      }
      busy = false;
      const summary = tr("dataMongo.importDone", { n: inserted.toLocaleString(locale()), f: failed.toLocaleString(locale()) });
      say(errors.length ? summary + "\n" + errors.slice(0, 10).join("\n") : summary, failed > 0);
      if (!failed) { toast(summary); closeSheet(); }
      void mongoLoadDocs(t, true);
      void mongoLoadCollections(true);
    })();
  };
}

/* --- the console ----------------------------------------------------------------------------------- */

/** Run one command document in the console (SPEC §data.mongo): shell syntax in, the reply as a
 *  shell-syntax block, against the database the sidebar shows. True when the server answered
 *  (the caller records the block in the history). */
export async function mongoRunCommand(d          , block        , after            )                   {
  const db = mongoCurrentDb();
  if (!db) { toast(tr("dataMongo.pickDatabase"), true); return false; }
  const p = parseDocument(block, null);
  if (p.error || !p.value) { toast(p.error ? tr("dataMongo.commandError", { e: p.error }) : tr("dataSql.typeCommandFirst"), true); return false; }
  const token = mongoRunReq.issue();
  d.sqlBusy = true;
  after();
  const j = await mongoPost                                        ("command", { db, command: p.value });
  if (!mongoRunReq.accepts(token)) return false; // superseded: a newer run owns the console
  d.sqlBusy = false;
  if (!j) { d.sqlResult = null; d.sqlResults = null; d.resultTab = 0; after(); return false; }
  d.sqlResult = { columns: ["reply"], rows: [{ reply: j.reply }], rowCount: 1, explained: false, elapsedMs: j.elapsedMs };
  d.sqlResults = [d.sqlResult];
  d.resultTab = 0;
  after();
  void mongoLoadCollections(true); // a command may have created or dropped a collection
  return true;
}

// One console run at a time: a slow reply must not land over the newer run's.
const mongoRunReq = dbReqGuard();

/** The reply of a console command as the code block. */
export function mongoReplyNode(reply         )              {
  return h("div", { style: "padding:var(--s2) var(--s3)" }, tokenCodeNode(shellTokens(reply), { all: true }).node);
}

/** The console's templates, grouped by what they act on. The collection name is the one most
 *  recently opened, so a template is one edit from running. */
export function mongoTemplateItems(insert                        )             {
  let coll = "collection";
  let best = -1;
  dbTabs().forEach((x) => { if (x.kind === "coll" && x.coll && x.touched > best) { best = x.touched; coll = x.coll; } });
  const c = JSON.stringify(coll);
  const groups                       = [
    [tr("dataMongo.tplServer"), ["{ serverStatus: 1 }", "{ buildInfo: 1 }", "{ hostInfo: 1 }", "{ replSetGetStatus: 1 }", "{ top: 1 }"]],
    [tr("dataMongo.tplDatabase"), ["{ dbStats: 1 }", "{ listCollections: 1, nameOnly: true }", "{ profile: -1 }", "{ profile: 1, slowms: 100 }"]],
    [tr("dataMongo.tplCollection"), [
      "{ count: " + c + ", query: {} }",
      "{ distinct: " + c + ", key: \"field\" }",
      "{ aggregate: " + c + ", pipeline: [{ $collStats: { storageStats: {} } }], cursor: {} }",
      "{ createIndexes: " + c + ", indexes: [{ key: { field: 1 }, name: \"field_1\" }] }",
      "{ validate: " + c + " }",
    ]],
    [tr("dataMongo.tplWrite"), [
      "{ insert: " + c + ", documents: [{ }] }",
      "{ update: " + c + ", updates: [{ q: { }, u: { $set: { } }, multi: false }] }",
      "{ delete: " + c + ", deletes: [{ q: { }, limit: 1 }] }",
    ]],
  ];
  const items             = [];
  groups.forEach(([label, lines]                    )       => {
    if (items.length) items.push({ sep: true });
    items.push({ heading: true, label, fn: ()       => {} });
    lines.forEach((line        )       => { items.push({ label: line, title: line, fn: ()       => { insert(line); } }); });
  });
  return items;
}

/** Re-print the console's command in shell syntax (the Format item). Null when it does not
 *  parse - the caller says so. */
export function mongoFormatCommand(text        )                {
  try { return shellText(mongoParse(text)); } catch { return null; }
}

/** Show the collection's facts as a tag line - used by the panes module. */
export function mongoTypeTag(info                     )         {
  if (!info) return null;
  if (info.type === "view") return tag(tr("dataMongo.view"));
  if (info.type === "timeseries") return tag(tr("dataMongo.timeseries"));
  if (info.capped) return tag(tr("dataMongo.capped"));
  return null;
}

/** The aggregation reply shape is shared with the panes module. */
;                                

/** Open a popup menu at an element (the panes module's stage menus). */
export function mongoMenuAt(anchor         , items            )       {
  popupMenu(anchor.getBoundingClientRect(), items);
}
