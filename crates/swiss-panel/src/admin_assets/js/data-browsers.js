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
import { h } from "./h.js";
import { renderDbFilters } from "./data-filters.js";
import { renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { renderDbBar } from "./data-sql.js";
import { dbKeyShown } from "./data-tree.js";
// Cycle with data-edit.js (it reads dbIsRedis/dbLoadKeys from here): function declarations,
// runtime-only use — the same shape as the data-sql import above.
// Cycle with data-stream.js (it renders this module's stream branch and needs the display
// decode + cell menu): function declarations, runtime-only use — the same shape as the
// data-edit.js import above.
import { dbRenderStream } from "./data-stream.js";
import { dbCopyText } from "./data-csv.js";
import { dbOpenValueSheet } from "./data-value.js";
import { dbLoadDatabases, renderDbTables } from "./data-view.js";
import { dbTabGlyph, renderDbTabs } from "./data-tabs.js";
import { dbConn, dbTab } from "./db-state.js";
import { tk, tr, trn } from "./i18n.js";
import { btn } from "./ui/button.js";
import { popupMenu } from "./ui/menu.js";
import { openFieldSheet } from "./ui/sheet.js";

/* --- redis key browser -------------------------------------------------------------------------- */
/* A redis connection in the picker swaps the table list for a SCAN-paged key list, and the
   right pane for a type-aware value view (SPEC §data.redis): a string edits in place (one SET)
   and the container types render as typed tables whose edits buffer locally and Commit as
   ONE guarded pipeline round trip — HSET/HDEL for hashes, ZADD/ZREM for zsets, LSET/RPUSH
   for lists, SADD/SREM for sets. Every buffered command is field-addressed, so no MULTI: a
   refused or failed command stops the run and the operator re-commits the rest. */

function dbIsRedis()          {
  const d = dbConn();
  const c = d.conns.find((x                    )          => { return x.name === d.conn; });
  return !!c && c.dialect === "redis";
}

// One SCAN page in flight (SPEC §data): More re-entering while a page loads
// re-sent the SAME cursor and appended that page twice. A reset (grep, type filter, refresh)
// still goes through — it restarts the walk from cursor 0 and supersedes.
let dbKeysLoading = false;

// One /keys request chain (SPEC §data): a slow More answer landing after a reset
// spliced its old-cursor page into the FRESH walk and dragged the cursor backward, so later
// pages shifted under the user (a key quietly went missing from the list). Responses apply
// only while their token is still the newest request.
const dbKeysReq = dbReqGuard();

/** A keyspace listing with each key once, first sighting kept. SCAN promises every key AT
 *  LEAST once: a rehash in the middle of a walk hands some back again, and a repeated key was
 *  a repeated row in the tree, a repeated completion and a repeated DEL argument. Pure. */
function dbUniqueKeys(rows                    )                     {
  const seen = new Set        ();
  return rows.filter((k                  )          => {
    if (seen.has(k.key)) return false;
    seen.add(k.key);
    return true;
  });
}

async function dbLoadKeys(reset                     )                {
  const d = dbConn();
  if (!d.conn) return;
  if (!reset && dbKeysLoading) return; // the More double-click: the page is already on its way
  if (reset) {
    d.redis = null;
    const t0 = dbTab();
    if (t0.kind === "key") t0.redisKey = null; // a reset walk deselects the open key
  }
  let q = dbKeysQuery(d);
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
    // SPEC §data: apiJson already toasted the server's own text, but a transient
    // toast over a list that still says "no keys" reads as an empty keyspace. Mark the
    // failure so the list paints it (renderDbTables), keeping the last good page.
    d.redisError = true;
    renderDbTables();
    return;
  }
  d.redisError = false;
  d.redis = {
    keys: dbUniqueKeys((d.redis && !reset ? d.redis?.keys : []).concat(j.keys || [])),
    cursor: j.cursor,
    done: !!j.done,
    total: j.total,
    pages: (d.redis && !reset ? d.redis.pages || 1 : 0) + 1,
  };
  renderDbTables();
  renderDbFilters(); // the shown/keyspace readout rides on the pattern bar
}

/** The first-page /keys request of the walk the record describes - its pattern and type
 *  filter, no cursor. */
function dbKeysQuery(d             )         {
  let q = "/api/db/" + encodeURIComponent(d.conn ) + "/keys?count=200";
  // The redis SCAN MATCH is exact-shape, while the SQL side greps as a substring - a bare
  // word finding every table but no key read as "the key does not exist". Wrap the grep as
  // a substring unless the user typed their own wildcard; a walk of SPEC §data.tabs caught this live.
  if (d.grep) {
    const pat = d.grep.includes("*") ? d.grep : "*" + d.grep + "*";
    q += "&pattern=" + encodeURIComponent(pat);
  }
  if (d.redisType) q += "&type=" + encodeURIComponent(d.redisType);
  return q;
}

/** SPEC §data.sessions (revised): walk the key list again QUIETLY - from cursor 0, as many pages deep
 *  as the walk on screen went, so the pages More fetched are re-read rather than lost. Nothing
 *  is cleared, the open key stays open, and the tree repaints only when the answer moved. A
 *  key list that is never re-walked freezes at its first answer: "SET test 1" on an empty
 *  Redis never showed up, whatever the operator clicked. */
async function dbRewalkKeys()                {
  const d = dbConn();
  const was = d.redis;
  if (!d.conn) return;
  if (!was) { void dbLoadKeys(true); return; } // nothing walked yet: the ordinary first walk
  if (dbKeysLoading) return; // a walk is already on its way; its answer is the fresh one
  const q = dbKeysQuery(d);
  const depth = was.pages || 1;
  const token = dbKeysReq.issue();
  let keys                     = [];
  let pages = 0;
  let j                                = null;
  dbKeysLoading = true;
  try {
    do {
      const at         = j && j.cursor ? "&cursor=" + encodeURIComponent(j.cursor) : "";
      j = await apiJson                        (q + at);
      // A failure keeps the walk on screen (apiJson has toasted); a newer walk owns the list.
      if (!j || !dbKeysReq.accepts(token)) return;
      keys = keys.concat(j.keys || []);
      pages++;
    } while (!j.done && pages < depth);
  } finally {
    dbKeysLoading = false;
  }
  const next = { keys: dbUniqueKeys(keys), cursor: j.cursor, done: !!j.done, total: j.total, pages: pages };
  if (!d.redisError && JSON.stringify(next) === JSON.stringify(was)) return;
  d.redisError = false;
  d.redis = next;
  // The session may have parked while the walk was out: the answer is still its keyspace, but
  // the tree on screen is another connection's.
  if (dbConn() !== d) return;
  renderDbTables();
  renderDbFilters();
}

/** A redis connection's sidebar, re-read quietly: the key walk and the database catalog
 *  (its per-database key counts). The refresh, a return to the page and a console command. */
function dbRefreshKeyspace()       {
  void dbRewalkKeys();
  void dbLoadDatabases(true);
}

// One /key request chain: a slow answer for the key the user just left must be dropped,
// or it would flash the PREVIOUS key's value into the pane (SPEC §data).
const dbValueReq = dbReqGuard();

async function dbLoadRedisValue(key        )                {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "key") return; // the value view belongs to a key tab (SPEC §data.tabs)
  // No result to clear any more: a command reply lives on the console's own tab, so opening a
  // key can no longer be standing on top of one (SPEC §data.tabs retired the shared overlay).
  d.redisKey = key;
  d.redisValue = null; // drop the previous key's value — never flash stale data
  d.redisEdits = null; // and its buffered edits — a different key cannot adopt them
  d.redisStreamRows = null; // SPEC §data.streams: the grown stream cache belongs to ONE window — a
  d.redisStreamMore = null; // fresh newest window is the truth; history re-walks from it
  renderDbGrid();
  const token = dbValueReq.issue();
  const j = await apiJson                 ("/api/db/" + encodeURIComponent(c.conn ) + "/key?key=" + encodeURIComponent(key));
  if (!dbValueReq.accepts(token)) return; // superseded: a newer key owns the pane
  if (!j) { d.redisKey = null; renderDbGrid(); return; }
  const glyph = dbTabGlyph(d);
  d.redisValue = j;
  d.redisValueAt = Date.now(); // the countdown's origin: this read, not the next repaint
  // SPEC §data.tabs: the toolbar's primary action follows the value's TYPE — it paints only when
  // the value is here, so the toolbar repainted with the grid (the status line rides along).
  renderDbToolbar();
  renderDbGrid();
  // The card wears the type's glyph: repaint the strip when the landed type is not the one
  // it was painted with (opened without a row, or the key was recreated as another type).
  if (dbTabGlyph(d) !== glyph) renderDbTabs();
}

/** SPEC §data.sessions: a resumed key tab re-reads its value WITHOUT blanking it first, and repaints only
 *  when the value moved. A tab holding buffered edits is left alone - they are keyed to what is
 *  on screen. */
async function dbRefreshRedisValue(key        )                {
  const conn = dbConn().conn;
  const d = dbTab();
  if (!conn || d.kind !== "key" || d.redisKey !== key || !d.redisValue || d.redisEdits) return;
  const token = dbValueReq.issue();
  const j = await apiJson                 ("/api/db/" + encodeURIComponent(conn) + "/key?key=" + encodeURIComponent(key));
  if (!dbValueReq.accepts(token) || !j) return;
  // Re-read the front AFTER the wait: the operator may have moved on, or started editing.
  const now = dbTab();
  if (now !== d || now.kind !== "key" || now.redisKey !== key || now.redisEdits) return;
  if (JSON.stringify(j) === JSON.stringify(now.redisValue)) return;
  const glyph = dbTabGlyph(now);
  now.redisValue = j;
  now.redisValueAt = Date.now();
  now.redisStreamRows = null; // the stream caches belong to the value they grew from
  now.redisStreamMore = null;
  renderDbToolbar();
  renderDbGrid();
  if (dbTabGlyph(now) !== glyph) renderDbTabs();
}

/* --- the typed value view (SPEC §data.redis) --------------------------------------------------------- */

/* The plan for one container type. `cols` names the columns, `edit` the cells a double-click
   edits on an existing row, `ins` the cells a buffered insert carries, `thing` the word the
   add button and the refusals use. `deletable` is false only for list: redis has no command
   that removes one item by index without rewriting the tail, so the buffer offers nothing it
   cannot honor (dbgate's list changeset stops at LSET/RPUSH for the same reason).
   `add` holds a tk()-marked key (SPEC §panel.i18n): the button paints it through tr() at render
   time, so a language flip is honored — a module-eval tr() would freeze English.
   `cols` and `thing` themselves stay WIRE words (cell addressing compares them), so their
   display forms go through the tk()-marked label maps below at paint time. */
/* Display labels for the wire vocabulary above (SPEC §panel.i18n tk idiom): the th heads and the
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
 *  then args with STRINGS double-quoted and numbers plain. Quotes AND backslashes are escaped,
 *  so the line reads back as the same bytes wherever it is pasted - the console included: a
 *  value ending in `\` used to show as `"x\"`, an unclosed quote there. Pure. */
function dbRedisCommandText(cmd                                   )         {
  return cmd.verb + " " + cmd.args.map((a         )         => {
    return typeof a === "number" ? String(a) : '"' + String(a).replace(/[\\"]/g, "\\$&") + '"';
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
    // The shared empty state (SPEC §panel.design) — a non-key tab on a redis connection is the
    // "no key opened yet" state.
    wrap.appendChild(emptyNode({ icon: "database", title: tr("dataBrowsers.selectKey"), hint: tr("dataBrowsers.pickKeyLeftView") }));
    return;
  }
  if (!d.redisKey) {
    // The shared empty state (SPEC §panel.design).
    wrap.appendChild(emptyNode({ icon: "database", title: tr("dataBrowsers.selectKey"), hint: tr("dataBrowsers.pickKeyLeftView") }));
    return;
  }
  const v = d.redisValue;
  if (!v) { wrap.appendChild(el("div", "db-hint", tr("dataBrowsers.loadingK", { k: dbKeyShown(d.redisKey) }))); return; }
  const meta = el("div", "db-detail-meta");
  // The TTL is NOT here any more (2026-09-28): one live countdown, in the head's top-right.
  meta.appendChild(document.createTextNode(dbKeyShown(v.key) + " · " + v.type));
  if (v.length != null) meta.appendChild(document.createTextNode(" · " + trn(v.length, "dataBrowsers.nEntries.one", "dataBrowsers.nEntries.other")));
  if (v.truncated) meta.appendChild(document.createTextNode(" · " + tr("dataBrowsers.truncated")));
  // The facts only. The key's add action and its ⋯ (Rename / Delete, SPEC §data.redis) are the
  // head's since SPEC §data.tabs (renderDbToolbar: one primary + one overflow). This line kept a
  // second copy of both until SPEC §panel.pages, and its "+ Field" had lost its data-radd address, so
  // a real click on it did nothing.
  wrap.appendChild(meta);
  const cfg = DB_REDIS_TYPES[v.type];
  if (v.type === "none") {
    wrap.appendChild(el("div", "db-hint", tr("dataBrowsers.keyFoundMayExpired")));
    return;
  }
  if (v.note) wrap.appendChild(el("div", "db-hint", v.note));
  if (cfg) { dbRedisTypedTable(wrap, v, cfg); return; }
  if (v.type === "string") { dbRedisStringEditor(wrap, v); return; }
  if (v.type === "stream") { dbRenderStream(wrap, v); return; } // SPEC §data.streams: the stream window view
  // Module types stay read-only — their commands have no field grid (SPEC §data.redis).
  // A stream key never reaches this line: it dispatches above into its own window view
  // (SPEC §data.streams), which is read-only too — browsing a stream never mutates it.
  const pre = el("pre", "db-ddl");
  pre.style.position = "static";
  pre.style.margin = "var(--s2)";
  // The value is data, not copy; the whole conditional is hoisted to a const because the
  // i18n gate's ternary scan counts the comparison literal in the condition as bare copy.
  const valueText = typeof v.value === "string" ? v.value : JSON.stringify(v.value, null, 2);
  pre.textContent = valueText;
  pre.oncontextmenu = (ev            )       => {
    dbRedisCellMenu(ev, v.type, valueText, dbKeyShown(v.key));
  };
  wrap.appendChild(pre);
}

/* --- the TTL countdown (2026-09-28) ------------------------------------------------------------
   The owner: "实时（获取一次时间就行了，页面上计算）显示这个 Key 剩余时间是多少". The server is asked
   ONCE — the TTL rides with the value — and the page does the arithmetic from there: the readout
   carries the moment it expires, and ONE shared second-ticker rewrites whatever readouts are on
   screen. Every TTL on the page used to be the number the last fetch happened to see, printed in
   three places (the sidebar row, the detail meta line, the status bar) and frozen there until
   something else caused a poll — a key that said 95s said it a minute later too. Now one readout
   lives, in the head's top-right corner beside the key's ⋯, and the others say nothing. */

let dbTtlTick                                        = null;

/** Seconds left, as a clock once it is a minute or more: 45s · 1:31 · 2:05:00. Pure. */
export function dbTtlLabel(secs        )         {
  if (secs <= 0) return tr("dataBrowsers.ttlExpired");
  if (secs < 60) return tr("dataBrowsers.ttlSeconds", { n: secs });
  const two = (n        )         => { return n < 10 ? "0" + String(n) : String(n); };
  const h3 = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const tail = two(m) + ":" + two(secs % 60);
  return h3 > 0 ? String(h3) + ":" + tail : String(m) + ":" + two(secs % 60);
}

/** Rewrite every countdown on screen, and stop the ticker once none is left — a key tab that
 *  closed must not leave a timer running for the rest of the session. */
function dbTtlPaint()       {
  const now = Date.now();
  const live = document.querySelectorAll             ("[data-ttlend]");
  live.forEach((e             )       => {
    e.textContent = dbTtlLabel(Math.ceil((Number(e.dataset.ttlend) - now) / 1000));
  });
  if (!live.length && dbTtlTick != null) { clearInterval(dbTtlTick); dbTtlTick = null; }
}

/** The TTL readout is itself the control (SPEC §data.redis): click to edit in place, Enter runs
 *  EXPIRE — or PERSIST when emptied — through the guarded console, then the key re-reads.
 *  The button carries data-rttl; #pane's delegated click (dbRedisClick) builds the in-place
 *  input from the LIVE value, so a poll that moved the TTL between render and click seeds
 *  the editor with what the key says now (SPEC §panel.toolchain).
 *  `readAt` is when the value was read — the countdown's origin, so a repaint that carries no
 *  new read (the toolbar redraws for its own reasons) resumes the count instead of restarting
 *  it at the number the fetch saw. */
export function dbRedisTtl(v                 , readAt        )              {
  const title = tr("dataBrowsers.changeTtlEnterApplies");
  if (v.ttl == null || v.ttl < 0) {
    return h("button", { class: "db-ttl", type: "button", title, data: { rttl: "" } }, tr("dataBrowsers.noExpiry"));
  }
  // A read cannot be in the future: a clock that stepped backwards between the read and this
  // paint would otherwise show more life than the key was ever given.
  const ends = Math.min(readAt, Date.now()) + v.ttl * 1000;
  // The repaint owns the clock: one ticker, re-armed by whichever render last put a countdown
  // on screen, so the seconds start at THIS render's boundary and two countdowns never mean
  // two timers. dbTtlPaint stops it once nothing is left to count.
  if (dbTtlTick != null) clearInterval(dbTtlTick);
  dbTtlTick = setInterval(dbTtlPaint, 1000);
  return h("button", {
    class: "db-ttl", type: "button", title, data: { rttl: "", ttlend: String(ends) },
  }, dbTtlLabel(Math.ceil((ends - Date.now()) / 1000)));
}

/** The typed table — the row grid's own vocabulary (inserts first, db-dirty cells, db-del
 *  struck rows, the narrow remove/undo control column), pointing at the redis buffer instead of the
 *  SQL edit buffer. */
function dbRedisTypedTable(wrap             , v                 , cfg                )       {
  const b = dbRedisEdits();
  if (!b) return;
  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  hr.appendChild(el("th", "db-rowctl", ""));
  cfg.cols.forEach((c        )       => {
    // db-nosort: a typed value table does not sort. It wore db-rowctl for that until SPEC §panel.pages,
    // which also handed every column the control column's width and stretched the control
    // column to a third of the table.
    hr.appendChild(el("th", "db-col db-nosort", tr(REDIS_COL_KEYS[c] ?? c)));
  });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");

  // Buffered inserts first, so they read as "the row you are about to add". The row control
  // carries its address (data-rins / data-raddr) — #pane's delegated click owns the behavior
  // (SPEC §panel.toolchain), resolving the buffer live at event time.
  b.inserts.forEach((ins                         , i        )       => {
    const tri = el("tr", "db-ins");
    tri.appendChild(dbRedisRowCtl(false, false, null, i));
    cfg.cols.forEach((c        )       => {
      const editable = cfg.ins.includes(c);
      const filled = editable && ins[c] != null && String(ins[c]) !== "";
      const td = el("td", "db-cell db-cell-edit" + (filled ? " db-dirty" : ""));
      td.textContent = ins[c] == null ? "" : dbRedisDisplayText(String(ins[c]));
      if (editable) {
        td.title = tr("dataBrowsers.doubleClickEdit");
        td.ondblclick = ()       => {
          dbRedisCellEdit(td, i, "insert", c, ins[c] == null ? "" : String(ins[c]));
        };
      }
      td.oncontextmenu = (ev            )       => {
        dbRedisCellMenu(ev, tr(REDIS_COL_KEYS[c] ?? c), td.textContent || "", dbKeyShown(v.key) + " · " + v.type);
      };
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
      td.textContent = entry.cells[c] == null ? "" : dbRedisDisplayText(String(entry.cells[c]));
      td.title = td.textContent || ""; // long values truncate in the cell; the full text is one hover away
      if (editable) {
        td.ondblclick = ()       => {
          // -1, never an insert index: save() reads >= 0 as "this is a buffered insert row",
        // and a 0 here once made every existing-row edit vanish into inserts[0] (caught live).
        dbRedisCellEdit(td, -1, entry.addr, c, String(entry.cells[c]));
        };
      }
      td.oncontextmenu = (ev            )       => {
        dbRedisCellMenu(ev, tr(REDIS_COL_KEYS[c] ?? c), td.textContent || "", dbKeyShown(v.key) + " · " + v.type);
      };
      tri.appendChild(td);
    });
    tbody.appendChild(tri);
  });

  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
}

/** Decode a redis value for DISPLAY (SPEC §data.tabs): upstream Java services persist
 *  their strings JSON-encoded, and their serializers escape astral characters as literal
 *  surrogate-pair text — a title arrives from the wire as
 *  "\"\uD83D\uDCC8\u23EB\uD83D\uDC46{0} rose {2} within {1} hr\"" and the panel used to
 *  show those twelve backslash-u characters instead of the chart emoji. Two layers, both
 *  display-only: a whole JSON string literal unwraps (JSON.parse handles its own escapes),
 *  then stray well-formed surrogate PAIRS fold to the character they name. A lone surrogate
 *  escape and any \u that is not a pair are left untouched — a Windows path like c:\users
 *  must survive byte-identical. Editing still seeds the ORIGINAL bytes, so a saved value
 *  changes only when the user actually edits it. Pure. */
function dbRedisDisplayText(raw        )         {
  let s = raw;
  if (s.length >= 2 && s.startsWith(String.fromCharCode(34)) && s.endsWith(String.fromCharCode(34))) {
    try {
      const parsed          = JSON.parse(s);
      if (typeof parsed === "string") s = parsed;
    } catch { /* not a JSON literal — show it as stored */ }
  }
  const pair = /\\u([dD][89abAB][0-9a-fA-F]{2})\\u([dD][cdefCDEF][0-9a-fA-F]{2})/g;
  if (pair.test(s)) {
    s = s.replace(pair, (_m        , hi        , lo        )         => {
      return String.fromCharCode(parseInt(hi, 16), parseInt(lo, 16));
    });
  }
  return s;
}

/** The typed table's right-click — the SPEC §data.grid cell-menu vocabulary on the redis
 *  side. A zset member is regularly a long JSON blob the cell truncates to ellipsis; before
 *  this menu the only way to see one whole was the title hover, and stream values (a plain
 *  read-only pre) had nothing at all. Copy and View use the same words the SQL grid's cell
 *  menu uses, and the viewer is the same sheet (text / JSON tree / hex). */
function dbRedisCellMenu(e            , colLabel        , text        , where        )       {
  e.preventDefault();
  popupMenu({ left: e.clientX, top: e.clientY, bottom: e.clientY }, [
    { label: tr("logs.copyValue"), fn: ()       => { dbCopyText(text); } },
    { label: tr("dataCsv.viewValue"), fn: ()       => { dbOpenValueSheet(colLabel, text, where); } },
  ]);
}

/** The narrow remove/undo column the row grid uses, in the value view's words: remove
 *  buffers a delete (HDEL / ZREM / SREM on Commit) or removes a buffered insert, undo
 *  undoes a delete (SPEC §panel.design: the glyphs are i-x / i-undo now). A stored entry of a type
 *  with no honest delete command (list) gets no control at all; a buffered insert always has
 *  one, because dropping it is local (the SQL grid's data-irm). The insert rows passed "not
 *  deletable" until SPEC §panel.pages and drew an empty cell, so Discard-all was their only way back.
 *  The button's address is data-raddr (a stored entry) or data-rins (a buffered insert's index). */
function dbRedisRowCtl(deletable         , deleted         , addr               , insIdx        )              {
  const td = el("td", "db-rowctl");
  if (addr == null) {
    td.appendChild(h("button", {
      class: "db-act", type: "button", title: tr("dataGrid.removeBufferedInsert"), data: { rins: String(insIdx) },
    }, iconNode("x")));
    return td;
  }
  if (!deletable && !deleted) return td;
  td.appendChild(h("button", {
    class: "db-act", type: "button",
    title: tr(deleted ? "dataBrowsers.undoBufferedDelete" : "dataBrowsers.bufferDeleteCommit"),
    data: { raddr: addr },
  }, iconNode(deleted ? "undo" : "x")));
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
   the cancelled text came back as a buffered change Commit would have written (SPEC §data). */
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
 *  time (SPEC §panel.toolchain): the value read is whatever the textarea holds when the click lands. */
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
  toast(tr("dataBrowsers.setKey", { key: dbKeyShown(d.redisKey ?? "") }));
  void dbLoadRedisValue(d.redisKey);
}

/** SPEC §data.redis: a string edits in place — one textarea, one Set, one SET through the
 *  pipeline (a value keeps its spaces: pipeline args bind as separate strings, which the
 *  whitespace-splitting /command console can never promise). The textarea carries
 *  data-rstr and the button data-rset; #pane's delegated click/keydown answer both. */
function dbRedisStringEditor(wrap             , v                 )       {
  const row = el("div", "db-redis-str-row");
  const ta = el("textarea", "db-redis-str")                       ;
  ta.rows = 3;
  ta.spellcheck = false;
  // Display-decoded (dbRedisDisplayText): the editor opens showing the characters the
  // value names, and the unchanged-check compares against the same decoded text, so merely
  // opening and saving a JSON-encoded title is a no-op, not a rewrite.
  ta.value = typeof v.value === "string" ? dbRedisDisplayText(v.value) : "";
  ta.dataset.orig = ta.value;
  ta.setAttribute("data-rstr", "");
  row.appendChild(ta);
  row.appendChild(btn(tr("dataBrowsers.set"), { kind: "primary", data: { rset: "" } }));
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
  if (!confirm(tr("dataBrowsers.commitNKOne", { n: trn(cmds.length, "dataBrowsers.nCommands.one", "dataBrowsers.nCommands.other"), k: dbKeyShown(d.redisKey ?? "") }))) return;
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
  // the list refresh never ran, so the sidebar kept offering a key that is gone (SPEC §data).
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

/** The key the open tab is on, or null - every act below asks the live tab, never a captured
 *  one (SPEC §panel.toolchain: a menu built at click time still fires after a tab change). */
function dbOpenKey()                {
  const t = dbTab();
  return t.kind === "key" ? t.redisKey : null;
}

function dbRedisRenameKey()       {
  dbRedisRenameSheet(dbOpenKey());
}

/** Set or lift the key's expiry (SPEC §data.redis) on the same one-field sheet the rename uses:
 *  seconds applies EXPIRE, an empty field PERSIST. The TTL readout in the meta line edits in
 *  place too - this is the door that says so. */
function dbRedisTtlSheet()       {
  const t = dbTab();
  if (t.kind !== "key") return;
  const key = t.redisKey;
  const v = t.redisValue;
  if (!key) return;
  openFieldSheet({
    title: tr("dataBrowsers.setTtl"),
    sub: key,
    label: tr("dataBrowsers.secondsEmptyPersists"),
    def: v && v.ttl != null && v.ttl >= 0 ? String(v.ttl) : "",
    allowEmpty: true,
    save: tr("dataBrowsers.apply"),
    submit: async (secs        )                            => {
      if (secs && !/^\d+$/.test(secs)) return tr("dataBrowsers.ttlMustWholeNumber");
      // EXPIRE key 0 deletes the key on the spot - a delete that skipped the delete's own
      // question. Zero is refused here and pointed at Delete.
      if (secs && Number(secs) === 0) return tr("dataBrowsers.ttlZeroDeletes");
      const j = await dbRedisExec(secs ? ["EXPIRE", key, secs] : ["PERSIST", key]);
      if (!j) return false;
      dbRedisTtlSaid(key, secs, j.reply);
      await dbLoadRedisValue(key);
      return true;
    },
  });
}

/** What an EXPIRE or PERSIST answered, said: EXPIRE's 0 is a key that is no longer there
 *  (another client deleted it, or it expired while the field was open) - "TTL set" would be
 *  a claim about a key that does not exist. PERSIST's 0 is also "had no TTL", so it stays
 *  "removed". */
function dbRedisTtlSaid(key        , secs        , reply         )       {
  if (secs && Number(reply) === 0) toast(tr("dataBrowsers.keyAlreadyGone", { key: dbKeyShown(key) }), true);
  else toast(tr(secs ? "dataBrowsers.ttlSetSeconds" : "dataBrowsers.ttlRemoved", { s: secs }));
}

/** One redis command as its exact arguments, through the structured pipeline route: every
 *  byte of a key is an argument byte. The console route splits a LINE the way a shell does
 *  (5be034a) - right for what a person types, wrong for a key the panel already holds:
 *  `"DEL " + key` on a key named `my key` deleted the keys `my` and `key`, and a backslash in
 *  a name deleted a different key (2026-09-29). Same guards as the console (vet_pipeline).
 *  The reply comes back wrapped, so a nil reply is not mistaken for a refusal; null means
 *  the route refused and apiJson has toasted why. */
async function dbRedisExec(argv          )                                     {
  const conn = dbConn().conn;
  if (!conn) return null;
  const j = await apiJson                         ("/api/db/" + encodeURIComponent(conn) + "/redis-pipeline", {
    method: "POST",
    body: JSON.stringify({ commands: [argv] }),
  });
  if (!j) return null;
  return { reply: j.replies && j.replies.length ? j.replies[0] : null };
}

/** Rename a key (SPEC §data.redis) on the library's one-field sheet (SPEC §panel.pages). The sheet owns
 *  the shared rules: a name is required (inline, not a toast) and an unchanged name is a cancel.
 *  A refused RENAME returns false - the guarded console already toasted why - so the sheet
 *  stays with the typed name beside the reason; the old hand-built sheet closed first and lost
 *  it. On success the key list and the value reload behind the closing sheet. */
function dbRedisRenameSheet(key               )       {
  // The menu that opens this only exists over a selected key, but the record says the
  // selection is nullable and that is the truth to honour — not an assertion at the call.
  if (!key) return;
  openFieldSheet({
    title: tr("dataBrowsers.renameKey"),
    label: tr("dataBrowsers.newName"),
    def: key,
    save: tr("dataBrowsers.rename2"),
    exact: true,
    submit: async (to        )                            => {
      // RENAMENX, not RENAME: RENAME onto a name that exists throws that key's value away
      // without a word. A taken name is said inline, beside the typed name.
      const j = await dbRedisExec(["RENAMENX", key, to]);
      if (!j) return false;
      if (Number(j.reply) === 0) return tr("dataBrowsers.renameTargetExists", { to: dbKeyShown(to) });
      toast(tr("dataBrowsers.renamed", { to: dbKeyShown(to) }));
      const t = dbTab();
      if (t.kind === "key") t.redisKey = to;
      void dbLoadKeys(true).then(() => { void dbLoadRedisValue(to); });
      return true;
    },
  });
}

/* Deleting a key is the one destructive act the redis side has. It asks once, in words: the
   typed-name confirm it used to share with the table DROP/TRUNCATE made the operator retype
   the name they had just clicked (the owner, 2026-09-28: "太蠢了"). A key is not a table -
   one DEL, named in the question, is the weight this carries. */
function dbRedisDeleteKey()       {
  const d = dbTab();
  if (d.kind !== "key") return;
  const key = d.redisKey ;
  if (!confirm(tr("dataBrowsers.deleteKeyConfirm", { key: dbKeyShown(key) }))) return;
  void dbRedisExec(["DEL", key]).then(async (j                           )                => {
    if (!j) return;
    // DEL answers how many it removed: 0 is a key someone else deleted first, said as such.
    toast(tr(Number(j.reply) ? "dataBrowsers.deletedKey" : "dataBrowsers.keyAlreadyGone", { key: dbKeyShown(key) }));
    const t = dbTab();
    if (t.kind === "key") {
      t.redisKey = null;
      t.redisValue = null;
      t.redisEdits = null;
    }
    await dbLoadKeys(true);
    renderDbGrid();
  });
}

/* --- #pane's delegated listeners for the redis value view (SPEC §panel.toolchain) ----------------------------
   Behavior notes (SPEC §panel.toolchain): every action resolves its key, buffer and type config
   from LIVE state at event time — the add-row shape, the TTL editor's seed value and the
   row-control addresses all re-read dbTab(), so a re-read or a key switch between render
   and click acts on what is on screen now, never on the painted snapshot. */

function dbRedisClick(t         )          {
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
      const key = d.redisKey;
      if (key == null) return;
      const secs         = input.value.trim();
      if (secs && !/^\d+$/.test(secs)) { toast(tr("dataBrowsers.ttlMustWholeNumber"), true); void dbLoadRedisValue(key); return; }
      // The same two rules as the sheet (2026-09-29): 0 would delete the key with no question
      // asked, and the key goes as ONE argument - this line used to be "EXPIRE " + key, which
      // the console split at every blank in the name.
      if (secs && Number(secs) === 0) { toast(tr("dataBrowsers.ttlZeroDeletes"), true); void dbLoadRedisValue(key); return; }
      void dbRedisExec(secs ? ["EXPIRE", key, secs] : ["PERSIST", key]).then((j)       => {
        if (j) dbRedisTtlSaid(key, secs, j.reply);
        void dbLoadRedisValue(key);
      });
    };
    // Transient in-place overlay, the sheet idiom (SPEC §panel.toolchain keeps per-open wiring): the
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
  DB_REDIS_TYPES, REDIS_THING_KEYS, dbIsRedis, dbLoadKeys, dbLoadRedisValue, dbRedisValidScore, dbRefreshKeyspace, dbRefreshRedisValue, dbRewalkKeys,
  dbRedisClick, dbRedisCommandText, dbRedisCommands, dbRedisCommit, dbRedisDiscard,
  dbRedisDeleteKey, dbRedisDisplayText, dbRedisEntries, dbRedisKeydown, dbRedisPendingCount, dbRedisRenameKey,
  dbRedisTtlSheet, dbRenderRedisValue,
  dbRedisCellMenu, // SPEC §data.streams: the stream view reuses the SPEC §data.grid cell menu
};
