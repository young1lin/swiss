import { apiJson, el, emptyHtml, state, toast } from "./util.js";
import { renderDbFilters } from "./data-filters.js";
import { renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { renderDbTables } from "./data-view.js";

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
  wrap.appendChild(el("div", "db-detail-meta",
    v.key + " · " + v.type + " · TTL " + (v.ttl < 0 ? "none" : v.ttl + "s") +
    (v.length != null ? " · " + v.length + " entries" : "") +
    (v.truncated ? " · truncated" : "")));
  var pre = el("pre", "db-ddl");
  pre.style.position = "static";
  pre.style.margin = "var(--s2)";
  pre.textContent = typeof v.value === "string" ? v.value : JSON.stringify(v.value, null, 2);
  wrap.appendChild(pre);
}

export { dbIsRedis, dbLoadKeys, dbLoadRedisValue, dbRenderRedisValue };
