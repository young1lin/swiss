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

                                                                                
import { $, apiJson, el } from "./util.js";
import { dbIsRedis } from "./data-browsers.js";
import { dbSqlPaint } from "./data-filters.js";
import { dbConn, dbSqlTab } from "./db-state.js";


/* --- redis completion (2026-09-28) ------------------------------------------------------------ */
/* The owner, on the redis console: "I need completion, and templates - otherwise there is no way
   to know how to set a key, or delete one." Redis needs no round trip for either: the command set
   is fixed, and the key names are already in the sidebar. So this half is a table and two pure
   functions, and the SQL path above keeps the server to itself. */

/** One redis command as the console offers it: what to type, what it takes, and what it acts on
 *  (the word the sidebar already labels the key with, so the two agree). `key` says the first
 *  argument is a key name - that is what makes the second word completable. */
                                                                              

const REDIS_COMMANDS             = [
  { name: "GET", args: "key", group: "string", key: true },
  { name: "SET", args: "key value [EX seconds]", group: "string", key: true },
  { name: "SETEX", args: "key seconds value", group: "string", key: true },
  { name: "INCR", args: "key", group: "string", key: true },
  { name: "APPEND", args: "key value", group: "string", key: true },
  { name: "STRLEN", args: "key", group: "string", key: true },
  { name: "DEL", args: "key [key ...]", group: "key", key: true },
  { name: "EXISTS", args: "key", group: "key", key: true },
  { name: "RENAME", args: "key newkey", group: "key", key: true },
  { name: "TYPE", args: "key", group: "key", key: true },
  { name: "TTL", args: "key", group: "key", key: true },
  { name: "EXPIRE", args: "key seconds", group: "key", key: true },
  { name: "PERSIST", args: "key", group: "key", key: true },
  { name: "KEYS", args: "pattern", group: "key", key: true },
  { name: "SCAN", args: "cursor [MATCH pattern] [COUNT n]", group: "key", key: false },
  { name: "HGET", args: "key field", group: "hash", key: true },
  { name: "HSET", args: "key field value", group: "hash", key: true },
  { name: "HDEL", args: "key field", group: "hash", key: true },
  { name: "HGETALL", args: "key", group: "hash", key: true },
  { name: "HKEYS", args: "key", group: "hash", key: true },
  { name: "LPUSH", args: "key value", group: "list", key: true },
  { name: "RPUSH", args: "key value", group: "list", key: true },
  { name: "LPOP", args: "key", group: "list", key: true },
  { name: "LRANGE", args: "key start stop", group: "list", key: true },
  { name: "LLEN", args: "key", group: "list", key: true },
  { name: "SADD", args: "key member", group: "set", key: true },
  { name: "SREM", args: "key member", group: "set", key: true },
  { name: "SMEMBERS", args: "key", group: "set", key: true },
  { name: "SCARD", args: "key", group: "set", key: true },
  { name: "ZADD", args: "key score member", group: "zset", key: true },
  { name: "ZREM", args: "key member", group: "zset", key: true },
  { name: "ZRANGE", args: "key start stop [WITHSCORES]", group: "zset", key: true },
  { name: "ZCARD", args: "key", group: "zset", key: true },
  { name: "XADD", args: "key * field value", group: "stream", key: true },
  { name: "XLEN", args: "key", group: "stream", key: true },
  { name: "XRANGE", args: "key start end [COUNT n]", group: "stream", key: true },
  { name: "XREVRANGE", args: "key end start [COUNT n]", group: "stream", key: true },
  { name: "DBSIZE", args: "", group: "server", key: false },
  { name: "INFO", args: "[section]", group: "server", key: false },
];

/** The lines the Templates menu offers: one ready command per thing an operator does, the words
 *  themselves standing in for the arguments (no <braces> to delete before it runs). */
const REDIS_TEMPLATES                                    = [
  { group: "string", line: "SET key value" },
  { group: "string", line: "SET key value EX 60" },
  { group: "string", line: "GET key" },
  { group: "key", line: "DEL key" },
  { group: "key", line: "EXPIRE key 60" },
  { group: "key", line: "PERSIST key" },
  { group: "key", line: "TTL key" },
  { group: "key", line: "RENAME key newkey" },
  { group: "key", line: "SCAN 0 MATCH prefix:* COUNT 100" },
  { group: "hash", line: "HSET key field value" },
  { group: "hash", line: "HGETALL key" },
  { group: "hash", line: "HDEL key field" },
  { group: "list", line: "RPUSH key value" },
  { group: "list", line: "LRANGE key 0 -1" },
  { group: "set", line: "SADD key member" },
  { group: "set", line: "SMEMBERS key" },
  { group: "zset", line: "ZADD key 1 member" },
  { group: "zset", line: "ZRANGE key 0 -1 WITHSCORES" },
  { group: "stream", line: "XADD key * field value" },
  { group: "stream", line: "XREVRANGE key + - COUNT 20" },
  { group: "server", line: "INFO keyspace" },
];

/** The word the redis console would complete: a key is a byte string, so its run takes the
 *  characters a key name is made of - colon groups, dashes, dots, and * for a pattern - where
 *  the SQL class takes identifier characters. Pure. */
function dbRedisWordAt(text         , caret        )         {
  const s = String(text).slice(0, caret);
  const m = s.match(/[A-Za-z0-9_.:*?[\]{}@$#+-]+$/);
  return m ? m[0] : "";
}

/** What to offer for the caret in a redis console line: the first word completes from the
 *  command table (with its arguments as the detail), and the word after a command that reads a
 *  key completes from the keys already walked into the sidebar. A value is never guessed - it is
 *  the operator's to type, and a list of other keys' names there would be noise. Pure. */
function dbRedisCandidates(text        , caret        , keys          )                        {
  const prefix = dbRedisWordAt(text, caret);
  if (!prefix) return [];
  const before = text.slice(0, caret - prefix.length);
  const words = before.split(/\s+/).filter((w        )          => { return !!w; });
  if (!words.length) {
    const up = prefix.toUpperCase();
    return REDIS_COMMANDS
      .filter((c          )          => { return c.name.startsWith(up); })
      .slice(0, SUGGEST_MAX_ITEMS)
      .map((c          )                      => {
        return { label: c.name, kind: c.group, detail: (c.name + " " + c.args).trim() };
      });
  }
  if (words.length > 1) return []; // past the key: an argument is the operator's own
  const cmd = REDIS_COMMANDS.find((c          )          => { return c.name === words[0].toUpperCase(); });
  if (!cmd || !cmd.key) return [];
  return keys
    .filter((k        )          => { return k.startsWith(prefix); })
    .slice(0, SUGGEST_MAX_ITEMS)
    .map((k        )                      => { return { label: k, kind: "key" }; });
}

/** The keys the sidebar has walked so far - the completion's whole key source. */
function dbLoadedKeys()           {
  const r = dbConn().redis;
  return r && r.keys ? r.keys.map((k                 )         => { return k.key; }) : [];
}

/* --- SQL completion (docs/22 W3.1) ------------------------------------------------------------------ */
/* The console's suggestion list. The SERVER builds the candidate set (dialect keywords + table
   names + the FROM-nearest table's columns, cached per connection); this side only decides
   WHEN to ask: 150ms after a keystroke that ends in a word, never for redis, never more than
   8 rows on screen. Positioning borrows the highlight overlay's own trick — a hidden mirror
   that inherits the textarea's face and places a marker at the caret. */

const SUGGEST_DEBOUNCE_MS = 150;
const SUGGEST_MAX_ITEMS = 8;
let dbSuggestTimer                                       = null;
let dbSuggestItems                        = [];
let dbSuggestSel = -1;
let dbSuggestPrefix = "";

/** The word the server would complete: the [A-Za-z0-9_.$] run ending at the caret — the same
 *  character class sql_word_ending_at speaks on the server. Pure. */
function dbSuggestPrefixAt(text         , caret        )         {
  const s = String(text).slice(0, caret);
  const m = s.match(/[A-Za-z0-9_.$]+$/);
  return m ? m[0] : "";
}

/** The caret as a byte offset: the server slices bytes, a selection index counts UTF-16
 *  units, and the two only agree while the statement is ASCII. Pure. */
function dbSuggestByteOffset(text         , caret        )         {
  return new TextEncoder().encode(String(text).slice(0, caret)).length;
}

function dbSuggestHide()       {
  const box = $("dbSuggest");
  if (box && box.parentNode) box.parentNode.removeChild(box);
  dbSuggestItems = [];
  dbSuggestSel = -1;
  dbSuggestPrefix = "";
}

/** The input hook (debounced): a caret over no word closes the list at once — a debounce that
 *  kept a dead list on screen would be a lie about what is being completed. */
function dbSuggestOnInput(                         )       {
  clearTimeout(dbSuggestTimer );
  const d = dbConn();
  if (!d.conn) { dbSuggestHide(); return; }
  // Redis answers from the table above, with no server and no debounce: the candidates are
  // already here, and a 150ms wait on a local list only makes the console feel slow.
  if (dbIsRedis()) {
    const items = dbRedisCandidates(this.value, this.selectionStart || 0, dbLoadedKeys());
    if (!items.length) { dbSuggestHide(); return; }
    dbSuggestItems = items;
    dbSuggestPrefix = dbRedisWordAt(this.value, this.selectionStart || 0);
    dbSuggestSel = -1;
    dbSuggestRender(this);
    return;
  }
  if (!dbSuggestPrefixAt(this.value, this.selectionStart)) { dbSuggestHide(); return; }
  const ta = this;
  dbSuggestTimer = setTimeout(() => { void dbSuggestFetch(ta); }, SUGGEST_DEBOUNCE_MS);
}

async function dbSuggestFetch(ta                     )                {
  const d = dbConn();
  if (!d.conn || dbIsRedis() || !ta.closest(".db-sql-wrap")) { dbSuggestHide(); return; }
  const caret = ta.selectionStart ;
  const text = ta.value;
  const prefix = dbSuggestPrefixAt(text, caret);
  if (!prefix) { dbSuggestHide(); return; }
  const j = await apiJson                      ("/api/db/" + encodeURIComponent(d.conn) + "/completion", {
    method: "POST",
    body: JSON.stringify({ sql: text, caret: dbSuggestByteOffset(text, caret) }),
  });
  if (!j) { dbSuggestHide(); return; }
  const items                        = (j.items || []).slice(0, SUGGEST_MAX_ITEMS);
  if (!items.length) { dbSuggestHide(); return; }
  // A reply for a word the caret has already left is stale: drop it, the next keystroke is
  // fetching its own.
  if (dbSuggestPrefixAt(ta.value, ta.selectionStart ) !== prefix) return;
  dbSuggestItems = items;
  dbSuggestPrefix = prefix;
  dbSuggestSel = -1;
  dbSuggestRender(ta);
}

function dbSuggestRender(ta                     )       {
  // Replace only the BOX ELEMENT here — dbSuggestHide() also clears the item list, and this
  // function draws from that list: calling it first once rendered an empty, positioned box
  // for every reply (caught live on 19998).
  const old = $("dbSuggest");
  if (old && old.parentNode) old.parentNode.removeChild(old);
  const wrap = ta.closest(".db-sql-wrap");
  if (!wrap) return;
  const box = el("div", "db-suggest");
  box.id = "dbSuggest";
  dbSuggestItems.forEach((it                     , i        )       => {
    const row = el("div", "db-suggest-item");
    row.appendChild(el("span", "db-suggest-label", String(it.label)));
    row.appendChild(el("span", "db-suggest-kind", String(it.kind)));
    row.title = it.detail || it.kind;
    // mousedown, not click: the textarea's blur would tear the list down before a click on
    // it lands, and preventDefault keeps the caret where the accept will splice.
    row.onmousedown = (e            )       => { e.preventDefault(); dbSuggestAccept(i); };
    box.appendChild(row);
  });
  wrap.appendChild(box);
  dbSuggestPlace(ta, box);
}

/** Put the list at the caret. The mirror div inherits the textarea's face (same font, size,
 *  padding, width, wrapping) and carries a zero-width marker right after the text up to the
 *  caret; the marker's offset minus the textarea's scroll is the caret's point in the wrap. */
function dbSuggestPlace(ta                     , box             )       {
  const wrap = ta.closest(".db-sql-wrap");
  if (!wrap) return;
  const mirror = el("div", "db-sql-mirror db-sql-face");
  mirror.textContent = ta.value.slice(0, ta.selectionStart );
  const marker = el("span", "db-suggest-mark", "\u200b");
  mirror.appendChild(marker);
  wrap.appendChild(mirror);
  const left = marker.offsetLeft - ta.scrollLeft;
  let top = marker.offsetTop - ta.scrollTop + marker.offsetHeight;
  wrap.removeChild(mirror);
  // Below the caret line by default; flip above when the list would run past the console.
  if (top + box.offsetHeight > wrap.clientHeight) {
    top = Math.max(0, top - marker.offsetHeight - box.offsetHeight);
  }
  box.style.left = Math.max(0, left) + "px";
  box.style.top = top + "px";
}

function dbSuggestPaintSel()       {
  const box = $("dbSuggest");
  if (!box) return;
  const rows = box.children;
  for (let i = 0; i < rows.length; i++) {
    rows[i].classList.toggle("sel", i === dbSuggestSel);
  }
}

/** Replace the typed prefix with the chosen label and hand the console back its paint. */
function dbSuggestAccept(i        )       {
  const ta = $                     ("dbSql");
  if (!ta || !dbSuggestItems[i]) { dbSuggestHide(); return; }
  const label = String(dbSuggestItems[i].label);
  const caret = ta.selectionStart ;
  const start = caret - dbSuggestPrefix.length;
  ta.setRangeText(label, start, caret, "end");
  const st = dbSqlTab();
  if (st) st.sqlText = ta.value;
  dbSqlPaint();
  dbSuggestHide();
  ta.focus();
}

/** The console's key hook while the list is open: arrows move, Tab/Enter accept, Esc closes.
 *  Ctrl/Cmd+Enter falls through untouched — Run keeps working with the list on screen. */
function dbSuggestKeys(e               )       {
  if (!dbSuggestItems.length) return;
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    const n = dbSuggestSel < 0
      ? (e.key === "ArrowDown" ? 0 : dbSuggestItems.length - 1)
      : (dbSuggestSel + (e.key === "ArrowDown" ? 1 : -1) + dbSuggestItems.length) % dbSuggestItems.length;
    dbSuggestSel = n;
    dbSuggestPaintSel();
  } else if (e.key === "Tab" || (e.key === "Enter" && !e.ctrlKey && !e.metaKey && !e.altKey)) {
    e.preventDefault();
    dbSuggestAccept(dbSuggestSel < 0 ? 0 : dbSuggestSel);
  } else if (e.key === "Escape") {
    e.preventDefault();
    dbSuggestHide();
  }
}

export {
  REDIS_TEMPLATES, dbRedisCandidates, dbRedisWordAt, dbSuggestByteOffset, dbSuggestHide, dbSuggestKeys,
  dbSuggestOnInput, dbSuggestPrefixAt,
};
