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

/* ================================================================================================
   The Data sidebar's tree shape (docs/43 M2) - the pure half.

   renderDbTables turns the flat lists the API answers (/tables pages, redis SCAN pages) into
   the mockup-B tree: SQL connections cut into Tables / Views / Routines sections, a Postgres
   catalog nested schema -> section -> rows, a redis keyspace folded along ":" with
   single-child chains compressed. The cutting rules live HERE, DOM-free, because they are
   contracts: a view landing under Tables (or a redis namespace sprouting one-key folder
   chains) is a bug the acceptance suite pins without a document.

   A leaf module like group-logic.ts: data-view imports it, it imports types only - the
   vitest suite reaches it without a DOM.
   ================================================================================================ */
import type { ApiDbRedisKeyRow, ApiDbTableRow } from "./types/api.js";

/** The three SQL sections, in display order. The ids are stable across locales (they key the
 *  band's collapse state); the label the operator reads is tr("dataTree.<id>"). */
export const DB_TREE_SECTIONS = ["tables", "views", "routines"] as const;
export type DbTreeSection = (typeof DB_TREE_SECTIONS)[number];

/** Which section a catalog row belongs to. ApiDbTableRow.type is OPTIONAL on the wire - a
 *  server that does not answer it is still answering tables, so a missing value lands under
 *  Tables, never under a mystery fourth section. "view" and "matview" are the view-like
 *  kinds; the table-like kinds land under Tables; anything a dialect grows beyond those
 *  (procedures, functions) lands under Routines, which is honestly empty until one does.
 *  Pure. */
export function dbSectionOf(t: ApiDbTableRow): DbTreeSection {
  const ty = t.type || "";
  if (ty === "view" || ty === "matview") return "views";
  if (ty === "" || ty === "table" || ty === "partitioned table" || ty === "foreign table") {
    return "tables";
  }
  return "routines";
}

/** The sections as slices, every section keeping its slot even at count 0: an empty Views
 *  band says "this catalog has no views", which is information, and the tree's shape never
 *  jumps between connections. Pure. */
export function dbSectionSlices(rows: ApiDbTableRow[]): { name: DbTreeSection; rows: ApiDbTableRow[] }[] {
  return DB_TREE_SECTIONS.map((s: DbTreeSection) => {
    return { name: s, rows: rows.filter((t: ApiDbTableRow): boolean => { return dbSectionOf(t) === s; }) };
  });
}

/** One folded redis namespace: its name (the prefix without its trailing ":"), the exact
 *  prefix every key under it starts with, the keys that sit exactly on it, and the namespaces
 *  beneath it. `prefix` is what the tree slices by and tells groups apart with: `ns` alone is
 *  "" both for the root and for keys that START with ":" (`:lead`), and it was that one
 *  ambiguity that sent the builder into endless recursion (2026-09-29). */
export interface RedisNsGroup {
  ns: string;
  prefix: string;
  children: RedisNsGroup[];
  keys: ApiDbRedisKeyRow[];
}

/** Numeric name compare - the same leg dbRedisCompare walks, stated once so the tree and the
 *  flat list it replaced cannot disagree about order (t2 < t10, case folded). */
function cmpName(a: string, b: string): number {
  return a.localeCompare(b, undefined, { numeric: true, sensitivity: "base" });
}

/** Fold a flat keyspace along ":" (docs/43 M2), compressing single-child chains: with only
 *  stream:orders under stream, the tree shows ONE node stream:orders, not a stream folder
 *  around one child. Children sort before keys, both by name - a file listing's order, and
 *  a stable one at any size. Keys without a ":" are the keyspace's own rows: they come back
 *  as the FIRST element, a group with ns "" holding them (and nothing else), so the caller
 *  paints them at the root without a band over their heads. An empty keyspace answers [].
 *  Pure. */
export function redisNamespaceTree(
  keys: ApiDbRedisKeyRow[],
  keyCmp: (a: ApiDbRedisKeyRow, b: ApiDbRedisKeyRow) => number =
    (a: ApiDbRedisKeyRow, b: ApiDbRedisKeyRow): number => { return cmpName(a.key, b.key); },
): RedisNsGroup[] {
  const root = nsNode("", keys, keyCmp, 0);
  const tops = root.children;
  if (!root.keys.length) return tops;
  const bare: RedisNsGroup = { ns: "", prefix: "", children: [], keys: root.keys };
  return [bare].concat(tops);
}

/** The sprite glyph for a redis type (2026-09-28): a key row and its open card lead with it, so
 *  the six core types differ at a glance - "string" and "stream" side by side in the meta read
 *  alike. Monochrome like every descriptive mark (design rule 2); the word stays in the meta. A
 *  module type (ReJSON-RL, vectorset) or "none" is the plain key mark. Pure. */
const REDIS_TYPE_GLYPH: Record<string, string> = {
  string: "type", hash: "hash", list: "list", set: "braces", zset: "sort", stream: "activity",
};

export function redisTypeGlyph(type: string): string {
  return Object.prototype.hasOwnProperty.call(REDIS_TYPE_GLYPH, type) ? REDIS_TYPE_GLYPH[type] : "key";
}

/* The characters a page draws as nothing, or as something else: C0/C1 controls, the soft
   hyphen, zero-width and bidi-control marks (U+202E draws the rest of a name backwards), the
   line and paragraph separators, the BOM. */
const KEY_INVISIBLE = /[\u0000-\u001f\u007f-\u009f\u00ad\u200b-\u200f\u2028-\u202e\u2060-\u2064\ufeff]/;
const KEY_INVISIBLE_G = /[\u007f-\u009f\u00ad\u200b-\u200f\u2028-\u202e\u2060-\u2064\ufeff]/g;

/** A key's name as the panel prints it. Almost every name prints as itself; one the page or a
 *  dialog would silently change prints the way redis-cli prints it - quoted, escaped. HTML
 *  folds blanks, so ` x` and `x ` both painted as `x`, and `line\nbreak` read as a key named
 *  `line break` (2026-09-29). The empty name is `""`. Display only: every act sends the key's
 *  own bytes. Pure. */
export function dbKeyShown(key: string): string {
  if (key !== "" && !/^\s|\s$/.test(key) && !KEY_INVISIBLE.test(key)) return key;
  // JSON.stringify escapes C0 controls, the quote and the backslash; the rest by code point.
  return JSON.stringify(key).replace(KEY_INVISIBLE_G, (c: string): string => {
    return "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0");
  });
}

/** How many levels the keyspace tree folds into before the rest of a key is its row's name. */
export const REDIS_NS_MAX_DEPTH = 32;

/** One node: keys whose text ends at this prefix sit on it; the rest recurse by their next
 *  segment. A node with no keys of its own and exactly one child segment is a pass-through
 *  and folds into that child (the label carries the whole chain).
 *
 *  Every recursion adds at least one character (the segment and its ":") to the prefix, and
 *  every key under a node starts with that node's prefix, so the walk ends whatever the keys
 *  look like - a leading ":", "::" runs, a trailing ":". Segments are grouped in a Map: a
 *  plain object answered `constructor`, `toString` and `__proto__` from its prototype, and a
 *  keyspace with a `constructor:` namespace crashed the sidebar.
 *
 *  And the walk is at most REDIS_NS_MAX_DEPTH calls deep, folds included: past that, every
 *  key is a row of the deepest band, named by the rest of its key. A staircase of 5,000
 *  levels (`a:x`, `a:a:x`, ...) or two keys sharing a run of 100,000 colons overflowed the
 *  stack - here and in every walk of the tree after it - and took the Data page with it
 *  (2026-09-29). Nobody reads a band 32 levels down; everybody reads a page that paints. */
function nsNode(
  prefix: string,
  keys: ApiDbRedisKeyRow[],
  keyCmp: (a: ApiDbRedisKeyRow, b: ApiDbRedisKeyRow) => number,
  depth: number,
): RedisNsGroup {
  const direct: ApiDbRedisKeyRow[] = [];
  const bySeg = new Map<string, ApiDbRedisKeyRow[]>();
  const deepest = depth >= REDIS_NS_MAX_DEPTH;
  keys.forEach((k: ApiDbRedisKeyRow): void => {
    const rest = k.key.slice(prefix.length);
    // A name that is not UTF-8 is never split: its text is redis-cli's printing of the bytes,
    // and a ':' in that printing names no namespace. It stays at the root, whole.
    const cut = deepest || k.binary ? -1 : rest.indexOf(":");
    if (cut < 0) { direct.push(k); return; }
    const seg = rest.slice(0, cut);
    const list = bySeg.get(seg);
    if (list) list.push(k); else bySeg.set(seg, [k]);
  });
  direct.sort(keyCmp);
  const segs = Array.from(bySeg.keys()).sort(cmpName);
  // The root itself never compresses (its label is empty); every other pass-through folds.
  if (prefix && !direct.length && segs.length === 1) {
    return nsNode(prefix + segs[0] + ":", bySeg.get(segs[0]) || [], keyCmp, depth + 1);
  }
  return {
    ns: prefix.slice(0, -1),
    prefix: prefix,
    children: segs.map((s: string): RedisNsGroup => { return nsNode(prefix + s + ":", bySeg.get(s) || [], keyCmp, depth + 1); }),
    keys: direct,
  };
}

