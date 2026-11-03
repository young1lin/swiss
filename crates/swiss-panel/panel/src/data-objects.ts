/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */
import type { ApiDbTableRow } from "./types/api.js";

/** Only object addresses are retained. Rows, credentials and database payloads never enter storage. */
export interface ObjectAddress { name: string; schema: string | null }
interface Preferences { favorites: ObjectAddress[]; recent: ObjectAddress[]; prefixes: boolean }
const KEY = "swiss.dbObjects.";
const same = (a: ObjectAddress, b: ObjectAddress): boolean => a.name === b.name && a.schema === b.schema;
const key = (conn: string, database: string | null): string => KEY + JSON.stringify([conn, database]);
export function objectPrefs(conn: string, database: string | null): Preferences {
  const empty: Preferences = { favorites: [], recent: [], prefixes: false };
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(key(conn, database)) || "null");
    if (!raw || typeof raw !== "object") return empty;
    const data = raw as Record<string, unknown>;
    const list = (value: unknown, max: number): ObjectAddress[] => Array.isArray(value) ? value.filter((v): v is ObjectAddress => !!v && typeof v === "object" && typeof v.name === "string" && v.name.length <= 512 && (v.schema === null || typeof v.schema === "string" && v.schema.length <= 512)).slice(0, max) : [];
    return { favorites: list(data.favorites, 100), recent: list(data.recent, 20), prefixes: data.prefixes === true };
  } catch { return empty; }
}
function save(conn: string, database: string | null, data: Preferences): boolean {
  try {
    const target = key(conn, database), encoded = JSON.stringify(data);
    let scopes = 1, bytes = encoded.length;
    for (let i = 0; i < localStorage.length; i++) {
      const name = localStorage.key(i);
      if (name?.startsWith(KEY) && name !== target) { scopes++; bytes += (localStorage.getItem(name) || "").length; }
    }
    if (scopes > 64 || bytes > 500_000) return false;
    localStorage.setItem(target, encoded); return true;
  } catch { return false; }
}
export function visitObject(conn: string, database: string | null, address: ObjectAddress): void {
  const data = objectPrefs(conn, database);
  data.recent = [address, ...data.recent.filter(a => !same(a, address))].slice(0, 20);
  save(conn, database, data);
}
export function favoriteObject(conn: string, database: string | null, address: ObjectAddress): boolean {
  const data = objectPrefs(conn, database);
  if (data.favorites.some(a => same(a, address))) data.favorites = data.favorites.filter(a => !same(a, address));
  else { if (data.favorites.length >= 100) return false; data.favorites.push(address); }
  return save(conn, database, data);
}
export function prefixObjects(conn: string, database: string | null, enabled: boolean): boolean {
  const data = objectPrefs(conn, database); data.prefixes = enabled; return save(conn, database, data);
}
/** A single prefix level prevents deep names from creating deep DOM trees. */
export function objectPrefixes(rows: ApiDbTableRow[]): { name: string; rows: ApiDbTableRow[] }[] {
  const groups = new Map<string, ApiDbTableRow[]>();
  for (const r of rows) { const at = r.name.search(/[_:.]/); const name = at > 0 ? r.name.slice(0, at + 1) : ""; const g = groups.get(name); if (g) g.push(r); else groups.set(name, [r]); }
  return [...groups].map(([name, rows]) => ({ name, rows }));
}
