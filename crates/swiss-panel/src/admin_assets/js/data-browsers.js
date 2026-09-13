import { $, apiJson, el, emptyHtml, esc, icon, state, toast } from "./util.js";
import { closeSheet } from "./add-sheet.js";
import { renderDbFilters } from "./data-filters.js";
import { renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { renderDbBar } from "./data-sql.js";
// Cycle with data-edit.js (it reads dbIsRedis/dbLoadKeys from here): function declarations,
// runtime-only use — the same shape as the data-sql import above.
import { dbTypedConfirm } from "./data-edit.js";
import { renderDbTables } from "./data-view.js";
import { popupMenu } from "./menu.js";

/* --- redis key browser -------------------------------------------------------------------------- */
/* A redis connection in the picker swaps the table list for a SCAN-paged key list, and the
   right pane for a type-aware value view (docs/22 W3.3): a string edits in place (one SET)
   and the container types render as typed tables whose edits buffer locally and Commit as
   ONE guarded pipeline round trip — HSET/HDEL for hashes, ZADD/ZREM for zsets, LSET/RPUSH
   for lists, SADD/SREM for sets. Every buffered command is field-addressed, so no MULTI: a
   refused or failed command stops the run and the operator re-commits the rest. */

function dbIsRedis() {
  var d = state.db;
  var c = d.conns.find(function (x) { return x.name === d.conn; });
  return !!c && c.dialect === "redis";
}

async function dbLoadKeys(reset) {
  var d = state.db;
  if (!d.conn) return;
  if (reset) { d.redis = null; d.redisKey = null; }
  var q = "/api/db/" + encodeURIComponent(d.conn) + "/keys?count=200";
  if (d.grep) q += "&pattern=" + encodeURIComponent(d.grep);
  if (d.redisType) q += "&type=" + encodeURIComponent(d.redisType);
  if (d.redis && d.redis.cursor && d.redis.cursor !== "0") q += "&cursor=" + encodeURIComponent(d.redis.cursor);
  var j = await apiJson(q);
  if (!j) return;
  d.redis = {
    keys: (d.redis && !reset ? d.redis.keys : []).concat(j.keys || []),
    cursor: j.cursor,
    done: !!j.done,
    total: j.total,
  };
  renderDbTables();
  renderDbFilters(); // the shown/keyspace readout rides on the pattern bar
}

async function dbLoadRedisValue(key) {
  var d = state.db;
  // A fresh key selection is a navigation: drop the command result that owned the pane,
  // or the grid guard would keep rendering it and the value would never show.
  d.sqlResult = null;
  d.sqlBusy = false;
  d.redisKey = key;
  d.redisValue = null; // drop the previous key's value — never flash stale data
  d.redisEdits = null; // and its buffered edits — a different key cannot adopt them
  renderDbGrid();
  var j = await apiJson("/api/db/" + encodeURIComponent(d.conn) + "/key?key=" + encodeURIComponent(key));
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
var DB_REDIS_TYPES = {
  hash: { cols: ["field", "value"], edit: ["value"], ins: ["field", "value"], thing: "field", deletable: true, add: "+ Field" },
  zset: { cols: ["member", "score"], edit: ["score"], ins: ["member", "score"], thing: "member", deletable: true, add: "+ Member" },
  list: { cols: ["index", "value"], edit: ["value"], ins: ["value"], thing: "item", deletable: false, add: "+ Row" },
  set: { cols: ["member"], edit: [], ins: ["member"], thing: "member", deletable: true, add: "+ Member" },
};

/** A zset score is a number or redis' own inf spellings — anything else refuses Commit
 *  rather than shipping a command the server would bounce. Pure. */
function dbRedisValidScore(s) {
  return /^-?(\d+\.?\d*|\.\d+)$/.test(s) || s === "inf" || s === "-inf";
}

/** The buffer for the key in view, rebuilt when the key or its type changed. Null when the
 *  view holds no editable container. */
function dbRedisEdits() {
  var d = state.db;
  var v = d.redisValue;
  if (!d.redisKey || !v || !DB_REDIS_TYPES[v.type]) return null;
  if (!d.redisEdits || d.redisEdits.key !== d.redisKey || d.redisEdits.type !== v.type) {
    d.redisEdits = { key: d.redisKey, type: v.type, updates: {}, deletes: {}, inserts: [] };
  }
  return d.redisEdits;
}

/** How many changes the bar counts. Pure over the buffer. */
function dbRedisPendingCount() {
  var b = state.db && state.db.redisEdits;
  if (!b) return 0;
  return Object.keys(b.updates).length + Object.keys(b.deletes).length + b.inserts.length;
}

/** Fold the buffered edits into the exact command list Commit posts — updates, inserts, then
 *  deletes, the order dbgate's ChangeSetRedis uses, so an author comparing the two reads the
 *  same script. Scores and list indexes go as NUMBERS (the wire form); a blank address or a
 *  bad score throws so Commit refuses instead of silently dropping a half-filled row. Pure:
 *  the bar's preview and the POST body are both built from this list, so what the operator
 *  reads is exactly what runs. */
function dbRedisCommands(key, type, buf) {
  // A zset score goes on the wire as a number — except redis' own inf spellings, which JSON
  // cannot number and travel as the strings ZADD accepts verbatim.
  var scoreArg = function (s) { return s === "inf" || s === "-inf" ? s : Number(s); };
  var addrOf = { hash: "field", zset: "member", set: "member" };
  var addr = addrOf[type];
  var i, ins;
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
    Object.keys(buf.updates).forEach(function (m) {
      if (!dbRedisValidScore(String(buf.updates[m]))) {
        throw new Error('not a valid score for "' + m + '": use a number, or inf / -inf');
      }
    });
  }
  var out = [];
  var push = function (verb, args) { out.push({ verb: verb, args: args }); };
  Object.keys(buf.updates).forEach(function (a) {
    if (type === "hash") push("HSET", [key, a, buf.updates[a]]);
    else if (type === "zset") push("ZADD", [key, scoreArg(buf.updates[a]), a]);
    else if (type === "list") push("LSET", [key, Number(a), buf.updates[a]]);
  });
  for (i = 0; i < buf.inserts.length; i++) {
    ins = buf.inserts[i];
    if (type === "hash") push("HSET", [key, ins.field, ins.value]);
    else if (type === "zset") push("ZADD", [key, scoreArg(ins.score), ins.member]);
    else if (type === "list") push("RPUSH", [key, ins.value]);
    else if (type === "set") push("SADD", [key, ins.member]);
  }
  Object.keys(buf.deletes).forEach(function (a) {
    if (type === "hash") push("HDEL", [key, a]);
    else if (type === "zset") push("ZREM", [key, a]);
    else if (type === "set") push("SREM", [key, a]);
  });
  return out;
}

/** One command as the preview shows it — dbgate's convertRedisCallListToScript shape: verb,
 *  then args with STRINGS double-quoted (inner quotes escaped) and numbers plain. Pure. */
function dbRedisCommandText(cmd) {
  return cmd.verb + " " + cmd.args.map(function (a) {
    return typeof a === "number" ? String(a) : '"' + String(a).replace(/"/g, '\\"') + '"';
  }).join(" ");
}

/** One row of the typed table in a uniform shape: `addr` is what commands address (field /
 *  member / index string), `cells` the per-column text with buffered edits applied, the
 *  flags the pending state the row paints. Pure. */
function dbRedisEntries(v, buf) {
  var out = [];
  var val = v.value || {};
  if (v.type === "hash" || v.type === "zset") {
    Object.keys(val).forEach(function (a) {
      var upd = buf.updates[a];
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
    (v.value || []).forEach(function (item, i) {
      var a = String(i);
      var upd = buf.updates[a];
      out.push({ addr: a, deleted: false, updated: upd != null, cells: { index: a, value: upd != null ? upd : String(item) } });
    });
  } else if (v.type === "set") {
    (v.value || []).forEach(function (m) {
      out.push({ addr: String(m), deleted: !!buf.deletes[m], updated: false, cells: { member: String(m) } });
    });
  }
  return out;
}

function dbRenderRedisValue(wrap) {
  var d = state.db;
  if (!d.redisKey) {
    // The shared empty state (docs/18 V7).
    wrap.innerHTML = emptyHtml({ icon: "database", title: "Select a key", hint: "Pick a key on the left to view its value." });
    return;
  }
  var v = d.redisValue;
  if (!v) { wrap.appendChild(el("div", "db-hint", "Loading " + d.redisKey + "…")); return; }
  var meta = el("div", "db-detail-meta");
  meta.appendChild(document.createTextNode(v.key + " · " + v.type + " · "));
  meta.appendChild(dbRedisTtl(v));
  if (v.length != null) meta.appendChild(document.createTextNode(" · " + v.length + " entries"));
  if (v.truncated) meta.appendChild(document.createTextNode(" · truncated"));
  meta.appendChild(el("span", "grow"));
  var cfg = DB_REDIS_TYPES[v.type];
  if (cfg) {
    // One add action per view; it buffers a row, never touches redis directly.
    var add = el("button", "btn", cfg.add);
    add.title = "Buffer a new " + cfg.thing + " — applied only on Commit";
    add.onclick = function () {
      var b = dbRedisEdits();
      if (!b) return;
      var ins = {};
      cfg.ins.forEach(function (c) { ins[c] = c === "score" ? "0" : ""; });
      b.inserts.push(ins);
      renderDbGrid();
      renderDbBar();
    };
    meta.appendChild(add);
  }
  // docs/22 W1.3: the key's own actions ride the value header. Rename takes a one-input
  // sheet, Delete is last and red and demands the key name typed back — the same confirm
  // vocabulary as the table DDL ops. Everything runs through the guarded /command console.
  var more = el("button", "btn icon");
  more.type = "button";
  more.innerHTML = icon("ellipsis");
  more.title = "Rename or delete this key";
  // stopPropagation or the document-level click closer (connect.js) eats the menu the same
  // click opened — every other popupMenu trigger does the same.
  more.onclick = function (e) { e.stopPropagation(); dbRedisKeyMenu(more); };
  meta.appendChild(more);
  wrap.appendChild(meta);
  if (v.type === "none") {
    wrap.appendChild(el("div", "db-hint", "Key not found — it may have expired."));
    return;
  }
  if (v.note) wrap.appendChild(el("div", "db-hint", v.note));
  if (cfg) { dbRedisTypedTable(wrap, v, cfg); return; }
  if (v.type === "string") { dbRedisStringEditor(wrap, v); return; }
  // Stream and module types stay read-only — their commands have no field grid (docs/22 W3.3).
  var pre = el("pre", "db-ddl");
  pre.style.position = "static";
  pre.style.margin = "var(--s2)";
  pre.textContent = typeof v.value === "string" ? v.value : JSON.stringify(v.value, null, 2);
  wrap.appendChild(pre);
}

/** The TTL readout is itself the control (docs/22 W3.3): click to edit in place, Enter runs
 *  EXPIRE — or PERSIST when emptied — through the guarded console, then the key re-reads. */
function dbRedisTtl(v) {
  var d = state.db;
  var btn = el("button", "db-ttl", v.ttl < 0 ? "no expiry" : v.ttl + "s");
  btn.type = "button";
  btn.title = "Change the TTL — Enter applies EXPIRE, empty removes it (PERSIST)";
  btn.onclick = function () {
    var input = el("input", "db-ttl-in");
    input.value = v.ttl < 0 ? "" : String(v.ttl);
    input.placeholder = "seconds";
    btn.replaceWith(input);
    input.focus();
    input.select();
    var ran = false;
    function apply() {
      if (ran) return;
      ran = true;
      var secs = input.value.trim();
      if (secs && !/^\d+$/.test(secs)) { toast("TTL must be a whole number of seconds", true); dbLoadRedisValue(d.redisKey); return; }
      var line = secs ? "EXPIRE " + d.redisKey + " " + secs : "PERSIST " + d.redisKey;
      void dbRedisCommand(line).then(function (j) {
        if (j) toast(secs ? "TTL set to " + secs + "s" : "TTL removed");
        dbLoadRedisValue(d.redisKey);
      });
    }
    input.onkeydown = function (e) {
      e.stopPropagation();
      if (e.key === "Enter") { e.preventDefault(); apply(); }
      else if (e.key === "Escape") { e.preventDefault(); ran = true; dbLoadRedisValue(d.redisKey); }
    };
    input.onblur = apply;
  };
  return btn;
}

/** The typed table — the row grid's own vocabulary (inserts first, db-dirty cells, db-del
 *  struck rows, the narrow ✕/↩ control column), pointing at the redis buffer instead of the
 *  SQL edit buffer. */
function dbRedisTypedTable(wrap, v, cfg) {
  var d = state.db;
  var b = dbRedisEdits();
  if (!b) return;
  var tbl = el("table", "db-grid");
  var thead = el("thead");
  var hr = el("tr");
  hr.appendChild(el("th", "db-rowctl", ""));
  cfg.cols.forEach(function (c) {
    var th = el("th", "db-col", c);
    th.classList.add("db-rowctl"); // no sort affordance on a typed value table
    hr.appendChild(th);
  });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  var tbody = el("tbody");

  // Buffered inserts first, so they read as "the row you are about to add".
  b.inserts.forEach(function (ins, i) {
    var tr = el("tr", "db-ins");
    tr.appendChild(dbRedisRowCtl(function () {
      b.inserts.splice(i, 1);
      renderDbGrid();
      renderDbBar();
    }, false, false));
    cfg.cols.forEach(function (c) {
      var editable = cfg.ins.indexOf(c) >= 0;
      var filled = editable && ins[c] != null && String(ins[c]) !== "";
      var td = el("td", "db-cell db-cell-edit" + (filled ? " db-dirty" : ""));
      td.textContent = ins[c] == null ? "" : String(ins[c]);
      if (editable) {
        td.title = "Double-click to edit";
        td.ondblclick = function () {
          dbRedisCellEdit(td, i, "insert", c, ins[c] == null ? "" : String(ins[c]));
        };
      }
      tr.appendChild(td);
    });
    tbody.appendChild(tr);
  });

  dbRedisEntries(v, b).forEach(function (entry) {
    var tr = el("tr", entry.deleted ? "db-del" : "");
    tr.appendChild(dbRedisRowCtl(function () { dbRedisToggleDelete(entry.addr); }, cfg.deletable, entry.deleted));
    cfg.cols.forEach(function (c) {
      var editable = cfg.edit.indexOf(c) >= 0 && !entry.deleted;
      var td = el("td", "db-cell" + (entry.updated && cfg.edit.indexOf(c) >= 0 ? " db-dirty" : "") + (editable ? " db-cell-edit" : ""));
      td.textContent = entry.cells[c] == null ? "" : String(entry.cells[c]);
      td.title = td.textContent || ""; // long values truncate in the cell; the full text is one hover away
      if (editable) {
        td.ondblclick = function () {
          dbRedisCellEdit(td, 0, entry.addr, c, String(entry.cells[c]));
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
function dbRedisRowCtl(onClick, deletable, deleted) {
  var td = el("td", "db-rowctl");
  if (!deletable && !deleted) return td;
  var b2 = el("button", "db-act", deleted ? "↩" : "✕");
  b2.type = "button";
  b2.title = deleted ? "Undo this buffered delete" : "Buffer a delete — applied only on Commit";
  b2.onclick = onClick;
  td.appendChild(b2);
  return td;
}

function dbRedisToggleDelete(addr) {
  var b = dbRedisEdits();
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
var dbRedisEditor = null;

function dbRedisCellEdit(td, insertIdx, addr, col, current) {
  dbRedisEditorClose();
  var wrap = $("dbGridWrap");
  if (!wrap) return;
  var wrapRect = wrap.getBoundingClientRect();
  var rect = td.getBoundingClientRect();
  var ta = el("textarea", "db-inline-edit");
  ta.rows = 1;
  ta.spellcheck = false;
  ta.value = current;
  ta.style.left = (rect.left - wrapRect.left + wrap.scrollLeft) + "px";
  ta.style.top = (rect.top - wrapRect.top + wrap.scrollTop) + "px";
  ta.style.width = Math.max(rect.width + 24, 96) + "px";
  ta.style.minHeight = Math.max(rect.height, 22) + "px";
  wrap.appendChild(ta);
  dbRedisEditor = ta;
  function autoSize() {
    ta.style.height = "auto";
    ta.style.height = Math.min(Math.max(ta.scrollHeight, 22), 320) + "px";
  }
  function save() {
    var raw = ta.value;
    dbRedisEditorClose();
    var b = dbRedisEdits();
    if (!b) return;
    if (col === "score" && !dbRedisValidScore(raw)) {
      toast("A score is a number, or inf / -inf", true);
      renderDbGrid();
      return;
    }
    if (insertIdx >= 0) {
      var ins = b.inserts[insertIdx];
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
  ta.onkeydown = function (e) {
    e.stopPropagation();
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); save(); }
    else if (e.key === "Escape") { e.preventDefault(); dbRedisEditorClose(); renderDbGrid(); }
  };
  function dismiss(ev) {
    if (ev.target === ta || ta.contains(ev.target)) return;
    document.removeEventListener("mousedown", dismiss, true);
    save();
  }
  document.addEventListener("mousedown", dismiss, true);
  autoSize();
  ta.focus();
  ta.select();
}

function dbRedisEditorClose() {
  if (dbRedisEditor) { dbRedisEditor.remove(); dbRedisEditor = null; }
}

/** docs/22 W3.3: a string edits in place — one textarea, one Set, one SET through the
 *  pipeline (a value keeps its spaces: pipeline args bind as separate strings, which the
 *  whitespace-splitting /command console can never promise). */
function dbRedisStringEditor(wrap, v) {
  var d = state.db;
  var row = el("div", "db-redis-str-row");
  var ta = el("textarea", "db-redis-str");
  ta.rows = 3;
  ta.spellcheck = false;
  ta.value = typeof v.value === "string" ? v.value : "";
  var set = el("button", "btn primary", "Set");
  set.type = "button";
  async function go() {
    var value = ta.value;
    if (value === ta.dataset.orig) { toast("Unchanged"); return; }
    var j = await apiJson("/api/db/" + encodeURIComponent(d.conn) + "/redis-pipeline", {
      method: "POST",
      body: JSON.stringify({ commands: [["SET", d.redisKey, value]] }),
    });
    if (!j) return;
    toast("Set " + d.redisKey);
    dbLoadRedisValue(d.redisKey);
  }
  set.onclick = function () { void go(); };
  ta.onkeydown = function (e) {
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
async function dbRedisCommit() {
  var d = state.db;
  var b = dbRedisEdits();
  if (!b) return;
  var cmds;
  try {
    cmds = dbRedisCommands(d.redisKey, b.type, b);
  } catch (err) {
    toast(String(err && err.message ? err.message : err), true);
    return;
  }
  if (!cmds.length) return;
  if (!confirm("Commit " + cmds.length + " command" + (cmds.length > 1 ? "s" : "") + " to " + d.redisKey + "?\n" +
      "One pipelined round trip, every command guard-checked. Redis has no transaction here — a failed command stops the run.")) return;
  var j = await apiJson("/api/db/" + encodeURIComponent(d.conn) + "/redis-pipeline", {
    method: "POST",
    body: JSON.stringify({ commands: cmds.map(function (c) { return [c.verb].concat(c.args); }) }),
  });
  if (!j) return;
  toast("Committed " + cmds.length + " command" + (cmds.length > 1 ? "s" : ""));
  d.redisEdits = null;
  d.sqlPreview = false;
  dbLoadRedisValue(d.redisKey);
  // Deleting a hash's last field (or a set's last member) deletes the KEY — refresh the list
  // so it does not offer a key that is gone.
  if (d.redisValue && d.redisValue.type === "none") dbLoadKeys(true);
}

/** Drop the buffer without a single command — the twin of the row grid's Discard. */
function dbRedisDiscard() {
  var d = state.db;
  var n = dbRedisPendingCount();
  if (!n) return;
  if (!confirm("Discard " + n + " buffered change" + (n > 1 ? "s" : "") + "? Nothing has been written to redis.")) return;
  d.redisEdits = null;
  d.sqlPreview = false;
  renderDbGrid();
  renderDbBar();
}

function dbRedisKeyMenu(anchorEl) {
  popupMenu(anchorEl.getBoundingClientRect(), [
    { label: "Rename\u2026", fn: function () { dbRedisRenameSheet(state.db.redisKey); } },
    { sep: true },
    { label: "Delete\u2026", danger: true, fn: dbRedisDeleteKey },
  ]);
}

/** One guarded console command; null means the toast already said why. */
async function dbRedisCommand(line) {
  return apiJson("/api/db/" + encodeURIComponent(state.db.conn) + "/command", {
    method: "POST",
    body: JSON.stringify({ command: line }),
  });
}

/** The one-input sheet Rename uses (docs/22 W1.3). `submit(value)` runs after the sheet
 *  closes; it owns its own reload and error handling. */
function dbRedisKeySheet(cfg) {
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
  var input = $("dbKeyIn");
  input.value = cfg.value || "";
  input.placeholder = cfg.placeholder || "";
  $("dbKeyCancel").onclick = closeSheet;
  $("sheet").onclick = function (e) { if (e.target === $("sheet")) closeSheet(); };
  function go() { var v = input.value.trim(); closeSheet(); cfg.submit(v); }
  $("dbKeyGo").onclick = go;
  input.onkeydown = function (e) {
    if (e.key === "Enter") { e.preventDefault(); go(); }
  };
  input.focus();
  input.select();
}

function dbRedisRenameSheet(key) {
  dbRedisKeySheet({
    title: "Rename key",
    label: "New name",
    value: key,
    primary: "Rename",
    submit: async function (to) {
      var d = state.db;
      if (!to || to === key) return;
      var j = await dbRedisCommand("RENAME " + key + " " + to);
      if (!j) return;
      toast("Renamed to " + to);
      d.redisKey = to;
      await dbLoadKeys(true);
      dbLoadRedisValue(to);
    },
  });
}

/* Deleting a key is the one destructive act the redis side has — the same typed-name confirm
   the table DROP/TRUNCATE use (a W1 audit follow-up), with the key's own words. */
function dbRedisDeleteKey() {
  var d = state.db;
  var key = d.redisKey;
  dbTypedConfirm({ what: "DELETE (permanently)", name: key, kind: "key" }, function () {
    void dbRedisCommand("DEL " + key).then(async function (j) {
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
