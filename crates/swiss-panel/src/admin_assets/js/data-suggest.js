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


/* --- redis completion (2026-09-28; rebuilt on the server's own catalog, docs/50) --------------- */
/* The owner, on the redis console: "I need completion, and templates - otherwise there is no way
   to know how to set a key, or delete one." Then, on what that first cut became: "Redis 命令自动
   补全的功能，做得也很烂，没有 template，反正做得我不太满意，需要你来重构下."

   What was wrong with the table this file used to carry: forty commands written out by hand, no
   summaries, nothing past the first argument, and a Templates menu buried in the tab's ⋯. The
   catalog now comes from the SERVER — COMMAND DOCS plus COMMAND INFO (docs/50) — so it is that
   server's commands, its modules, its version, with redis' own one-line summaries and its own
   argument spelling; completion works at every word, not just the first; and the console's hint
   line under the box shows the syntax of the command being typed. */

/* The catalog row the console codes against is the wire shape itself (types/api.d.ts):
   one type, written once, for what the server says and what this file reads. */
                                       

/** The lines the Templates menu offers: one ready command per thing an operator does, the words
 *  themselves standing in for the arguments (no <braces> to delete before it runs). The catalog
 *  says what a command TAKES; a template says what somebody came here to DO, which is why this
 *  list is written rather than derived. */
const REDIS_TEMPLATES                                    = [
  { group: "string", line: "SET key value" },
  { group: "string", line: "SET key value EX 60" },
  { group: "string", line: "SET key value NX" },
  { group: "string", line: "GET key" },
  { group: "string", line: "MGET key1 key2" },
  { group: "key", line: "DEL key" },
  { group: "key", line: "EXPIRE key 60" },
  { group: "key", line: "PERSIST key" },
  { group: "key", line: "TTL key" },
  { group: "key", line: "RENAME key newkey" },
  { group: "key", line: "SCAN 0 MATCH prefix:* COUNT 100" },
  { group: "key", line: "TYPE key" },
  { group: "hash", line: "HSET key field value" },
  { group: "hash", line: "HGETALL key" },
  { group: "hash", line: "HDEL key field" },
  { group: "hash", line: "HSCAN key 0 MATCH f* COUNT 100" },
  { group: "list", line: "RPUSH key value" },
  { group: "list", line: "LRANGE key 0 -1" },
  { group: "list", line: "LLEN key" },
  { group: "set", line: "SADD key member" },
  { group: "set", line: "SMEMBERS key" },
  { group: "set", line: "SSCAN key 0 COUNT 100" },
  { group: "zset", line: "ZADD key 1 member" },
  { group: "zset", line: "ZRANGE key 0 -1 WITHSCORES" },
  { group: "zset", line: "ZRANGEBYSCORE key -inf +inf LIMIT 0 20" },
  { group: "stream", line: "XADD key * field value" },
  { group: "stream", line: "XREVRANGE key + - COUNT 20" },
  { group: "stream", line: "XLEN key" },
  { group: "stream", line: "XINFO STREAM key" },
  { group: "server", line: "INFO keyspace" },
  { group: "server", line: "MEMORY USAGE key" },
  { group: "server", line: "DBSIZE" },
];

/** The word the redis console would complete: a key is a byte string, so its run takes the
 *  characters a key name is made of - colon groups, dashes, dots, and * for a pattern - where
 *  the SQL class takes identifier characters. Pure. */
function dbRedisWordAt(text         , caret        )         {
  const s = String(text).slice(0, caret);
  const m = s.match(/[A-Za-z0-9_.:*?[\]{}@$#+-]+$/);
  return m ? m[0] : "";
}

/** The words of the line the caret sits on, and which of them the caret is in. A console holds
 *  several lines; a command is one of them, and completing against the whole box would read the
 *  line above as part of this command. Pure. */
export function dbRedisLineAt(text        , caret        )                                     {
  const upto = text.slice(0, caret);
  const line = upto.slice(upto.lastIndexOf("\n") + 1);
  const words = line.split(/\s+/);
  // A line ending in a space puts the caret on a new, empty word - which is exactly where a
  // token or the next key should be offered.
  return { words: words.filter((w        , i        )          => { return !!w || i === words.length - 1; }), index: Math.max(0, words.length - 1) };
}

/** The catalog row for the command a line names, subcommands included: `XINFO STREAM` is one
 *  name in the catalog, so the two-word form is tried before the one-word one. Pure. */
export function dbRedisCmdOf(words          , cmds            )                  {
  if (!words.length || !words[0]) return null;
  const two = words.length > 1 && words[1]
    ? (words[0] + " " + words[1]).toUpperCase()
    : "";
  const byName = (n        )                       => {
    return cmds.find((c          )          => { return c.name === n; });
  };
  return (two ? byName(two) : undefined) || byName(words[0].toUpperCase()) || null;
}

/** Whether the word at `index` of a command's line is a KEY argument, by the three numbers
 *  redis-cli reads for the same purpose: first key, last key (negative counts back from the
 *  end) and step. Word 0 is the command itself, so the positions are 1-based over the line.
 *  Pure. */
export function dbRedisKeyAt(cmd          , index        , words        )          {
  const first = cmd.firstKey || 0;
  if (first <= 0 || index < first) return false;
  // A subcommand spends one more word on its name before its arguments start.
  const last = cmd.lastKey == null ? first : cmd.lastKey;
  const step = cmd.step && cmd.step > 0 ? cmd.step : 1;
  // A negative last key counts from the END of the line, redis' own way: -1 is the last
  // word (DEL takes keys all the way there), -2 the one before it.
  const end = last < 0 ? words + last : last;
  if (index > end) return false;
  return (index - first) % step === 0;
}

/** What to offer for the caret in a redis console line (docs/50 §2.2). Word 0 completes from the
 *  catalog; the word after a container completes from its subcommands; a key position completes
 *  from the keys the sidebar has walked; anything else completes the command's own tokens (NX,
 *  MATCH, WITHSCORES). A value is never guessed - it is the operator's to type, and a list of
 *  other keys' names there would be noise. Pure. */
export function dbRedisCandidates(
  text        , caret        , keys          , cmds            ,
)                        {
  const prefix = dbRedisWordAt(text, caret);
  const line = dbRedisLineAt(text, caret);
  const item = (c          , label        )                      => {
    return {
      label: label,
      kind: c.group || "command",
      // The syntax first (it is what gets typed next), the summary after it.
      detail: (c.syntax ? label + " " + c.syntax : label) + (c.summary ? " — " + c.summary : ""),
    };
  };
  const up = prefix.toUpperCase();
  if (line.index === 0) {
    // An empty first word offers nothing: a list of every command the server has is not a
    // suggestion, it is a manual. Past the command, an empty word is exactly where the next
    // key or token belongs, so there the blank prefix is a question worth answering.
    if (!prefix) return [];
    return cmds
      .filter((c          )          => { return c.name.indexOf(" ") < 0 && c.name.startsWith(up); })
      .slice(0, SUGGEST_MAX_ITEMS)
      .map((c          )                      => { return item(c, c.name); });
  }
  const head = (line.words[0] || "").toUpperCase();
  if (line.index === 1) {
    // A container's subcommands, offered under the leaf name that gets typed.
    const subs = cmds.filter((c          )          => {
      return c.name.startsWith(head + " ") && c.name.slice(head.length + 1).startsWith(up);
    });
    if (subs.length) {
      return subs.slice(0, SUGGEST_MAX_ITEMS).map((c          )                      => {
        return item(c, c.name.slice(head.length + 1));
      });
    }
  }
  const cmd = dbRedisCmdOf(line.words, cmds);
  if (!cmd) return [];
  // A key position is measured over the line as the SERVER sees it, so a subcommand's own
  // word counts exactly as the catalog's key positions already assume.
  if (dbRedisKeyAt(cmd, line.index, line.words.length)) {
    return keys
      .filter((k        )          => { return k.startsWith(prefix); })
      .slice(0, SUGGEST_MAX_ITEMS)
      .map((k        )                      => { return { label: k, kind: "key" }; });
  }
  const typed = line.words.map((w        )         => { return w.toUpperCase(); });
  return (cmd.tokens || [])
    .filter((t        )          => { return t.startsWith(up) && typed.indexOf(t) < 0; })
    .slice(0, SUGGEST_MAX_ITEMS)
    .map((t        )                      => { return { label: t, kind: "token", detail: cmd.name + " " + cmd.syntax }; });
}

/** The console's hint line while a redis command is being typed (docs/50 §2.3): the command's
 *  own syntax and summary, from the server that will run it. Empty when the line names nothing
 *  known, so the standing hint stays. Pure. */
export function dbRedisSignature(text        , caret        , cmds            )         {
  const line = dbRedisLineAt(text, caret);
  const cmd = dbRedisCmdOf(line.words, cmds);
  if (!cmd) return "";
  const head = (cmd.name + " " + cmd.syntax).trim();
  return cmd.summary ? head + " — " + cmd.summary : head;
}

/** The keys the sidebar has walked so far - one half of the completion's sources. */
function dbLoadedKeys()           {
  const r = dbConn().redis;
  return r && r.keys ? r.keys.map((k                 )         => { return k.key; }) : [];
}

/** The catalog this connection answered with, or an empty one until it has. */
function dbRedisCommands()             {
  return dbConn().redisCommands || [];
}

/* One catalog read per connection, in flight or done: the answer is the SERVER's, it does not
   change while the connection lives, and asking again per keystroke would be absurd. */
let dbRedisCatalogFor = "";

/** Read the command catalog for the open connection, once (docs/50). A failure is quiet: the
 *  console still runs commands, the completion simply has nothing to offer, and the next
 *  connection switch tries again. */
export async function dbRedisLoadCommands()                {
  const d = dbConn();
  if (!d.conn || !dbIsRedis()) return;
  if (dbRedisCatalogFor === d.conn) return;
  dbRedisCatalogFor = d.conn;
  const j = await apiJson                          (
    "/api/db/" + encodeURIComponent(d.conn) + "/redis-commands");
  const cur = dbConn();
  if (!j || cur.conn !== d.conn) return; // the switch moved on; its own load will run
  cur.redisCommands = j.commands || [];
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
    const items = dbRedisCandidates(this.value, this.selectionStart || 0, dbLoadedKeys(), dbRedisCommands());
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
  REDIS_TEMPLATES, dbRedisCommands, dbRedisWordAt, dbSuggestByteOffset,
  dbSuggestHide, dbSuggestKeys, dbSuggestOnInput, dbSuggestPrefixAt,
};
