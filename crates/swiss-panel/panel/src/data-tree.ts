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

/** One folded redis namespace: its full prefix (no trailing ":"), the keys that sit exactly
 *  on it, and the namespaces beneath it. */
export interface RedisNsGroup {
  ns: string;
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
  const root = nsNode("", keys, keyCmp);
  const tops = root.children;
  if (!root.keys.length) return tops;
  const bare: RedisNsGroup = { ns: "", children: [], keys: root.keys };
  return [bare].concat(tops);
}

/** One node: keys whose text ends at this prefix sit on it; the rest recurse by their next
 *  segment. A node with no keys of its own and exactly one child segment is a pass-through
 *  and folds into that child (the label carries the whole chain). */
function nsNode(
  ns: string,
  keys: ApiDbRedisKeyRow[],
  keyCmp: (a: ApiDbRedisKeyRow, b: ApiDbRedisKeyRow) => number,
): RedisNsGroup {
  const direct: ApiDbRedisKeyRow[] = [];
  const bySeg: Record<string, ApiDbRedisKeyRow[]> = {};
  keys.forEach((k: ApiDbRedisKeyRow): void => {
    const rest = ns ? k.key.slice(ns.length + 1) : k.key;
    const cut = rest.indexOf(":");
    if (cut < 0) { direct.push(k); return; }
    const seg = rest.slice(0, cut);
    if (!bySeg[seg]) bySeg[seg] = [];
    bySeg[seg].push(k);
  });
  direct.sort(keyCmp);
  const segs = Object.keys(bySeg).sort(cmpName);
  // The root itself never compresses (its label is empty); every other pass-through folds.
  if (ns && !direct.length && segs.length === 1) {
    return nsNode(ns + ":" + segs[0], bySeg[segs[0]], keyCmp);
  }
  return {
    ns: ns,
    children: segs.map((s: string): RedisNsGroup => { return nsNode(ns ? ns + ":" + s : s, bySeg[s], keyCmp); }),
    keys: direct,
  };
}

