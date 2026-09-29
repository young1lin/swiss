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

import type { ApiDbCompletionItem, ApiDbCompletionReply, ApiRedisCommand, ApiRedisCommandsResponse, ApiRedisKeySpec } from "./types/api.js";
import { $, apiJson, el } from "./util.js";
import { dbIsRedis } from "./data-browsers.js";
import { dbSqlPaint } from "./data-filters.js";
import { dbConn, dbSqlTab } from "./db-state.js";
import { dbKeyShown } from "./data-tree.js";
// docs/50: the same lexer the gateway splits a console line with (vendored, MIT).
import { split } from "./vendor/shlex/3.0.0/index.js";


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
export type RedisCmd = ApiRedisCommand;

/** The lines the Templates menu offers: one ready command per thing an operator does, the words
 *  themselves standing in for the arguments (no <braces> to delete before it runs). The catalog
 *  says what a command TAKES; a template says what somebody came here to DO, which is why this
 *  list is written rather than derived. */
const REDIS_TEMPLATES: { group: string; line: string }[] = [
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

/** Past this many characters a console line is data being pasted, not a command being typed:
 *  it is counted by blanks, not shlex, and nothing is offered on it (2026-09-29 - see
 *  dbRedisWordSpan for what a long quoted value used to cost per keystroke). */
const REDIS_LINE_MAX = 65536;

/** How far back from the caret a word's raw start is looked for. A word longer than this is a
 *  pasted value; nothing completes it, so its span need not be exact. */
const REDIS_SPAN_LOOKBACK = 4096;

/** shlex's split of a half-typed piece of line: an open quote is closed for the count (the
 *  caret is then inside that quoted word). null when no closing quote makes it readable - a
 *  trailing backslash. Pure. */
function dbRedisSplitOpen(s: string): { words: string[]; open: boolean } | null {
  for (const close of ["", "\"", "'"]) {
    try {
      return { words: split(s + close), open: close !== "" };
    } catch (e) { /* not closed by this quote; try the next */ }
  }
  return null;
}

/** The words of the line the caret sits on, and which of them the caret is in, split the way
 *  the SERVER will split that line when it runs: shlex on both sides (the console route since
 *  7017a51, the vendored JS port here), so `MSET "a b" 1 user:` is four words and the key
 *  positions count exactly the words redis will see. A console line is usually half-typed: an
 *  open quote is closed for the count (the caret is then inside that quoted word), and a line
 *  shlex cannot read at all - a trailing backslash - falls back to whitespace. Leading blanks
 *  are not a word. Pure. */
export function dbRedisLineAt(text: string, caret: number): { words: string[]; index: number } {
  const upto = text.slice(0, caret);
  const line = upto.slice(upto.lastIndexOf("\n") + 1);
  const read = line.length > REDIS_LINE_MAX ? null : dbRedisSplitOpen(line);
  const words = read ? read.words : line.trim() ? line.trim().split(/\s+/) : [];
  const open = !!read && read.open;
  // A caret after whitespace stands on a new, empty word - exactly where the next key or
  // token is offered - unless that whitespace is inside an open quote or escaped (`my\ `).
  const blankEnd = /\s$/.test(line) && !/(^|[^\\])(\\\\)*\\\s$/.test(line);
  if (!words.length || (!open && blankEnd)) words.push("");
  return { words: words, index: words.length - 1 };
}

/** The word the caret is completing, as the server will read it, and where its RAW text
 *  starts - the span a pick replaces. A key's name is whatever bytes it holds (用户:1, a
 *  space, a quote), so the word is the shlex word the line splits into, not a run of
 *  key-looking characters: that run made `GET 用户` offer every key and paste the pick after
 *  用户 (2026-09-29). The raw start is the nearest blank-led point before the caret from which
 *  the rest splits into exactly that one word - a blank inside an open quote or after a
 *  backslash starts no such run. Only that tail is split, never the line before it: splitting
 *  the whole prefix once per blank inside a long quoted value (a pasted JSON) stalled the
 *  console for minutes a keystroke. A tail that no split reads, or none within
 *  REDIS_SPAN_LOOKBACK, falls back to the blank-bounded run, as dbRedisLineAt does. Pure. */
export function dbRedisWordSpan(text: string, caret: number): { start: number; word: string } {
  const from = text.lastIndexOf("\n", caret - 1) + 1;
  const line = dbRedisLineAt(text, caret);
  const word = line.words[line.index] || "";
  const floor = Math.max(from, caret - REDIS_SPAN_LOOKBACK);
  for (let p = caret; p >= floor; p--) {
    if (p > from && !/\s/.test(text.charAt(p - 1))) continue;
    if (p === caret) {
      if (word === "") return { start: p, word: word };
      continue;
    }
    const tail = dbRedisSplitOpen(text.slice(p, caret));
    if (tail && tail.words.length === 1 && tail.words[0] === word) return { start: p, word: word };
  }
  const run = text.slice(from, caret).match(/\S*$/);
  return { start: caret - (run ? run[0].length : 0), word: word };
}

/** A key as the console must type it: bare when a split reads it back unchanged, else in
 *  double quotes with its `"` and `\` escaped - the one form the panel's splitter, the
 *  server's and redis-cli all read back as the same bytes. The empty name is `""`. Pure. */
export function dbRedisQuoteArg(s: string): string {
  if (s && !/[\s"'\\]/.test(s)) return s;
  return "\"" + s.replace(/[\\"]/g, "\\$&") + "\"";
}

/** The catalog row for the command a line names, subcommands included: `XINFO STREAM` is one
 *  name in the catalog, so the two-word form is tried before the one-word one. Pure. */
export function dbRedisCmdOf(words: string[], cmds: RedisCmd[]): RedisCmd | null {
  if (!words.length || !words[0]) return null;
  const two = words.length > 1 && words[1]
    ? (words[0] + " " + words[1]).toUpperCase()
    : "";
  const byName = (n: string): RedisCmd | undefined => {
    return cmds.find((c: RedisCmd): boolean => { return c.name === n; });
  };
  return (two ? byName(two) : undefined) || byName(words[0].toUpperCase()) || null;
}

/** Whether one key specification (redis 7) makes word `index` of a line a key. The line is
 *  usually half-typed, so "the end of the arguments" is the end of what is typed so far - a
 *  range that runs to the end grows as the operator types, and XREAD's `limit 2` keeps the
 *  first half of what follows STREAMS as keys (the rest are ids), rounding up so the one word
 *  being typed after STREAMS is a key. Pure. */
function dbRedisSpecKeyAt(spec: ApiRedisKeySpec, words: string[], index: number): boolean {
  const argc = words.length;
  let start = -1;
  const b = spec.begin;
  if (b.type === "index") {
    start = b.index;
  } else {
    const kw = b.keyword.toUpperCase();
    // A keyword found only BEFORE the caret's word counts: the word being typed is not yet the
    // keyword it may become.
    if (b.startfrom >= 0) {
      for (let j = b.startfrom; j < index; j++) { if (words[j].toUpperCase() === kw) { start = j + 1; break; } }
    } else {
      for (let j = Math.min(index - 1, argc + b.startfrom); j >= 1; j--) { if (words[j].toUpperCase() === kw) { start = j + 1; break; } }
    }
  }
  if (start < 1 || index < start) return false;
  const f = spec.find;
  const step = f.keystep > 0 ? f.keystep : 1;
  if (f.type === "range") {
    let last: number;
    if (f.lastkey >= 0) last = start + f.lastkey;
    else if (f.lastkey === -1 && f.limit > 1) last = start + Math.ceil((argc - start) / f.limit) - 1;
    else last = argc + f.lastkey;
    return index <= last && (index - start) % step === 0;
  }
  // keynum: the count sits at start + keynumidx and must already be typed, as a number.
  const at = start + f.keynumidx;
  if (at >= index) return false;
  const n = Number(words[at]);
  if (!Number.isInteger(n) || n < 1) return false;
  const first = start + f.firstkey;
  const last = first + (n - 1) * step;
  return index >= first && index <= last && (index - first) % step === 0;
}

/** Whether word `index` lies past a keyword that opens a list running to the END of the line
 *  (XREAD's STREAMS, MIGRATE's KEYS): a key spec that begins at a keyword and whose range has
 *  lastkey -1. Past it only keys and their values follow - no option token is valid any more,
 *  so none is offered. With `split`, only the lists that are keys-then-values (`limit` > 1),
 *  where a word may be either while the line is unfinished. Pure. */
function dbRedisPastTail(cmd: RedisCmd, index: number, words: string[], split: boolean): boolean {
  return (cmd.keySpecs || []).some((sp: ApiRedisKeySpec): boolean => {
    const f = sp.find;
    if (f.type !== "range" || f.lastkey !== -1 || sp.begin.type !== "keyword") return false;
    if (split && f.limit <= 1) return false;
    const kw = sp.begin.keyword.toUpperCase();
    for (let j = Math.max(1, sp.begin.startfrom); j < index; j++) {
      if (words[j].toUpperCase() === kw) return true;
    }
    return false;
  });
}

/** Whether word `index` MIGHT be a key where nobody can know yet: XREAD's `STREAMS a b` could
 *  be two keys still to be followed by two ids, or one key and its id. The caller offers keys
 *  there only when the typed prefix already matches one - an id (`0`, `$`, `>`) never does,
 *  a key name being typed does. Pure. */
export function dbRedisKeyMaybe(cmd: RedisCmd, index: number, words: string[]): boolean {
  return dbRedisPastTail(cmd, index, words, true);
}

/** Whether word `index` of a command's line is a KEY argument (word 0 is the command, so the
 *  positions count over the whole line - a subcommand's own word included, exactly as the
 *  server's numbers assume). redis 7's key specs decide when the catalog has them: they are
 *  exact, including the commands whose keys move. Otherwise the three legacy numbers redis-cli
 *  reads: first key, last key (negative counts back from the end of the line) and step.
 *  Pure. */
export function dbRedisKeyAt(cmd: RedisCmd, index: number, words: string[]): boolean {
  if (index < 1) return false;
  if (cmd.keySpecs && cmd.keySpecs.length) {
    return cmd.keySpecs.some((sp: ApiRedisKeySpec): boolean => { return dbRedisSpecKeyAt(sp, words, index); });
  }
  const first = cmd.firstKey || 0;
  if (first <= 0 || index < first) return false;
  const last = cmd.lastKey == null ? first : cmd.lastKey;
  const step = cmd.step && cmd.step > 0 ? cmd.step : 1;
  const end = last < 0 ? words.length + last : last;
  if (index > end) return false;
  return (index - first) % step === 0;
}

/** What to offer for the caret in a redis console line (docs/50 §2.2). Word 0 completes from the
 *  catalog; the word after a container completes from its subcommands; a key position completes
 *  from the keys the sidebar has walked; anything else completes the command's own tokens (NX,
 *  MATCH, WITHSCORES). A value is never guessed - it is the operator's to type, and a list of
 *  other keys' names there would be noise. Pure. */
export function dbRedisCandidates(
  text: string, caret: number, keys: string[], cmds: RedisCmd[],
): DbSuggestItem[] {
  const lineStart = text.lastIndexOf("\n", caret - 1) + 1;
  if (caret - lineStart > REDIS_LINE_MAX) return [];
  const line = dbRedisLineAt(text, caret);
  const prefix = line.words[line.index] || "";
  const item = (c: RedisCmd, label: string): ApiDbCompletionItem => {
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
      .filter((c: RedisCmd): boolean => { return c.name.indexOf(" ") < 0 && c.name.startsWith(up); })
      .slice(0, SUGGEST_MAX_ITEMS)
      .map((c: RedisCmd): ApiDbCompletionItem => { return item(c, c.name); });
  }
  const head = (line.words[0] || "").toUpperCase();
  if (line.index === 1) {
    // A container's subcommands, offered under the leaf name that gets typed.
    const subs = cmds.filter((c: RedisCmd): boolean => {
      return c.name.startsWith(head + " ") && c.name.slice(head.length + 1).startsWith(up);
    });
    if (subs.length) {
      return subs.slice(0, SUGGEST_MAX_ITEMS).map((c: RedisCmd): ApiDbCompletionItem => {
        return item(c, c.name.slice(head.length + 1));
      });
    }
  }
  const cmd = dbRedisCmdOf(line.words, cmds);
  if (!cmd) return [];
  // A key position is measured over the line as the SERVER sees it, so a subcommand's own
  // word counts exactly as the catalog's key positions already assume.
  if (dbRedisKeyAt(cmd, line.index, line.words) ||
      (prefix && dbRedisKeyMaybe(cmd, line.index, line.words) && keys.some((k: string): boolean => { return k.startsWith(prefix); }))) {
    // A name holding a line break cannot be typed on one console line, so it is not offered
    // (the line would run as two commands); every other name goes in as the server reads it.
    return keys
      .filter((k: string): boolean => { return k.startsWith(prefix) && !/[\r\n]/.test(k); })
      .slice(0, SUGGEST_MAX_ITEMS)
      .map((k: string): DbSuggestItem => {
        return { label: dbKeyShown(k), kind: "key", insert: dbRedisQuoteArg(k) };
      });
  }
  // Past STREAMS (or any keyword that opens a to-the-end key list) only keys and values
  // follow: COUNT and BLOCK there would be offers redis refuses.
  if (dbRedisPastTail(cmd, line.index, line.words, false)) return [];
  const typed = line.words.map((w: string): string => { return w.toUpperCase(); });
  return (cmd.tokens || [])
    .filter((t: string): boolean => { return t.startsWith(up) && typed.indexOf(t) < 0; })
    .slice(0, SUGGEST_MAX_ITEMS)
    .map((t: string): ApiDbCompletionItem => { return { label: t, kind: "token", detail: cmd.name + " " + cmd.syntax }; });
}

/** The console's hint line while a redis command is being typed (docs/50 §2.3): the command's
 *  own syntax and summary, from the server that will run it. Empty when the line names nothing
 *  known, so the standing hint stays. Pure. */
export function dbRedisSignature(text: string, caret: number, cmds: RedisCmd[]): string {
  const line = dbRedisLineAt(text, caret);
  const cmd = dbRedisCmdOf(line.words, cmds);
  if (!cmd) return "";
  const head = (cmd.name + " " + cmd.syntax).trim();
  return cmd.summary ? head + " — " + cmd.summary : head;
}

/** The keys the sidebar has walked so far - one half of the completion's sources. */
function dbLoadedKeys(): string[] {
  const r = dbConn().redis;
  // A name that is not UTF-8 cannot be typed as its printed text, so it is not offered.
  return r
    ? r.keys.filter((k: { binary?: boolean }): boolean => { return !k.binary; }).map((k: { key: string }): string => { return k.key; })
    : [];
}

/** The catalog this connection answered with, or an empty one until it has. */
function dbRedisCommands(): RedisCmd[] {
  return dbConn().redisCommands || [];
}

/* One catalog read per connection, in flight or done: the answer is the SERVER's, it does not
   change while the connection lives, and asking again per keystroke would be absurd. */
let dbRedisCatalogFor = "";

/** Read the command catalog for the open connection, once (docs/50). A failure is quiet: the
 *  console still runs commands, the completion simply has nothing to offer, and the next
 *  connection switch tries again. */
export async function dbRedisLoadCommands(): Promise<void> {
  const d = dbConn();
  if (!d.conn || !dbIsRedis()) return;
  if (dbRedisCatalogFor === d.conn) return;
  dbRedisCatalogFor = d.conn;
  const j = await apiJson<ApiRedisCommandsResponse>(
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
let dbSuggestTimer: ReturnType<typeof setTimeout> | null = null;
/** A completion row: what it shows, and - when that is not what gets typed - what does (a
 *  key named `my key` goes in as `"my key"`, or the line would hand redis two words). */
type DbSuggestItem = ApiDbCompletionItem & { insert?: string };
let dbSuggestItems: DbSuggestItem[] = [];
let dbSuggestSel = -1;
let dbSuggestPrefix = "";

/** The word the server would complete: the [A-Za-z0-9_.$] run ending at the caret — the same
 *  character class sql_word_ending_at speaks on the server. Pure. */
function dbSuggestPrefixAt(text: unknown, caret: number): string {
  const s = String(text).slice(0, caret);
  const m = s.match(/[A-Za-z0-9_.$]+$/);
  return m ? m[0] : "";
}

/** The caret as a byte offset: the server slices bytes, a selection index counts UTF-16
 *  units, and the two only agree while the statement is ASCII. Pure. */
function dbSuggestByteOffset(text: unknown, caret: number): number {
  return new TextEncoder().encode(String(text).slice(0, caret)).length;
}

function dbSuggestHide(): void {
  const box = $("dbSuggest");
  if (box && box.parentNode) box.parentNode.removeChild(box);
  dbSuggestItems = [];
  dbSuggestSel = -1;
  dbSuggestPrefix = "";
}

/** The input hook (debounced): a caret over no word closes the list at once — a debounce that
 *  kept a dead list on screen would be a lie about what is being completed. */
function dbSuggestOnInput(this: HTMLTextAreaElement): void {
  clearTimeout(dbSuggestTimer!);
  const d = dbConn();
  if (!d.conn) { dbSuggestHide(); return; }
  // Redis answers from the table above, with no server and no debounce: the candidates are
  // already here, and a 150ms wait on a local list only makes the console feel slow.
  if (dbIsRedis()) {
    const items = dbRedisCandidates(this.value, this.selectionStart || 0, dbLoadedKeys(), dbRedisCommands());
    if (!items.length) { dbSuggestHide(); return; }
    dbSuggestItems = items;
    const caret = this.selectionStart || 0;
    dbSuggestPrefix = this.value.slice(dbRedisWordSpan(this.value, caret).start, caret);
    dbSuggestSel = -1;
    dbSuggestRender(this);
    return;
  }
  if (!dbSuggestPrefixAt(this.value, this.selectionStart)) { dbSuggestHide(); return; }
  const ta = this;
  dbSuggestTimer = setTimeout(() => { void dbSuggestFetch(ta); }, SUGGEST_DEBOUNCE_MS);
}

async function dbSuggestFetch(ta: HTMLTextAreaElement): Promise<void> {
  const d = dbConn();
  if (!d.conn || dbIsRedis() || !ta.closest(".db-sql-wrap")) { dbSuggestHide(); return; }
  const caret = ta.selectionStart!;
  const text = ta.value;
  const prefix = dbSuggestPrefixAt(text, caret);
  if (!prefix) { dbSuggestHide(); return; }
  const j = await apiJson<ApiDbCompletionReply>("/api/db/" + encodeURIComponent(d.conn) + "/completion", {
    method: "POST",
    body: JSON.stringify({ sql: text, caret: dbSuggestByteOffset(text, caret) }),
  });
  if (!j) { dbSuggestHide(); return; }
  const items: ApiDbCompletionItem[] = (j.items || []).slice(0, SUGGEST_MAX_ITEMS);
  if (!items.length) { dbSuggestHide(); return; }
  // A reply for a word the caret has already left is stale: drop it, the next keystroke is
  // fetching its own.
  if (dbSuggestPrefixAt(ta.value, ta.selectionStart!) !== prefix) return;
  dbSuggestItems = items;
  dbSuggestPrefix = prefix;
  dbSuggestSel = -1;
  dbSuggestRender(ta);
}

function dbSuggestRender(ta: HTMLTextAreaElement): void {
  // Replace only the BOX ELEMENT here — dbSuggestHide() also clears the item list, and this
  // function draws from that list: calling it first once rendered an empty, positioned box
  // for every reply (caught live on 19998).
  const old = $("dbSuggest");
  if (old && old.parentNode) old.parentNode.removeChild(old);
  const wrap = ta.closest(".db-sql-wrap");
  if (!wrap) return;
  const box = el("div", "db-suggest");
  box.id = "dbSuggest";
  dbSuggestItems.forEach((it: ApiDbCompletionItem, i: number): void => {
    const row = el("div", "db-suggest-item");
    row.appendChild(el("span", "db-suggest-label", String(it.label)));
    row.appendChild(el("span", "db-suggest-kind", String(it.kind)));
    row.title = it.detail || it.kind;
    // mousedown, not click: the textarea's blur would tear the list down before a click on
    // it lands, and preventDefault keeps the caret where the accept will splice.
    row.onmousedown = (e: MouseEvent): void => { e.preventDefault(); dbSuggestAccept(i); };
    box.appendChild(row);
  });
  wrap.appendChild(box);
  dbSuggestPlace(ta, box);
}

/** Put the list at the caret. The mirror div inherits the textarea's face (same font, size,
 *  padding, width, wrapping) and carries a zero-width marker right after the text up to the
 *  caret; the marker's offset minus the textarea's scroll is the caret's point in the wrap. */
function dbSuggestPlace(ta: HTMLTextAreaElement, box: HTMLElement): void {
  const wrap = ta.closest(".db-sql-wrap");
  if (!wrap) return;
  const mirror = el("div", "db-sql-mirror db-sql-face");
  mirror.textContent = ta.value.slice(0, ta.selectionStart!);
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

function dbSuggestPaintSel(): void {
  const box = $("dbSuggest");
  if (!box) return;
  const rows = box.children;
  for (let i = 0; i < rows.length; i++) {
    rows[i].classList.toggle("sel", i === dbSuggestSel);
  }
}

/** Replace the typed prefix with the chosen label and hand the console back its paint. */
function dbSuggestAccept(i: number): void {
  const ta = $<HTMLTextAreaElement>("dbSql");
  if (!ta || !dbSuggestItems[i]) { dbSuggestHide(); return; }
  const label = String(dbSuggestItems[i].insert ?? dbSuggestItems[i].label);
  const caret = ta.selectionStart!;
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
function dbSuggestKeys(e: KeyboardEvent): void {
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
  REDIS_TEMPLATES, dbRedisCommands, dbSuggestByteOffset,
  dbSuggestHide, dbSuggestKeys, dbSuggestOnInput, dbSuggestPrefixAt,
};
