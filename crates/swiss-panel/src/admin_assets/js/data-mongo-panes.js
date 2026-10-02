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

             
                                                                                                                  
                                          
                        
                                                                
                                               
                                     
                                              
import { $, apiJson, el, emptyNode, toast } from "./util.js";
import { h } from "./h.js";
import { cellText, mongoParse, MongoSyntaxError, shellText, shellTokens } from "./mongo-ejson.js";
import { dbConn } from "./db-state.js";
import { renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { renderDbFilters } from "./data-filters.js";
import { dbCopyText } from "./data-csv.js";
import {
  mongoAllowsDestroy, mongoBytes, mongoCollInfo, mongoLoadCollections, mongoLoadDocs, mongoNs, mongoQueryOf, mongoTab, mongoTeachPaths,
} from "./data-mongo.js";
import { locale, tr, trn } from "./i18n.js";
import { btn, iconBtn } from "./ui/button.js";
import { hint } from "./ui/form.js";
import { jsonCodeNode, tokenCodeNode, valueBlock } from "./ui/json-view.js";
import { closeSheet, sheet, showSheet } from "./ui/sheet.js";
import { tag } from "./ui/status.js";

/* ================================================================================================
   The collection workspace's analysis panes (SPEC §data.mongo-panel, §data.mongo-pipeline,
   §data.mongo-schema): Aggregations, Schema, Indexes, Explain and Validation. data-mongo.ts owns
   the tab, the head and the Documents pane and hands each of these its body, its primary action
   and its overflow rows; the delegated events come back here through the four dispatchers at
   the end. Every value is shell syntax on screen (mongo-ejson.ts).
   ================================================================================================ */

/** The stage operators the builder offers, with the body a new stage of each starts from. */
export const MONGO_STAGES                     = [
  ["$match", "{ }"],
  ["$project", "{ field: 1 }"],
  ["$addFields", "{ newField: \"$field\" }"],
  ["$set", "{ newField: \"$field\" }"],
  ["$unset", "\"field\""],
  ["$group", "{ _id: \"$field\", count: { $sum: 1 } }"],
  ["$sort", "{ field: -1 }"],
  ["$limit", "10"],
  ["$skip", "0"],
  ["$unwind", "\"$field\""],
  ["$lookup", "{ from: \"other\", localField: \"field\", foreignField: \"_id\", as: \"joined\" }"],
  ["$graphLookup", "{ from: \"other\", startWith: \"$field\", connectFromField: \"field\", connectToField: \"_id\", as: \"path\" }"],
  ["$facet", "{ total: [{ $count: \"n\" }] }"],
  ["$bucket", "{ groupBy: \"$field\", boundaries: [0, 10, 100], default: \"other\" }"],
  ["$bucketAuto", "{ groupBy: \"$field\", buckets: 5 }"],
  ["$sortByCount", "\"$field\""],
  ["$count", "\"count\""],
  ["$replaceRoot", "{ newRoot: \"$field\" }"],
  ["$replaceWith", "\"$field\""],
  ["$sample", "{ size: 10 }"],
  ["$unionWith", "{ coll: \"other\" }"],
  ["$setWindowFields", "{ sortBy: { field: 1 }, output: { running: { $sum: \"$value\", window: { documents: [\"unbounded\", \"current\"] } } } }"],
  ["$densify", "{ field: \"ts\", range: { step: 1, unit: \"hour\", bounds: \"full\" } }"],
  ["$fill", "{ output: { value: { method: \"linear\" } }, sortBy: { ts: 1 } }"],
  ["$redact", "\"$$KEEP\""],
  ["$geoNear", "{ near: { type: \"Point\", coordinates: [0, 0] }, distanceField: \"distance\" }"],
  ["$indexStats", "{ }"],
  ["$merge", "{ into: \"target\" }"],
  ["$out", "\"target\""],
];

/** How many documents a stage preview shows. */
const PREVIEW_DOCS = 10;
/** How many output documents a run shows. */
const RUN_DOCS = 20;
const SAMPLE_SIZES = [100, 1000, 5000, 10000];
const PIPELINES_KEY = "swiss.mongoPipelines";

function opBody(op        )         {
  const hit = MONGO_STAGES.find((s                  )          => s[0] === op);
  return hit ? hit[1] : "{ }";
}

function codeNode(v         , oneLine          )              {
  return tokenCodeNode(shellTokens(v, { oneLine }), { all: true, oneLine }).node;
}

function syntaxText(e         )         {
  if (e instanceof MongoSyntaxError) return tr("dataMongo.syntaxAt", { e: e.message, l: e.line, c: e.col });
  return e instanceof Error ? e.message : String(e);
}

/* --- the pipeline ----------------------------------------------------------------------------------- */

/** The stages that run, as pipeline documents, up to (and including) stage `upTo` when given.
 *  The first stage that does not parse marks itself and stops the build. Pure over the tab. */
export function mongoPipelineOf(t           , upTo         )                                                              {
  if (t.aggText !== null) {
    try {
      const v = mongoParse(t.aggText || "[]");
      if (!Array.isArray(v)) return { pipeline: [], error: tr("dataMongo.pipelineShape") };
      const stages = v.filter((s       )                             => s !== null && typeof s === "object" && !Array.isArray(s));
      if (stages.length !== v.length) return { pipeline: [], error: tr("dataMongo.pipelineShape") };
      return { pipeline: stages, error: null };
    } catch (e) {
      return { pipeline: [], error: syntaxText(e) };
    }
  }
  const out                          = [];
  for (let i = 0; i < t.stages.length; i++) {
    if (upTo !== undefined && i > upTo) break;
    const s = t.stages[i];
    s.error = null;
    if (!s.on) continue;
    try {
      out.push({ [s.op]: mongoParse(s.body) });
    } catch (e) {
      s.error = syntaxText(e);
      return { pipeline: [], error: tr("dataMongo.stageError", { n: i + 1, e: s.error }) };
    }
  }
  return { pipeline: out, error: null };
}

/** The pipeline as one shell-syntax array - what text mode edits and Copy hands out. */
function pipelineText(t           )         {
  const p = mongoPipelineOf(t);
  return shellText(p.error ? [] : p.pipeline);
}

async function mongoAggRun(t           )                {
  const p = mongoPipelineOf(t);
  if (p.error) { toast(p.error, true); renderDbGrid(); return; }
  if (!p.pipeline.length) { toast(tr("dataMongo.pipelineEmpty"), true); return; }
  const last = Object.keys(p.pipeline[p.pipeline.length - 1])[0] || "";
  if ((last === "$out" || last === "$merge") && !confirm(tr("dataMongo.writeAsk", { op: last }))) return;
  t.aggBusy = true;
  renderDbGrid();
  const j = await apiJson                  (endpoint("aggregate"), {
    method: "POST", body: JSON.stringify({ ...mongoNs(t), pipeline: p.pipeline, limit: RUN_DOCS, allowDiskUse: true }),
  });
  t.aggBusy = false;
  t.aggResult = j;
  if (j && j.wrote) void mongoLoadCollections(true);
  if (mongoTab() === t) { renderDbGrid(); renderDbToolbar(); }
}

async function mongoStagePreview(t           , i        )                {
  const s = t.stages[i];
  if (s.op === "$out" || s.op === "$merge") { s.error = tr("dataMongo.previewWrites"); renderDbGrid(); return; }
  const p = mongoPipelineOf(t, i);
  if (p.error) { renderDbGrid(); return; }
  const j = await apiJson                  (endpoint("aggregate"), {
    method: "POST", body: JSON.stringify({ ...mongoNs(t), pipeline: p.pipeline, limit: PREVIEW_DOCS }),
  });
  s.preview = j ? { docs: j.docs, more: j.more } : null;
  if (mongoTab() === t) renderDbGrid();
}

function endpoint(path        )         {
  return "/api/db/" + encodeURIComponent(dbConn().conn || "") + "/mongo/" + path;
}

function renderAgg(wrap             , t           )       {
  const box = h("div", { style: "display:flex;flex-direction:column;gap:var(--s2);padding:var(--s2) var(--s3)" });
  if (t.aggText !== null) {
    box.appendChild(hint(tr("dataMongo.textModeNote")));
    box.appendChild(h("textarea", { spellcheck: false, value: t.aggText, style: "min-height:16em;width:100%", data: { maggtext: "" } }));
  } else {
    if (!t.stages.length) box.appendChild(hint(tr("dataMongo.pipelineStart")));
    t.stages.forEach((s              , i        )       => { box.appendChild(stageCard(t, s, i)); });
    box.appendChild(h("div", { class: "db-console-row" },
      btn(tr("dataMongo.addStage"), { icon: "plus", data: { maggadd: "" } }),
      hint(tr("dataMongo.stageHint"))));
  }
  if (t.aggBusy) box.appendChild(el("div", "db-hint", tr("dataGrid.running")));
  else if (t.aggResult) box.appendChild(aggResultNode(t.aggResult));
  wrap.appendChild(box);
}

function stageCard(t           , s              , i        )              {
  const sel = h("select", { class: "db-fsel", title: tr("dataMongo.stageOp"), data: { mstageop: String(i) } },
    MONGO_STAGES.map((op                  )         => h("option", { value: op[0], selected: op[0] === s.op }, op[0])));
  const on = h("input", { type: "checkbox", checked: s.on, title: tr("dataMongo.stageOn"), data: { mstageon: String(i) } });
  const tools           = [
    iconBtn("play", tr("dataMongo.previewStage"), { ghost: true, data: { mstagepreview: String(i) } }),
    iconBtn("arrow-up", tr("dataMongo.moveUp"), { ghost: true, disabled: i === 0, data: { mstageup: String(i) } }),
    iconBtn("chevron-down", tr("dataMongo.moveDown"), { ghost: true, disabled: i === t.stages.length - 1, data: { mstagedown: String(i) } }),
    iconBtn("trash", tr("dataMongo.removeStage"), { ghost: true, data: { mstagerm: String(i) } }),
  ];
  const notes           = [];
  if (!s.on) notes.push(tr("dataMongo.stageOff"));
  if (s.preview) notes.push(trn(s.preview.docs.length, "dataMongo.nOut.one", "dataMongo.nOut.other") + (s.preview.more ? "+" : ""));
  const preview           = [];
  if (s.error) preview.push(hint(s.error, { bad: true }));
  else if (s.preview) {
    if (!s.preview.docs.length) preview.push(hint(tr("dataMongo.previewEmpty")));
    s.preview.docs.forEach((d                         )       => { preview.push(codeNode(d, true)); });
  }
  return valueBlock({ label: String(i + 1) + " · " + s.op, notes, tools, data: { mstage: String(i) } },
    h("div", { style: "display:flex;align-items:center;gap:var(--s3)" }, sel,
      h("label", { style: "display:inline-flex;align-items:center;gap:var(--s1)" }, on, tr("dataMongo.stageInclude"))),
    h("textarea", { spellcheck: false, value: s.body, style: "min-height:5em;width:100%", data: { mstagebody: String(i) } }),
    preview);
}

function aggResultNode(r                  )              {
  if (r.wrote) {
    return valueBlock({ label: tr("dataMongo.output"), notes: [tr("dataMongo.wrote", { ns: r.wrote })] });
  }
  const notes = [trn(r.docs.length, "dataMongo.nOut.one", "dataMongo.nOut.other") + (r.more ? "+" : "")];
  if (r.elapsedMs != null) notes.push(tr("dataGrid.msMs", { ms: r.elapsedMs }).trim());
  return valueBlock({
    label: tr("dataMongo.output"), notes,
    tools: [iconBtn("copy", tr("dataMongo.copyOutput"), { ghost: true, data: { maggcopy: "" } })],
  }, r.docs.length ? r.docs.map((d                         )              => codeNode(d)) : hint(tr("dataMongo.previewEmpty")));
}

/* Saved pipelines, per collection, in this browser (the console's favorites idiom). */
function savedPipelines()                                         {
  try {
    const raw = localStorage.getItem(PIPELINES_KEY);
    const v          = raw ? JSON.parse(raw) : {};
    return typeof v === "object" && v !== null ? v                                           : {};
  } catch (e) {
    return {};
  }
}

function pipelineKey(t           )         {
  return (dbConn().conn || "") + "/" + (t.db || "") + "." + (t.coll || "");
}

function savePipeline(t           )       {
  const p = mongoPipelineOf(t);
  if (p.error || !p.pipeline.length) { toast(p.error || tr("dataMongo.pipelineEmpty"), true); return; }
  const all = savedPipelines();
  const mine = all[pipelineKey(t)] || {};
  const name = new Date().toLocaleString(locale()) + " · " + p.pipeline.map((s) => Object.keys(s)[0]).join(" → ");
  mine[name] = shellText(p.pipeline);
  all[pipelineKey(t)] = mine;
  try { localStorage.setItem(PIPELINES_KEY, JSON.stringify(all)); toast(tr("dataMongo.pipelineSaved")); } catch (e) { toast(tr("dataMongo.pipelineSaveFailed"), true); }
}

/** Load a pipeline's text into the builder - stages when every stage is one operator, text
 *  mode otherwise. */
function loadPipeline(t           , text        )       {
  try {
    const v = mongoParse(text);
    if (Array.isArray(v) && v.every((s       )          => s !== null && typeof s === "object" && !Array.isArray(s) && Object.keys(s).length === 1)) {
      t.stages = v.map((s       )               => {
        const o = s                         ;
        const op = Object.keys(o)[0];
        return { op, body: shellText(o[op]), on: true, preview: null, error: null };
      });
      t.aggText = null;
    } else {
      t.aggText = text;
    }
  } catch (e) {
    t.aggText = text;
  }
  t.aggResult = null;
  renderDbGrid();
}

/* --- schema ----------------------------------------------------------------------------------------- */

async function mongoSchemaRun(t           )                {
  const q = mongoQueryOf(t);
  t.schemaBusy = true;
  renderDbGrid();
  const j = await apiJson                (endpoint("schema"), {
    method: "POST", body: JSON.stringify({ ...mongoNs(t), sample: t.schemaSample, filter: q.error ? {} : q.filter }),
  });
  t.schemaBusy = false;
  if (j) { t.schema = j; mongoTeachPaths(t, j.paths); }
  if (mongoTab() === t) { renderDbGrid(); renderDbToolbar(); }
}

/** One type's details: its top values (a click filters on one), its range or its lengths. */
function typeDetail(path        , ty                    )           {
  const out           = [];
  if (ty.top && ty.top.length && ty.type !== "Document" && ty.type !== "Array") {
    ty.top.slice(0, 6).forEach((tv                                   )       => {
      out.push(btn(cellText(tv.value, 40) + " ×" + tv.count.toLocaleString(locale()), {
        kind: "ghost", title: tr("dataMongo.filterOnValue"),
        data: { mfpath: path, mfval: JSON.stringify(tv.value) },
      }));
    });
    if (ty.distinct != null) out.push(el("span", "db-filter-hint", tr(ty.distinctCapped ? "dataMongo.distinctAtLeast" : "dataMongo.distinctN", { n: ty.distinct })));
  }
  if (ty.min !== undefined && ty.max !== undefined) out.push(el("span", "db-filter-hint", cellText(ty.min, 40) + " … " + cellText(ty.max, 40)));
  if (ty.lengths) out.push(el("span", "db-filter-hint", tr("dataMongo.lengths", { a: ty.lengths.min, b: ty.lengths.max })));
  return out;
}

/** The fields as rows: a nested path indented under its parent, array element documents
 *  under the array. */
function schemaRows(fields                       , depth        , out               )       {
  fields.forEach((f                     )       => {
    const tri = el("tr");
    const name = el("td", "db-cell");
    name.style.paddingLeft = "calc(var(--s3) * " + (depth + 1) + ")";
    name.appendChild(h("span", { title: f.path }, f.name));
    tri.appendChild(name);
    tri.appendChild(el("td", "db-cell tnum", Math.round(f.probability * 1000) / 10 + "%"));
    const types = el("td", "db-cell");
    f.types.forEach((ty                    )       => {
      types.appendChild(tag(ty.type + " " + Math.round(ty.probability * 1000) / 10 + "%", { mono: true }));
      types.appendChild(document.createTextNode(" "));
    });
    tri.appendChild(types);
    const detail = el("td", "db-cell");
    f.types.forEach((ty                    )       => { typeDetail(f.path, ty).forEach((n        )       => { if (n instanceof Node) detail.appendChild(n); }); });
    tri.appendChild(detail);
    out.push(tri);
    f.types.forEach((ty                    )       => {
      if (ty.fields) schemaRows(ty.fields, depth + 1, out);
      if (ty.elements) ty.elements.types.forEach((et                    )       => { if (et.fields) schemaRows(et.fields, depth + 1, out); });
    });
  });
}

function renderSchema(wrap             , t           )       {
  const q = mongoQueryOf(t);
  const top = h("div", { class: "db-console-row", style: "padding:var(--s2) var(--s3)" },
    h("span", { class: "db-filter-hint" }, tr("dataMongo.sampleSize")),
    h("select", { class: "db-fsel", data: { msample: "" }, title: tr("dataMongo.sampleSize") },
      SAMPLE_SIZES.map((n        )         => h("option", { value: String(n), selected: n === t.schemaSample }, n.toLocaleString(locale())))),
    hint(!q.error && Object.keys(q.filter).length ? tr("dataMongo.schemaFiltered", { f: shellText(q.filter, { oneLine: true }) }) : tr("dataMongo.schemaWhole")));
  wrap.appendChild(top);
  if (t.schemaBusy) { wrap.appendChild(el("div", "db-hint", tr("dataMongo.analyzing"))); return; }
  const s = t.schema;
  if (!s) {
    wrap.appendChild(emptyNode({ icon: "list", title: tr("dataMongo.schemaTitle"), hint: tr("dataMongo.schemaHint") }));
    return;
  }
  wrap.appendChild(h("div", { class: "db-console-row", style: "padding:0 var(--s3)" },
    hint(tr("dataMongo.sampledN", { n: s.sampled.toLocaleString(locale()), ms: s.elapsedMs ?? 0 }) + (s.fieldsCapped ? " · " + tr("dataMongo.fieldsCapped") : ""))));
  // Nothing sampled (the filter matched nothing): a header over no rows would read as a failure.
  if (!s.fields.length) { wrap.appendChild(el("div", "db-hint", tr("dataMongo.nothingMatches"))); return; }
  const tbl = el("table", "db-grid");
  const hr = el("tr");
  [tr("dataMongo.field"), tr("dataMongo.presence"), tr("dataMongo.types"), tr("dataMongo.values")].forEach((x        )       => { hr.appendChild(el("th", "db-col", x)); });
  const thead = el("thead");
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const rows                = [];
  schemaRows(s.fields, 0, rows);
  const tbody = el("tbody");
  rows.forEach((r             )       => { tbody.appendChild(r); });
  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
}

/* --- indexes ---------------------------------------------------------------------------------------- */

async function mongoLoadIndexes(t           )                {
  const j = await apiJson                         (endpoint("indexes?db=" + encodeURIComponent(t.db || "") + "&collection=" + encodeURIComponent(t.coll || "")));
  t.indexes = j || { indexes: [] };
  if (mongoTab() === t) renderDbGrid();
}

function indexProps(ix               )           {
  const out           = [];
  if (ix.unique) out.push(tag(tr("dataMongo.ixUnique")));
  if (ix.sparse) out.push(tag(tr("dataMongo.ixSparse")));
  if (ix.hidden) out.push(tag(tr("dataMongo.ixHidden")));
  if (ix.expireAfterSeconds != null) out.push(tag(tr("dataMongo.ixTtl", { n: ix.expireAfterSeconds })));
  if (ix.partialFilterExpression) out.push(tag(tr("dataMongo.ixPartial"), { mono: true }));
  if (ix.collation) out.push(tag(tr("dataMongo.ixCollation")));
  const key = Object.values(ix.key).map((v         )         => cellText(v));
  if (key.some((k        )          => k === "\"text\"")) out.push(tag(tr("dataMongo.ixText")));
  if (key.some((k        )          => k === "\"2dsphere\"" || k === "\"2d\"")) out.push(tag(tr("dataMongo.ixGeo")));
  if (key.some((k        )          => k === "\"hashed\"")) out.push(tag(tr("dataMongo.ixHashed")));
  return out.flatMap((n        )           => [n, " "]);
}

function renderIndexes(wrap             , t           )       {
  const r = t.indexes;
  if (!r) { wrap.appendChild(el("div", "db-hint", tr("dataGrid.loading"))); void mongoLoadIndexes(t); return; }
  if (r.statsError) wrap.appendChild(h("div", { style: "padding:var(--s2) var(--s3)" }, hint(tr("dataMongo.indexStatsError", { e: r.statsError }))));
  if (!r.indexes.length) { wrap.appendChild(el("div", "db-hint", tr("dataMongo.noIndexes"))); return; }
  const tbl = el("table", "db-grid");
  const hr = el("tr");
  [tr("dataMongo.ixName"), tr("dataMongo.ixKeys"), tr("dataMongo.ixProps"), tr("dataMongo.ixSize"), tr("dataMongo.ixUsage")].forEach((x        )       => { hr.appendChild(el("th", "db-col", x)); });
  hr.appendChild(el("th", "db-rowctl", ""));
  const thead = el("thead");
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  r.indexes.forEach((ix               , i        )       => {
    const tri = el("tr");
    tri.appendChild(el("td", "db-cell", ix.name));
    const keys = el("td", "db-cell");
    keys.appendChild(codeNode(ix.key, true));
    tri.appendChild(keys);
    const props = el("td", "db-cell");
    indexProps(ix).forEach((n        )       => { if (n instanceof Node) props.appendChild(n); else if (typeof n === "string") props.appendChild(document.createTextNode(n)); });
    if (ix.partialFilterExpression) props.title = shellText(ix.partialFilterExpression, { oneLine: true });
    tri.appendChild(props);
    tri.appendChild(el("td", "db-cell tnum", mongoBytes(ix.size)));
    const usage = ix.ops == null ? "" : tr("dataMongo.ixOps", { n: ix.ops.toLocaleString(locale()) }) + (ix.since ? " · " + tr("dataMongo.ixSince", { d: new Date(ix.since).toLocaleDateString(locale()) }) : "");
    const u = el("td", "db-cell tnum", usage);
    if (ix.ops === 0 && ix.name !== "_id_") u.title = tr("dataMongo.ixUnused");
    tri.appendChild(u);
    const ctl = el("td", "db-rowctl");
    if (ix.name !== "_id_") {
      ctl.appendChild(iconBtn(ix.hidden ? "eye" : "eye-off", ix.hidden ? tr("dataMongo.ixUnhide") : tr("dataMongo.ixHide"), { ghost: true, data: { mixhide: String(i) } }));
      ctl.appendChild(iconBtn("trash", tr("dataMongo.ixDrop"), { ghost: true, data: { mixdrop: String(i) } }));
    }
    tri.appendChild(ctl);
    tbody.appendChild(tri);
  });
  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
}

async function mongoIndexOp(t           , body                         )                   {
  const j = await apiJson                                (endpoint("index"), { method: "POST", body: JSON.stringify({ ...mongoNs(t), ...body }) });
  if (!j) return false;
  await mongoLoadIndexes(t);
  void mongoLoadCollections(true);
  return true;
}

function openCreateIndex(t           )       {
  showSheet(sheet({
    title: tr("dataMongo.newIndex"),
    sub: (t.db || "") + "." + (t.coll || ""),
    label: tr("dataMongo.newIndex"),
    body: [
      hint(tr("dataMongo.newIndexNote")),
      h("textarea", { id: "mgIx", spellcheck: false, style: "min-height:12em;width:100%" }),
      hint("", { id: "mgIxErr", bad: true, live: true, hidden: true }),
    ],
    foot: [h("span", { class: "grow" }), btn(tr("dataCell.cancel"), { id: "mgIxCancel" }), btn(tr("dataMongo.createIndex"), { kind: "primary", id: "mgIxGo" })],
  }));
  const ta = $                     ("mgIx");
  ta.value = "{\n  keys: { field: 1 },\n  // options: name, unique, sparse, hidden, expireAfterSeconds,\n  //          partialFilterExpression, collation\n  options: { }\n}";
  const err = $("mgIxErr");
  $("mgIxCancel").onclick = ()       => { closeSheet(); };
  $("mgIxGo").onclick = ()       => {
    let v       ;
    try { v = mongoParse(ta.value); } catch (e) { err.textContent = syntaxText(e); err.hidden = false; return; }
    if (v === null || typeof v !== "object" || Array.isArray(v) || v.keys === null || typeof v.keys !== "object" || Array.isArray(v.keys)) {
      err.textContent = tr("dataMongo.indexShape"); err.hidden = false; return;
    }
    const spec = v;
    void mongoIndexOp(t, { op: "create", keys: spec.keys, options: spec.options && typeof spec.options === "object" ? spec.options : {} })
      .then((ok         )       => { if (ok) { closeSheet(); toast(tr("dataMongo.indexCreated")); } });
  };
  ta.focus();
}

/* --- explain ---------------------------------------------------------------------------------------- */

async function mongoExplainRun(t           , of                , verbosity         )                {
  const body                          = { ...mongoNs(t), verbosity: verbosity || "executionStats" };
  if (of === "agg") {
    const p = mongoPipelineOf(t);
    if (p.error) { toast(p.error, true); return; }
    body.pipeline = p.pipeline;
  } else {
    const q = mongoQueryOf(t);
    if (q.error) { t.qError = q.error; renderDbFilters(); return; }
    body.filter = q.filter;
    if (q.sort) body.sort = q.sort;
    if (q.projection) body.projection = q.projection;
    body.skip = t.qSkip;
    body.limit = t.qLimit;
  }
  t.explainOf = of;
  t.pane = "explain";
  t.explain = null;
  t.loading = true;
  renderDbToolbar(); renderDbFilters(); renderDbGrid();
  const j = await apiJson                      (endpoint("explain"), { method: "POST", body: JSON.stringify(body) });
  t.loading = false;
  t.explain = j;
  if (mongoTab() === t) renderDbGrid();
}

function stat(label        , v                    )         {
  return v == null ? null : h("span", { class: "db-filter-hint" }, label + " " + v.toLocaleString(locale()));
}

function renderExplain(wrap             , t           )       {
  if (t.loading) { wrap.appendChild(el("div", "db-hint", tr("dataMongo.explaining"))); return; }
  const r = t.explain;
  if (!r) {
    wrap.appendChild(emptyNode({ icon: "activity", title: tr("dataMongo.explainTitle"), hint: tr("dataMongo.explainHint") }));
    return;
  }
  const s = r.summary;
  const box = h("div", { style: "display:flex;flex-direction:column;gap:var(--s2);padding:var(--s2) var(--s3)" });
  const verdict           = [];
  verdict.push(s.collscan ? tag(tr("dataMongo.collscan"), { tone: "bad" }) : tag(s.indexes.length ? tr("dataMongo.usesIndex", { ix: s.indexes.join(", ") }) : tr("dataMongo.noScan")));
  if (s.inMemorySort) verdict.push(tag(tr("dataMongo.inMemorySort"), { tone: "warn" }));
  if (s.rejectedPlans) verdict.push(tag(trn(s.rejectedPlans, "dataMongo.rejectedN.one", "dataMongo.rejectedN.other")));
  if (s.sharded) verdict.push(tag(tr("dataMongo.sharded")));
  box.appendChild(h("div", { class: "db-console-row" }, verdict,
    h("span", { class: "db-filter-hint" }, tr(t.explainOf === "agg" ? "dataMongo.explainOfAgg" : "dataMongo.explainOfFind")),
    s.engine ? h("span", { class: "db-filter-hint" }, tr("dataMongo.engine", { e: s.engine })) : null));
  box.appendChild(h("div", { class: "db-console-row" },
    stat(tr("dataMongo.returned"), s.nReturned),
    stat(tr("dataMongo.keysExamined"), s.totalKeysExamined),
    stat(tr("dataMongo.docsExamined"), s.totalDocsExamined),
    s.executionTimeMillis != null ? h("span", { class: "db-filter-hint" }, tr("dataGrid.msMs", { ms: s.executionTimeMillis }).trim()) : null));
  if (s.nReturned != null && s.totalDocsExamined != null && s.nReturned > 0 && s.totalDocsExamined / s.nReturned >= 10) {
    box.appendChild(hint(tr("dataMongo.ratioWarn", { n: Math.round(s.totalDocsExamined / s.nReturned) }), { bad: true }));
  }
  const tbl = el("table", "db-grid");
  const hr = el("tr");
  [tr("dataMongo.stage"), tr("dataMongo.ixName"), tr("dataMongo.returned"), tr("dataMongo.keysExamined"), tr("dataMongo.docsExamined"), "ms"].forEach((x        )       => { hr.appendChild(el("th", "db-col", x)); });
  const thead = el("thead");
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  const num = (v                    )         => (v == null ? "" : v.toLocaleString(locale()));
  s.stages.forEach((row                 )       => {
    const tri = el("tr");
    const st = el("td", "db-cell");
    st.style.paddingLeft = "calc(var(--s3) * " + (row.depth + 1) + ")";
    st.appendChild(row.stage === "COLLSCAN" ? tag(row.stage, { tone: "bad", mono: true }) : row.stage === "SORT" ? tag(row.stage, { tone: "warn", mono: true }) : tag(row.stage, { mono: true }));
    if (row.filter !== undefined) st.title = shellText(row.filter, { oneLine: true });
    tri.appendChild(st);
    tri.appendChild(el("td", "db-cell", row.indexName ? row.indexName + (row.keyPattern ? " " + shellText(row.keyPattern, { oneLine: true }) : "") : ""));
    tri.appendChild(el("td", "db-cell tnum", num(row.nReturned)));
    tri.appendChild(el("td", "db-cell tnum", num(row.keysExamined)));
    tri.appendChild(el("td", "db-cell tnum", num(row.docsExamined)));
    tri.appendChild(el("td", "db-cell tnum", num(row.executionTimeMillisEstimate)));
    tbody.appendChild(tri);
  });
  (s.pipeline || []).forEach((p)       => {
    const tri = el("tr");
    const st = el("td", "db-cell");
    st.appendChild(tag(p.stage, { mono: true }));
    tri.appendChild(st);
    tri.appendChild(el("td", "db-cell", ""));
    tri.appendChild(el("td", "db-cell tnum", num(p.nReturned)));
    tri.appendChild(el("td", "db-cell", ""));
    tri.appendChild(el("td", "db-cell", ""));
    tri.appendChild(el("td", "db-cell tnum", num(p.executionTimeMillisEstimate)));
    tbody.appendChild(tri);
  });
  tbl.appendChild(tbody);
  box.appendChild(tbl);
  box.appendChild(h("details", null, h("summary", null, tr("dataMongo.rawExplain")), jsonCodeNode(r.explain, false).node));
  wrap.appendChild(box);
}

/* --- validation ------------------------------------------------------------------------------------- */

function renderValidation(wrap             , t           )       {
  const info = mongoCollInfo(t);
  if (t.validatorText === null) {
    t.validatorText = info && info.validator
      ? shellText(info.validator)
      : "{\n  $jsonSchema: {\n    bsonType: \"object\",\n    required: [],\n    properties: {\n    }\n  }\n}";
  }
  const level = info && info.validationLevel ? info.validationLevel : "strict";
  const action = info && info.validationAction ? info.validationAction : "error";
  wrap.appendChild(h("div", { style: "display:flex;flex-direction:column;gap:var(--s2);padding:var(--s2) var(--s3)" },
    hint(info && info.validator ? tr("dataMongo.validatorNote") : tr("dataMongo.noValidatorNote")),
    h("div", { class: "db-console-row" },
      h("span", { class: "db-filter-hint" }, tr("dataMongo.validationLevel")),
      h("select", { class: "db-fsel", data: { mvlevel: "" } },
        ["off", "strict", "moderate"].map((v        )         => h("option", { value: v, selected: v === level }, v))),
      h("span", { class: "db-filter-hint" }, tr("dataMongo.validationAction")),
      h("select", { class: "db-fsel", data: { mvaction: "" } },
        ["error", "warn", "errorAndLog"].map((v        )         => h("option", { value: v, selected: v === action }, v)))),
    h("textarea", { spellcheck: false, value: t.validatorText, style: "min-height:18em;width:100%", data: { mval: "" } })));
}

async function mongoSaveValidator(t           )                {
  let v       ;
  try { v = mongoParse(t.validatorText || "{}"); } catch (e) { toast(syntaxText(e), true); return; }
  if (v === null || typeof v !== "object" || Array.isArray(v)) { toast(tr("dataMongo.notADocument"), true); return; }
  const level = document.querySelector                   ("[data-mvlevel]")?.value || "strict";
  const action = document.querySelector                   ("[data-mvaction]")?.value || "error";
  const j = await apiJson                    (endpoint("command"), {
    method: "POST",
    body: JSON.stringify({ db: t.db, command: { collMod: t.coll, validator: v, validationLevel: level, validationAction: action } }),
  });
  if (!j) return;
  toast(tr("dataMongo.validatorSaved"));
  await mongoLoadCollections(true);
  t.validatorText = null;
  if (mongoTab() === t) renderDbGrid();
}

/* --- the seams data-mongo.ts calls ---------------------------------------------------------------- */

/** The pane's body. */
export function renderMongoPane(wrap             , t           )       {
  if (t.pane === "agg") renderAgg(wrap, t);
  else if (t.pane === "schema") renderSchema(wrap, t);
  else if (t.pane === "indexes") renderIndexes(wrap, t);
  else if (t.pane === "explain") renderExplain(wrap, t);
  else if (t.pane === "validation") renderValidation(wrap, t);
}

/** The pane's one primary action for the head. */
export function mongoPaneHead(t           )                     {
  if (t.pane === "agg") return btn(tr("dataView.run"), { title: tr("dataMongo.runPipelineTitle"), data: { maggrun: "" } });
  if (t.pane === "schema") return btn(tr("dataMongo.analyze"), { title: tr("dataMongo.analyzeTitle"), data: { mschemarun: "" } });
  if (t.pane === "indexes") return btn(tr("dataMongo.addIndex"), { title: tr("dataMongo.newIndex"), data: { mixadd: "" } });
  if (t.pane === "explain") return btn(tr("dataView.explain"), { title: tr("dataMongo.explainTitle"), data: { mexplainrun: "" } });
  if (t.pane === "validation") return btn(tr("dataMongo.saveValidator"), { title: tr("dataMongo.saveValidatorTitle"), data: { mvalsave: "" } });
  return null;
}

/** The pane's rows in the workspace's overflow. */
export function mongoPaneMore(t           )             {
  if (t.pane === "agg") {
    const items             = [
      { label: t.aggText !== null ? tr("dataMongo.builderMode") : tr("dataMongo.textMode"), fn: ()       => { toggleTextMode(t); } },
      { label: tr("dataMongo.copyPipeline"), fn: ()       => { dbCopyText(t.aggText !== null ? t.aggText : pipelineText(t)); } },
      { label: tr("dataMongo.explainPipeline"), fn: ()       => { void mongoExplainRun(t, "agg"); } },
      { label: tr("dataMongo.savePipeline"), fn: ()       => { savePipeline(t); } },
      { label: tr("dataMongo.clearPipeline"), fn: ()       => { t.stages = []; t.aggText = t.aggText === null ? null : "[]"; t.aggResult = null; renderDbGrid(); } },
    ];
    const mine = savedPipelines()[pipelineKey(t)] || {};
    const names = Object.keys(mine);
    if (names.length) {
      items.push({ sep: true });
      items.push({ heading: true, label: tr("dataMongo.savedPipelines"), fn: ()       => {} });
      names.slice(-10).reverse().forEach((n        )       => { items.push({ label: n, title: mine[n], fn: ()       => { loadPipeline(t, mine[n]); } }); });
    }
    return items;
  }
  if (t.pane === "explain") {
    return [
      { label: tr("dataMongo.explainQueryPlanner"), fn: ()       => { void mongoExplainRun(t, t.explainOf, "queryPlanner"); } },
      { label: tr("dataMongo.explainAllPlans"), fn: ()       => { void mongoExplainRun(t, t.explainOf, "allPlansExecution"); } },
    ];
  }
  if (t.pane === "indexes") return [{ label: tr("dataGrid.refresh"), fn: ()       => { t.indexes = null; renderDbGrid(); } }];
  if (t.pane === "validation") return [{ label: tr("dataMongo.revertValidator"), fn: ()       => { t.validatorText = null; renderDbGrid(); } }];
  return [];
}

function toggleTextMode(t           )       {
  if (t.aggText === null) {
    t.aggText = pipelineText(t);
  } else {
    loadPipeline(t, t.aggText);
    if (t.aggText !== null) { toast(tr("dataMongo.textModeKept"), true); return; }
  }
  renderDbGrid();
}

function indexAt(t           , raw                    )                       {
  const r = t.indexes;
  return r ? r.indexes[Number(raw)] || null : null;
}

export function mongoPanesClick(t         , _ev            , tab           )          {
  if (t.closest("[data-maggrun]")) { void mongoAggRun(tab); return true; }
  if (t.closest("[data-maggadd]")) {
    const op = tab.stages.length ? "$project" : "$match";
    tab.stages.push({ op, body: opBody(op), on: true, preview: null, error: null });
    renderDbGrid();
    return true;
  }
  if (t.closest("[data-maggcopy]")) { if (tab.aggResult) dbCopyText(shellText(tab.aggResult.docs)); return true; }
  const pv = t.closest             ("[data-mstagepreview]");
  if (pv) { void mongoStagePreview(tab, Number(pv.dataset.mstagepreview)); return true; }
  const up = t.closest             ("[data-mstageup]");
  if (up) {
    const i = Number(up.dataset.mstageup);
    if (i > 0) { const [s] = tab.stages.splice(i, 1); tab.stages.splice(i - 1, 0, s); tab.stages.forEach((x) => { x.preview = null; }); renderDbGrid(); }
    return true;
  }
  const down = t.closest             ("[data-mstagedown]");
  if (down) {
    const i = Number(down.dataset.mstagedown);
    if (i < tab.stages.length - 1) { const [s] = tab.stages.splice(i, 1); tab.stages.splice(i + 1, 0, s); tab.stages.forEach((x) => { x.preview = null; }); renderDbGrid(); }
    return true;
  }
  const rm = t.closest             ("[data-mstagerm]");
  if (rm) { tab.stages.splice(Number(rm.dataset.mstagerm), 1); tab.stages.forEach((x) => { x.preview = null; }); renderDbGrid(); return true; }
  if (t.closest("[data-mschemarun]")) { void mongoSchemaRun(tab); return true; }
  const fv = t.closest             ("[data-mfpath]");
  if (fv && fv.dataset.mfpath && fv.dataset.mfval !== undefined) {
    // A top value becomes the documents' filter: the schema's "show me those" in one click.
    const path = fv.dataset.mfpath;
    let value         ;
    try { value = JSON.parse(fv.dataset.mfval); } catch { return true; }
    tab.qFilter = shellText({ [path]: value }, { oneLine: true });
    tab.qSkip = 0;
    tab.pane = "docs";
    renderDbToolbar(); renderDbFilters();
    void mongoLoadDocs(tab);
    return true;
  }
  if (t.closest("[data-mixadd]")) { openCreateIndex(tab); return true; }
  const hide = t.closest             ("[data-mixhide]");
  if (hide) {
    const ix = indexAt(tab, hide.dataset.mixhide);
    if (ix) void mongoIndexOp(tab, { op: ix.hidden ? "unhide" : "hide", name: ix.name });
    return true;
  }
  const drop = t.closest             ("[data-mixdrop]");
  if (drop) {
    const ix = indexAt(tab, drop.dataset.mixdrop);
    if (ix && confirm(tr("dataMongo.ixDropAsk", { name: ix.name }))) {
      void mongoIndexOp(tab, { op: "drop", name: ix.name }).then((ok         )       => { if (ok) toast(tr("dataMongo.indexDropped")); });
    }
    return true;
  }
  if (t.closest("[data-mexplainrun]")) { void mongoExplainRun(tab, tab.explainOf); return true; }
  if (t.closest("[data-mvalsave]")) { void mongoSaveValidator(tab); return true; }
  return false;
}

export function mongoPanesInput(t         , tab           )          {
  const body = t.closest                     ("[data-mstagebody]");
  if (body) {
    const s = tab.stages[Number(body.dataset.mstagebody)];
    if (s) { s.body = body.value; s.error = null; }
    return true;
  }
  const text = t.closest                     ("[data-maggtext]");
  if (text) { tab.aggText = text.value; return true; }
  const val = t.closest                     ("[data-mval]");
  if (val) { tab.validatorText = val.value; return true; }
  return false;
}

export function mongoPanesChange(t         , tab           )          {
  const op = t.closest                   ("[data-mstageop]");
  if (op) {
    const s = tab.stages[Number(op.dataset.mstageop)];
    if (s) {
      // A body still at the old operator's template follows the new operator; a typed one stays.
      if (s.body.trim() === opBody(s.op).trim()) s.body = opBody(op.value);
      s.op = op.value;
      s.preview = null;
      renderDbGrid();
    }
    return true;
  }
  const on = t.closest                  ("[data-mstageon]");
  if (on) {
    const s = tab.stages[Number(on.dataset.mstageon)];
    if (s) { s.on = on.checked; tab.stages.forEach((x) => { x.preview = null; }); renderDbGrid(); }
    return true;
  }
  const sample = t.closest                   ("[data-msample]");
  if (sample) { tab.schemaSample = Number(sample.value) || 1000; return true; }
  return false;
}

/** Ctrl+Enter in a stage previews it; in text mode it runs the pipeline. */
export function mongoPanesKeydown(t         , ev               , tab           )          {
  if (!(ev.ctrlKey || ev.metaKey) || ev.key !== "Enter") return false;
  const body = t.closest                     ("[data-mstagebody]");
  if (body) {
    ev.preventDefault();
    const s = tab.stages[Number(body.dataset.mstagebody)];
    if (s) s.body = body.value;
    void mongoStagePreview(tab, Number(body.dataset.mstagebody));
    return true;
  }
  if (t.closest("[data-maggtext]")) { ev.preventDefault(); void mongoAggRun(tab); return true; }
  return false;
}

/** The query bar's Find while the Explain pane is in front: explain that find. */
export function mongoExplainFind(tab           )       { void mongoExplainRun(tab, "find"); }

/** Whether destroying data is allowed (the panes ask before offering $out). */
export function mongoPanesDestroyAllowed()          { return mongoAllowsDestroy(); }
