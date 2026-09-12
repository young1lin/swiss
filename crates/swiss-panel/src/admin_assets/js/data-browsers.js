import { $, apiJson, el, emptyHtml, esc, icon, state, toast } from "./util.js";
import { closeSheet } from "./add-sheet.js";
import { renderDbFilters } from "./data-filters.js";
import { renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { renderDbTables } from "./data-view.js";
import { popupMenu } from "./menu.js";

/* --- redis key browser -------------------------------------------------------------------------- */
/* A redis connection in the picker swaps the table list for a SCAN-paged key list, and the
   right pane for a type-aware value view. Read-only: writes belong to redis_command. */

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
  renderDbGrid();
  var j = await apiJson("/api/db/" + encodeURIComponent(d.conn) + "/key?key=" + encodeURIComponent(key));
  if (!j) { d.redisKey = null; renderDbGrid(); return; }
  d.redisValue = j;
  renderDbGrid();
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
  var meta = el("div", "db-detail-meta",
    v.key + " · " + v.type + " · TTL " + (v.ttl < 0 ? "none" : v.ttl + "s") +
    (v.length != null ? " · " + v.length + " entries" : "") +
    (v.truncated ? " · truncated" : ""));
  // docs/22 W1.3: the key's own actions ride the value header. Rename and TTL take a one-input
  // sheet, Delete is last and red and demands the key name typed back — the same confirm
  // vocabulary as the table DDL ops. Everything runs through the guarded /command console.
  var more = el("button", "btn icon");
  more.type = "button";
  more.innerHTML = icon("ellipsis");
  more.title = "Rename, set TTL or delete this key";
  more.onclick = function () { dbRedisKeyMenu(more); };
  meta.appendChild(more);
  wrap.appendChild(meta);
  var pre = el("pre", "db-ddl");
  pre.style.position = "static";
  pre.style.margin = "var(--s2)";
  pre.textContent = typeof v.value === "string" ? v.value : JSON.stringify(v.value, null, 2);
  wrap.appendChild(pre);
}

function dbRedisKeyMenu(anchorEl) {
  var d = state.db;
  popupMenu(anchorEl.getBoundingClientRect(), [
    { label: "Rename\u2026", fn: function () { dbRedisRenameSheet(d.redisKey); } },
    { label: "Set TTL\u2026", fn: function () { dbRedisTtlSheet(d.redisKey); } },
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

/** The one-input sheet Rename and Set TTL share (docs/22 W1.3). `submit(value)` runs after the
 *  sheet closes; it owns its own reload and error handling. */
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

function dbRedisTtlSheet(key) {
  dbRedisKeySheet({
    title: "Set TTL",
    label: "Seconds (empty removes the TTL)",
    value: "",
    placeholder: "e.g. 3600",
    primary: "Set TTL",
    submit: async function (secs) {
      if (secs && !/^\d+$/.test(secs)) { toast("TTL must be a whole number of seconds", true); return; }
      var line = secs ? "EXPIRE " + key + " " + secs : "PERSIST " + key;
      var j = await dbRedisCommand(line);
      if (!j) return;
      toast(secs ? "TTL set to " + secs + "s" : "TTL removed");
      dbLoadRedisValue(key);
    },
  });
}

function dbRedisDeleteKey() {
  var d = state.db;
  var key = d.redisKey;
  var typed = prompt("DELETE (permanently) " + key + "\n" +
    "This cannot be undone. Type the key name to confirm:", "");
  if (typed !== key) { if (typed !== null) toast("Name did not match — nothing was done", true); return; }
  void dbRedisCommand("DEL " + key).then(async function (j) {
    if (!j) return;
    toast("Deleted " + key);
    d.redisKey = null;
    d.redisValue = null;
    await dbLoadKeys(true);
    renderDbGrid();
  });
}

export { dbIsRedis, dbLoadKeys, dbLoadRedisValue, dbRenderRedisValue };
