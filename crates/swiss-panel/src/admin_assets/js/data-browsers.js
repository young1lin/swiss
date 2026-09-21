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

                                                                                                  
                                                                     
import { $, apiJson, dbReqGuard, el, emptyNode, errText, iconNode, toast } from "./util.js";
import { fill, h } from "./h.js";
import { closeSheet } from "./add-sheet.js";
import { renderDbFilters } from "./data-filters.js";
import { renderDbGrid } from "./data-grid.js";
import { renderDbBar } from "./data-sql.js";
// Cycle with data-edit.js (it reads dbIsRedis/dbLoadKeys from here): function declarations,
// runtime-only use — the same shape as the data-sql import above.
import { dbTypedConfirm } from "./data-edit.js";
import { renderDbTables } from "./data-view.js";
import { popupMenu } from "./menu.js";
import { dbConn, dbTab } from "./db-state.js";
import { tk, tr, trn } from "./i18n.js";

/* --- redis key browser -------------------------------------------------------------------------- */
/* A redis connection in the picker swaps the table list for a SCAN-paged key list, and the
   right pane for a type-aware value view (docs/22 W3.3): a string edits in place (one SET)
   and the container types render as typed tables whose edits buffer locally and Commit as
   ONE guarded pipeline round trip — HSET/HDEL for hashes, ZADD/ZREM for zsets, LSET/RPUSH
   for lists, SADD/SREM for sets. Every buffered command is field-addressed, so no MULTI: a
   refused or failed command stops the run and the operator re-commits the rest. */

function dbIsRedis()          {
  const d = dbConn();
  const c = d.conns.find((x                    )          => { return x.name === d.conn; });
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

async function dbLoadKeys(reset                     )                {
  const d = dbConn();
  if (!d.conn) return;
  if (!reset && dbKeysLoading) return; // the More double-click: the page is already on its way
  if (reset) {
    d.redis = null;
    const t0 = dbTab();
    if (t0.kind === "key") t0.redisKey = null; // a reset walk deselects the open key
  }
  let q = "/api/db/" + encodeURIComponent(d.conn) + "/keys?count=200";
  if (d.grep) q += "&pattern=" + encodeURIComponent(d.grep);
  if (d.redisType) q += "&type=" + encodeURIComponent(d.redisType);
  if (d.redis && d.redis.cursor && d.redis.cursor !== "0") q += "&cursor=" + encodeURIComponent(d.redis.cursor);
  const token = dbKeysReq.issue();
  let j                               ;
  dbKeysLoading = true;
  try {
    j = await apiJson                        (q);
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

async function dbLoadRedisValue(key        )                {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "key") return; // a redis connection's tab is kind "key" (docs/42 T1)
  // A fresh key selection is a navigation: drop the command result that owned the pane,
  // or the grid guard would keep rendering it and the value would never show.
  c.sqlResult = null;
  c.sqlResults = null; c.resultTab = 0; // docs/22 W4.3: key navigation closes every result tab
  c.sqlBusy = false;
  d.redisKey = key;
  d.redisValue = null; // drop the previous key's value — never flash stale data
  d.redisEdits = null; // and its buffered edits — a different key cannot adopt them
  renderDbGrid();
  const token = dbValueReq.issue();
  const j = await apiJson                 ("/api/db/" + encodeURIComponent(c.conn ) + "/key?key=" + encodeURIComponent(key));
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
   cannot honor (dbgate's list changeset stops at LSET/RPUSH for the same reason).
   `add` holds a tk()-marked key (docs/38 L7): the button paints it through tr() at render
   time, so a language flip is honored — a module-eval tr() would freeze English.
   `cols` and `thing` themselves stay WIRE words (cell addressing compares them), so their
   display forms go through the tk()-marked label maps below at paint time. */
/* Display labels for the wire vocabulary above (docs/38 L7 tk idiom): the th heads and the
 * add-button's {thing} noun are copy; the underlying strings are cell keys, not copy. */
const REDIS_COL_KEYS                         = {
  field: tk("dataBrowsers.colField"), value: tk("dataBrowsers.colValue"),
  member: tk("dataBrowsers.colMember"), score: tk("dataBrowsers.colScore"),
  index: tk("dataBrowsers.colIndex"),
};
const REDIS_THING_KEYS                         = {
  field: tk("dataBrowsers.thingField"), member: tk("dataBrowsers.thingMember"),
  item: tk("dataBrowsers.thingItem"),
};

const DB_REDIS_TYPES                                 = {
  hash: { cols: ["field", "value"], edit: ["value"], ins: ["field", "value"], thing: "field", deletable: true, add: tk("dataBrowsers.addField") },
  zset: { cols: ["member", "score"], edit: ["score"], ins: ["member", "score"], thing: "member", deletable: true, add: tk("dataBrowsers.addMember") },
  list: { cols: ["index", "value"], edit: ["value"], ins: ["value"], thing: "item", deletable: false, add: tk("dataGrid.row") },
  set: { cols: ["member"], edit: [], ins: ["member"], thing: "member", deletable: true, add: tk("dataBrowsers.addMember") },
};

/** A zset score is a number or redis' own inf spellings — anything else refuses Commit
 *  rather than shipping a command the server would bounce. Pure. */
function dbRedisValidScore(s        )          {
  return /^-?(\d+\.?\d*|\.\d+)$/.test(s) || s === "inf" || s === "-inf";
}

/** The buffer for the key in view, rebuilt when the key or its type changed. Null when the
 *  view holds no editable container. */
function dbRedisEdits()                      {
  const d = dbTab();
  if (d.kind !== "key") return null;
  const v = d.redisValue;
  if (!d.redisKey || !v || !DB_REDIS_TYPES[v.type]) return null;
  if (!d.redisEdits || d.redisEdits.key !== d.redisKey || d.redisEdits.type !== v.type) {
    d.redisEdits = { key: d.redisKey , type: v.type, updates: {}, deletes: {}, inserts: [] };
  }
  return d.redisEdits;
}

/** How many changes the bar counts. Pure over the buffer. */
function dbRedisPendingCount()         {
  const t = dbTab();
  const b = t.kind === "key" ? t.redisEdits : null;
  if (!b) return 0;
  return Object.keys(b.updates).length + Object.keys(b.deletes).length + b.inserts.length;
}

/** Fold the buffered edits into the exact command list Commit posts — updates, inserts, then
 *  deletes, the order dbgate's ChangeSetRedis uses, so an author comparing the two reads the
 *  same script. Scores and list indexes go as NUMBERS (the wire form); a blank address or a
 *  bad score throws so Commit refuses instead of silently dropping a half-filled row. Pure:
 *  the bar's preview and the POST body are both built from this list, so what the operator
 *  reads is exactly what runs. */
function dbRedisCommands(key        , type        , buf                                                                                                            )                                      {
  // A zset score goes on the wire as a number — except redis' own inf spellings, which JSON
  // cannot number and travel as the strings ZADD accepts verbatim.
  const scoreArg = (s        )                  => { return s === "inf" || s === "-inf" ? s : Number(s); };
  const addrOf                         = { hash: "field", zset: "member", set: "member" };
  const addr = addrOf[type];
  let i        , ins                         ;
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
    Object.keys(buf.updates).forEach((m        )       => {
      if (!dbRedisValidScore(String(buf.updates[m]))) {
        throw new Error('not a valid score for "' + m + '": use a number, or inf / -inf');
      }
    });
  }
  const out                                      = [];
  const push = (verb        , args           )       => { out.push({ verb: verb, args: args }); };
  Object.keys(buf.updates).forEach((a        )       => {
    if (type === "hash") push("HSET", [key, a, buf.updates[a]]);
    else if (type === "zset") push("ZADD", [key, scoreArg(buf.updates[a]          ), a]);
    else if (type === "list") push("LSET", [key, Number(a), buf.updates[a]]);
  });
  for (i = 0; i < buf.inserts.length; i++) {
    ins = buf.inserts[i];
    if (type === "hash") push("HSET", [key, ins.field, ins.value]);
    else if (type === "zset") push("ZADD", [key, scoreArg(ins.score          ), ins.member]);
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
function dbRedisCommandText(cmd                                   )         {
  return cmd.verb + " " + cmd.args.map((a         )         => {
    return typeof a === "number" ? String(a) : '"' + String(a).replace(/"/g, '\\"') + '"';
  }).join(" ");
}

/** One row of the typed table in a uniform shape: `addr` is what commands address (field /
 *  member / index string), `cells` the per-column text with buffered edits applied, the
 *  flags the pending state the row paints. Pure. */
function dbRedisEntries(v                 , buf              )                                                                                         {
  const out                                                                                         = [];
  const val = v.value                            || {};
  if (v.type === "hash" || v.type === "zset") {
    Object.keys(val).forEach((a        )       => {
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
    (v.value              || []).forEach((item         , i        )       => {
      const a = String(i);
      const upd = buf.updates[a];
      out.push({ addr: a, deleted: false, updated: upd != null, cells: { index: a, value: upd != null ? upd : String(item) } });
    });
  } else if (v.type === "set") {
    (v.value              || []).forEach((m         )       => {
      out.push({ addr: String(m), deleted: !!buf.deletes[m          ], updated: false, cells: { member: String(m) } });
    });
  }
  return out;
}

function dbRenderRedisValue(wrap             )       {
  const d = dbTab();
  if (d.kind !== "key") {
    // The shared empty state (docs/18 V7) — a non-key tab on a redis connection is the
    // "no key opened yet" state.
    wrap.appendChild(emptyNode({ icon: "database", title: tr("dataBrowsers.selectKey"), hint: tr("dataBrowsers.pickKeyLeftView") }));
    return;
  }
  if (!d.redisKey) {
    // The shared empty state (docs/18 V7).
    wrap.appendChild(emptyNode({ icon: "database", title: tr("dataBrowsers.selectKey"), hint: tr("dataBrowsers.pickKeyLeftView") }));
    return;
  }
  const v = d.redisValue;
  if (!v) { wrap.appendChild(el("div", "db-hint", tr("dataBrowsers.loadingK", { k: d.redisKey }))); return; }
  const meta = el("div", "db-detail-meta");
  meta.appendChild(document.createTextNode(v.key + " · " + v.type + " · "));
  meta.appendChild(dbRedisTtl(v));
  if (v.length != null) meta.appendChild(document.createTextNode(" · " + trn(v.length, "dataBrowsers.nEntries.one", "dataBrowsers.nEntries.other")));
  if (v.truncated) meta.appendChild(document.createTextNode(" · " + tr("dataBrowsers.truncated")));
  meta.appendChild(el("span", "grow"));
  const cfg = DB_REDIS_TYPES[v.type];
  if (cfg) {
    // One add action per view; it buffers a row, never touches redis directly. data-radd —
    // the click resolves the live type config at event time (docs/37 R5).
    meta.appendChild(h("button", {
      class: "btn", type: "button", title: tr("dataBrowsers.bufferNewThingApplied", { thing: tr(REDIS_THING_KEYS[cfg.thing] ?? cfg.thing) }),
    }, tr(cfg.add)));
  }
  // docs/22 W1.3: the key's own actions ride the value header. Rename takes a one-input
  // sheet, Delete is last and red and demands the key name typed back — the same confirm
  // vocabulary as the table DDL ops. Everything runs through the guarded /command console.
  // stopPropagation lives in the dispatcher (dbRedisClick): connect.js closes any open menu
  // on clicks that reach document, and without it the same click tears the menu back down.
  meta.appendChild(h("button", {
    class: "btn icon", type: "button", title: tr("dataBrowsers.renameDeleteKey"), data: { rkeymenu: "" },
  }, iconNode("ellipsis")));
  wrap.appendChild(meta);
  if (v.type === "none") {
    wrap.appendChild(el("div", "db-hint", tr("dataBrowsers.keyFoundMayExpired")));
    return;
  }
  if (v.note) wrap.appendChild(el("div", "db-hint", v.note));
  if (cfg) { dbRedisTypedTable(wrap, v, cfg); return; }
  if (v.type === "string") { dbRedisStringEditor(wrap, v); return; }
  // Stream and module types stay read-only — their commands have no field grid (docs/22 W3.3).
  const pre = el("pre", "db-ddl");
  pre.style.position = "static";
  pre.style.margin = "var(--s2)";
  // The value is data, not copy; the whole conditional is hoisted to a const because the
  // i18n gate's ternary scan counts the comparison literal in the condition as bare copy.
  const valueText = typeof v.value === "string" ? v.value : JSON.stringify(v.value, null, 2);
  pre.textContent = valueText;
  wrap.appendChild(pre);
}

/** The TTL readout is itself the control (docs/22 W3.3): click to edit in place, Enter runs
 *  EXPIRE — or PERSIST when emptied — through the guarded console, then the key re-reads.
 *  The button carries data-rttl; #pane's delegated click (dbRedisClick) builds the in-place
 *  input from the LIVE value, so a poll that moved the TTL between render and click seeds
 *  the editor with what the key says now (docs/37 §10.1). */
function dbRedisTtl(v                 )              {
  return h("button", {
    class: "db-ttl", type: "button",
    title: tr("dataBrowsers.changeTtlEnterApplies"), data: { rttl: "" },
  }, tr(v.ttl  < 0 ? "dataBrowsers.noExpiry" : "dataBrowsers.ttlSeconds", { n: v.ttl  }));
}

/** The typed table — the row grid's own vocabulary (inserts first, db-dirty cells, db-del
 *  struck rows, the narrow ✕/↩ control column), pointing at the redis buffer instead of the
 *  SQL edit buffer. */
function dbRedisTypedTable(wrap             , v                 , cfg                )       {
  const b = dbRedisEdits();
  if (!b) return;
  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  hr.appendChild(el("th", "db-rowctl", ""));
  cfg.cols.forEach((c        )       => {
    const th = el("th", "db-col", tr(REDIS_COL_KEYS[c] ?? c));
    th.classList.add("db-rowctl"); // no sort affordance on a typed value table
    hr.appendChild(th);
  });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");

  // Buffered inserts first, so they read as "the row you are about to add". The row control
  // carries its address (data-rins / data-raddr) — #pane's delegated click owns the behavior
  // (docs/37 R5), resolving the buffer live at event time.
  b.inserts.forEach((ins                         , i        )       => {
    const tri = el("tr", "db-ins");
    tri.appendChild(dbRedisRowCtl(false, false, null, i));
    cfg.cols.forEach((c        )       => {
      const editable = cfg.ins.includes(c);
      const filled = editable && ins[c] != null && String(ins[c]) !== "";
      const td = el("td", "db-cell db-cell-edit" + (filled ? " db-dirty" : ""));
      td.textContent = ins[c] == null ? "" : String(ins[c]);
      if (editable) {
        td.title = tr("dataBrowsers.doubleClickEdit");
        td.ondblclick = ()       => {
          dbRedisCellEdit(td, i, "insert", c, ins[c] == null ? "" : String(ins[c]));
        };
      }
      tri.appendChild(td);
    });
    tbody.appendChild(tri);
  });

  dbRedisEntries(v, b).forEach((entry                                                                                      )       => {
    const tri = el("tr", entry.deleted ? "db-del" : "");
    tri.appendChild(dbRedisRowCtl(cfg.deletable, entry.deleted, entry.addr, -1));
    cfg.cols.forEach((c        )       => {
      const editable = cfg.edit.includes(c) && !entry.deleted;
      const td = el("td", "db-cell" + (entry.updated && cfg.edit.includes(c) ? " db-dirty" : "") + (editable ? " db-cell-edit" : ""));
      td.textContent = entry.cells[c] == null ? "" : String(entry.cells[c]);
      td.title = td.textContent || ""; // long values truncate in the cell; the full text is one hover away
      if (editable) {
        td.ondblclick = ()       => {
          // -1, never an insert index: save() reads >= 0 as "this is a buffered insert row",
        // and a 0 here once made every existing-row edit vanish into inserts[0] (caught live).
        dbRedisCellEdit(td, -1, entry.addr, c, String(entry.cells[c]));
        };
      }
      tri.appendChild(td);
    });
    tbody.appendChild(tri);
  });

  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
}

/** The narrow ✕/↩ column the row grid uses, in the value view's words: ✕ buffers a delete
 *  (HDEL / ZREM / SREM on Commit) or removes a buffered insert, ↩ undoes a delete. A type
 *  with no honest delete command (list) gets no control at all. The button's address is
 *  data-raddr (a stored entry) or data-rins (a buffered insert row's index). */
function dbRedisRowCtl(deletable         , deleted         , addr               , insIdx        )              {
  const td = el("td", "db-rowctl");
  if (!deletable && !deleted) return td;
  td.appendChild(h("button", {
    class: "db-act", type: "button",
    title: tr(deleted ? "dataBrowsers.undoBufferedDelete" : "dataBrowsers.bufferDeleteCommit"),
    data: addr != null ? { raddr: addr } : { rins: String(insIdx) },
  }, deleted ? "↩" : "✕"));
  return td;
}

function dbRedisToggleDelete(addr        )       {
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
let dbRedisEditor                                                       = null; // { ta, save } — close() must be able to unhook the dismiss too

function dbRedisCellEdit(td             , insertIdx        , addr        , col        , current        )       {
  dbRedisEditorClose();
  const wrap = $("dbGridWrap");
  if (!wrap) return;
  const wrapRect = wrap.getBoundingClientRect();
  const rect = td.getBoundingClientRect();
  const ta = el("textarea", "db-inline-edit")                       ;
  ta.rows = 1;
  ta.spellcheck = false;
  ta.value = current;
  ta.style.left = (rect.left - wrapRect.left + wrap.scrollLeft) + "px";
  ta.style.top = (rect.top - wrapRect.top + wrap.scrollTop) + "px";
  ta.style.width = Math.max(rect.width + 24, 96) + "px";
  ta.style.minHeight = Math.max(rect.height, 22) + "px";
  wrap.appendChild(ta);
  dbRedisEditor = { ta: ta, save: save };
  function autoSize()       {
    ta.style.height = "auto";
    ta.style.height = Math.min(Math.max(ta.scrollHeight, 22), 320) + "px";
  }
  function save()       {
    const raw = ta.value;
    dbRedisEditorClose();
    const b = dbRedisEdits();
    if (!b) return;
    if (col === "score" && !dbRedisValidScore(raw)) {
      toast(tr("dataBrowsers.scoreNumberInfInf"), true);
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
  ta.onkeydown = (e               )       => {
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
function dbRedisDismiss(ev            )       {
  if (!dbRedisEditor) return;
  const ta = dbRedisEditor.ta;
  if (ev.target === ta || ta.contains(ev.target        )) return;
  dbRedisEditor.save();
}

function dbRedisEditorClose()       {
  if (dbRedisEditor) {
    dbRedisEditor.ta.remove();
    document.removeEventListener("mousedown", dbRedisDismiss, true);
    dbRedisEditor = null;
  }
}

/** The Set action, shared by the button and the textarea's Ctrl+Enter — state at event
 *  time (docs/37 R5): the value read is whatever the textarea holds when the click lands. */
async function dbRedisSetString(ta                     )                {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "key" || !d.redisKey) return;
  const value = ta.value;
  if (value === ta.dataset.orig) { toast(tr("dataBrowsers.unchanged")); return; }
  const j = await apiJson("/api/db/" + encodeURIComponent(c.conn ) + "/redis-pipeline", {
    method: "POST",
    body: JSON.stringify({ commands: [["SET", d.redisKey, value]] }),
  });
  if (!j) return;
  toast(tr("dataBrowsers.setKey", { key: d.redisKey ?? "" }));
  void dbLoadRedisValue(d.redisKey);
}

/** docs/22 W3.3: a string edits in place — one textarea, one Set, one SET through the
 *  pipeline (a value keeps its spaces: pipeline args bind as separate strings, which the
 *  whitespace-splitting /command console can never promise). The textarea carries
 *  data-rstr and the button data-rset; #pane's delegated click/keydown answer both. */
function dbRedisStringEditor(wrap             , v                 )       {
  const row = el("div", "db-redis-str-row");
  const ta = el("textarea", "db-redis-str")                       ;
  ta.rows = 3;
  ta.spellcheck = false;
  ta.value = typeof v.value === "string" ? v.value : "";
  ta.dataset.orig = ta.value;
  ta.setAttribute("data-rstr", "");
  row.appendChild(ta);
  row.appendChild(h("button", { class: "btn primary", type: "button", data: { rset: "" } }, tr("dataBrowsers.set")));
  wrap.appendChild(row);
}

/** Commit the buffer: ONE pipeline round trip. The POST body is built from the very list the
 *  preview printed (dbRedisCommands), so the two cannot drift. A refusal keeps the buffer —
 *  the toast already said which command and why. */
async function dbRedisCommit()                {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "key") return;
  const b = dbRedisEdits();
  if (!b) return;
  let cmds                                     ;
  try {
    cmds = dbRedisCommands(d.redisKey , b.type, b);
  } catch (err) {
    toast(errText(err), true);
    return;
  }
  if (!cmds.length) return;
  if (!confirm(tr("dataBrowsers.commitNKOne", { n: trn(cmds.length, "dataBrowsers.nCommands.one", "dataBrowsers.nCommands.other"), k: d.redisKey ?? "" }))) return;
  const j = await apiJson("/api/db/" + encodeURIComponent(c.conn ) + "/redis-pipeline", {
    method: "POST",
    body: JSON.stringify({ commands: cmds.map((x                                   )            => { return [x.verb           ].concat(x.args); }) }),
  });
  if (!j) return;
  toast(tr("dataBrowsers.committedN", { n: trn(cmds.length, "dataBrowsers.nCommands.one", "dataBrowsers.nCommands.other") }));
  d.redisEdits = null;
  d.sqlPreview = false;
  const key = d.redisKey;
  // Awaited: deleting a hash's last field (or a set's last member) deletes the KEY, and the
  // re-read's "type none" answer IS the signal — checking before it answered read null and
  // the list refresh never ran, so the sidebar kept offering a key that is gone (docs/22
  // closeout audit).
  await dbLoadRedisValue(key );
  const t2 = dbTab();
  if (t2.kind === "key" && t2.redisKey === key && t2.redisValue && t2.redisValue.type === "none") void dbLoadKeys(true);
}

/** Drop the buffer without a single command — the twin of the row grid's Discard. */
function dbRedisDiscard()       {
  const d = dbTab();
  if (d.kind !== "key") return;
  const n = dbRedisPendingCount();
  if (!n) return;
  if (!confirm(tr("dataBrowsers.discardNNothingBeen", { n: trn(n, "dataBrowsers.nBufferedChanges.one", "dataBrowsers.nBufferedChanges.other") }))) return;
  d.redisEdits = null;
  d.sqlPreview = false;
  renderDbGrid();
  renderDbBar();
}

function dbRedisKeyMenu(anchorEl             )       {
  const t = dbTab();
  const key = t.kind === "key" ? t.redisKey : null;
  popupMenu(anchorEl.getBoundingClientRect(), [
    { label: tr("dataBrowsers.rename"), fn: ()       => { dbRedisRenameSheet(key); } },
    { sep: true },
    { label: tr("dataBrowsers.delete"), danger: true, fn: dbRedisDeleteKey },
  ]);
}

/** One guarded console command; null means the toast already said why. */
async function dbRedisCommand(line        )                   {
  // No connection is not an error worth a toast — it is the console being asked to run
  // against nothing, which the null return already says (docs/37 R4: the assertion this
  // replaced claimed a selection the record's type never promised).
  const conn = dbConn().conn;
  if (!conn) return null;
  return apiJson("/api/db/" + encodeURIComponent(conn) + "/command", {
    method: "POST",
    body: JSON.stringify({ command: line }),
  });
}

/** The one-input sheet Rename uses (docs/22 W1.3). `submit(value)` runs after the sheet
 *  closes; it owns its own reload and error handling. */
function dbRedisKeySheet(cfg                                                                                                                      )       {
  // docs/37 R5: the sheet is a node tree (every remote string lands as a TEXT node — no
  // esc() anywhere), painted AFTER the host is unhidden so the first paint is never into a
  // hidden box the user's click seemed to ignore.
  $("sheet").hidden = false;
  fill($("sheet"),
    h("div", { class: "sheet", role: "dialog", aria: { modal: "true", label: cfg.title } },
      h("div", { class: "sheet-head" }, h("h2", null, cfg.title)),
      h("div", { class: "sheet-body" },
        h("label", { class: "field" }, h("span", null, cfg.label), h("input", { id: "dbKeyIn", autocomplete: "off" }))),
      h("div", { class: "sheet-foot" }, h("span", { class: "grow" }),
        h("button", { class: "btn", id: "dbKeyCancel" }, tr("dataBrowsers.cancel")),
        h("button", { class: "btn primary", id: "dbKeyGo" }, cfg.primary))));
  const input = $                  ("dbKeyIn");
  input.value = cfg.value || "";
  input.placeholder = cfg.placeholder || "";
  $("dbKeyCancel").onclick = closeSheet;
  $("sheet").onclick = (e            )       => { if (e.target === $("sheet")) closeSheet(); };
  function go()       { const v         = input.value.trim(); closeSheet(); cfg.submit(v); }
  $("dbKeyGo").onclick = go;
  input.onkeydown = (e               )       => {
    if (e.key === "Enter") { e.preventDefault(); go(); }
  };
  input.focus();
  input.select();
}

function dbRedisRenameSheet(key               )       {
  // The menu that opens this only exists over a selected key, but the record says the
  // selection is nullable and that is the truth to honour — not an assertion at the call.
  if (!key) return;
  dbRedisKeySheet({
    title: tr("dataBrowsers.renameKey"),
    label: tr("dataBrowsers.newName"),
    value: key,
    primary: tr("dataBrowsers.rename2"),
    submit: async (to        )                => {
      if (!to || to === key) return;
      const j = await dbRedisCommand("RENAME " + key + " " + to);
      if (!j) return;
      toast(tr("dataBrowsers.renamed", { to }));
      const t = dbTab();
      if (t.kind === "key") t.redisKey = to;
      await dbLoadKeys(true);
      void dbLoadRedisValue(to);
    },
  });
}

/* Deleting a key is the one destructive act the redis side has — the same typed-name confirm
   the table DROP/TRUNCATE use (a W1 audit follow-up), with the key's own words. */
function dbRedisDeleteKey()       {
  const d = dbTab();
  if (d.kind !== "key") return;
  const key = d.redisKey ;
  dbTypedConfirm({ what: tr("dataEdit.whatDelete"), name: key, kind: "key" }, ()       => {
    void dbRedisCommand("DEL " + key).then(async (j         )                => {
      if (!j) return;
      toast(tr("dataBrowsers.deletedKey", { key }));
      const t = dbTab();
      if (t.kind === "key") {
        t.redisKey = null;
        t.redisValue = null;
        t.redisEdits = null;
      }
      await dbLoadKeys(true);
      renderDbGrid();
    });
  });
}

/* --- #pane's delegated listeners for the redis value view (docs/37 R5) ----------------------------
   Behavior notes (docs/37 §10.1): every action resolves its key, buffer and type config
   from LIVE state at event time — the add-row shape, the TTL editor's seed value and the
   row-control addresses all re-read dbTab(), so a re-read or a key switch between render
   and click acts on what is on screen now, never on the painted snapshot. */

function dbRedisClick(t         , ev            )          {
  const d = dbTab();
  if (d.kind !== "key") return false; // the value view's affordances belong to the key tab
  if (t.closest("[data-radd]")) {
    const v = d.redisValue;
    const cfg = v ? DB_REDIS_TYPES[v.type] : null;
    const b = dbRedisEdits();
    if (!cfg || !b) return true;
    const ins                          = {};
    cfg.ins.forEach((c        )       => { ins[c] = c === "score" ? "0" : ""; });
    b.inserts.push(ins);
    renderDbGrid();
    renderDbBar();
    return true;
  }
  const keyMenu = t.closest             ("[data-rkeymenu]");
  if (keyMenu) {
    // stopPropagation: connect.js closes any open menu on clicks that reach document, and
    // without this the very click that opens the menu also tears it down.
    ev.stopPropagation();
    dbRedisKeyMenu(keyMenu);
    return true;
  }
  if (t.closest("[data-rttl]")) {
    const btn = t.closest             ("[data-rttl]");
  if (!btn) return false;
    const v = d.redisValue;
    if (!v) return true;
    const input = el("input", "db-ttl-in")                    ;
    input.value = v.ttl  < 0 ? "" : String(v.ttl);
    input.placeholder = tr("dataBrowsers.seconds");
    btn.replaceWith(input);
    input.focus();
    input.select();
    let ran = false;
    const apply = ()       => {
      if (ran) return;
      ran = true;
      const secs         = input.value.trim();
      if (secs && !/^\d+$/.test(secs)) { toast(tr("dataBrowsers.ttlMustWholeNumber"), true); void dbLoadRedisValue(d.redisKey ); return; }
      const line = secs ? "EXPIRE " + d.redisKey + " " + secs : "PERSIST " + d.redisKey;
      void dbRedisCommand(line).then((j         )       => {
        if (j) toast(tr(secs ? "dataBrowsers.ttlSetSeconds" : "dataBrowsers.ttlRemoved", { s: secs }));
        void dbLoadRedisValue(d.redisKey );
      });
    };
    // Transient in-place overlay, the sheet idiom (docs/37 R5 keeps per-open wiring): the
    // input lives until Enter/Esc/blur, so it owns its handlers for that lifetime only.
    input.onkeydown = (e               )       => {
      e.stopPropagation();
      if (e.key === "Enter") { e.preventDefault(); apply(); }
      else if (e.key === "Escape") { e.preventDefault(); ran = true; void dbLoadRedisValue(d.redisKey ); }
    };
    input.onblur = apply;
    return true;
  }
  const raddr = t.closest             ("[data-raddr]");
  if (raddr) { dbRedisToggleDelete(raddr.dataset.raddr || ""); return true; }
  const rins = t.closest             ("[data-rins]");
  if (rins) {
    const b = dbRedisEdits();
    if (b) {
      b.inserts.splice(Number(rins.dataset.rins), 1);
      renderDbGrid();
      renderDbBar();
    }
    return true;
  }
  if (t.closest("[data-rset]")) {
    const ta = document.querySelector                     ("textarea[data-rstr]");
    if (ta) void dbRedisSetString(ta);
    return true;
  }
  return false;
}

function dbRedisKeydown(t         , ev               )          {
  const ta = t.closest                     ("[data-rstr]");
  if (!ta) return false;
  if ((ev.ctrlKey || ev.metaKey) && ev.key === "Enter") {
    ev.preventDefault();
    ev.stopPropagation();
    void dbRedisSetString(ta);
  }
  return true;
}

export {
  DB_REDIS_TYPES, dbIsRedis, dbLoadKeys, dbLoadRedisValue, dbRedisValidScore,
  dbRedisClick, dbRedisCommandText, dbRedisCommands, dbRedisCommit, dbRedisDiscard,
  dbRedisEntries, dbRedisKeydown, dbRedisPendingCount, dbRenderRedisValue,
};
