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

import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Same DOM-stub technique as admin-data-cellview.test.ts: the panel ships browser ES modules,
// so the module graph needs the globals stubbed before it will evaluate under Node.
const el = (): Record<string, unknown> => {
  const node: Record<string, unknown> = {
    style: {}, dataset: {}, hidden: false, disabled: false, checked: false, value: "",
    textContent: "", innerHTML: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    setAttribute: () => {}, getAttribute: () => "", removeAttribute: () => {},
    addEventListener: () => {}, removeEventListener: () => {},
    appendChild: (c: unknown) => c, removeChild: (c: unknown) => c, remove: () => {},
    querySelector: () => null, querySelectorAll: () => [],
    focus: () => {}, blur: () => {}, click: () => {},
    getBoundingClientRect: () => ({ left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 }),
    contains: () => false, closest: () => null, insertAdjacentHTML: () => {},
  };
  node.cloneNode = () => el();
  return node;
};
const doc = {
  documentElement: el(), body: el(), head: el(),
  hidden: false, visibilityState: "visible", activeElement: null,
  getElementById: () => el(), createElement: () => el(), createTextNode: () => el(),
  querySelector: () => null, querySelectorAll: () => [],
  addEventListener: () => {}, removeEventListener: () => {},
};

// The one piece of storage these tests actually read: a scripted localStorage, so favorite
// load/save round-trips pin against real key/value behaviour instead of a no-op stub.
const store = new Map<string, string>();
const localStorage = {
  getItem: (k: string) => (store.has(k) ? store.get(k)! : null),
  setItem: (k: string, v: string) => void store.set(k, v),
  removeItem: (k: string) => void store.delete(k),
};
Object.assign(globalThis, {
  document: doc, window: globalThis, localStorage,
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {},
  Blob: class {}, setInterval: () => 0, clearInterval: () => 0,
  addEventListener: () => {}, removeEventListener: () => {},
});

const admin = join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets");
const sql = await import(pathToFileURL(join(admin, "js", "data-sql.js")).href) as {
  dbFormatSql: (text: string) => string;
  dbFavoriteName: (sql: string) => string;
  dbFavPush: (sql: string) => void;
};
/* This suite drives the EMITTED tree, so the record has to come from the emitted tree too:
   a static import of ../src/db-state.js would be a second, unrelated instance and dbFavPush
   would look like it did nothing. */
const dbState = await import(pathToFileURL(join(admin, "js", "db-state.js")).href) as {
  dbConn: () => { favorites: string[] };
};

// The whitespace-only equivalent the round-trip pins against: the console's own lexer, every
// token kept verbatim, all whitespace dropped. Two statements are equivalent-for-running
// exactly when their token streams are equal.
const SQL_TOKEN_RE = /(--[^\n]*|#[^\n]*|\/\*[\s\S]*?\*\/)|('(?:[^'\\]|\\.|'')*'|"(?:[^"\\]|\\.|"")*"|`[^`]*`)|(\b\d+(?:\.\d+)?\b)|([A-Za-z_][A-Za-z0-9_$]*)|(\s+)|([^\sA-Za-z0-9_$]+)/g;
function stripWs(text: string): string {
  var out: string[] = [];
  var m: RegExpExecArray | null;
  SQL_TOKEN_RE.lastIndex = 0;
  while ((m = SQL_TOKEN_RE.exec(text)) !== null) {
    if (!m[5]) out.push(m[0]);
  }
  return out.join("");
}

/* SPEC §data.console — the lightweight formatter. Lexical only (the console's own SQL_TOKEN_RE
   stream), clause line breaks plus a two-space continuation indent, and it changes NOTHING
   but whitespace: case, tokens, strings and comments stay byte-identical, so a formatted
   statement runs exactly as it did. */
describe("dbFormatSql — clause breaks and the two-space continuation", () => {
  it("breaks the top-level clauses onto their own lines", () => {
    expect(sql.dbFormatSql("select id, name from t where x = 1 order by id"))
      .toBe("select id, name\nfrom t\nwhere x = 1\norder by id");
  });

  it("never touches case — the author's spelling is the output's spelling", () => {
    expect(sql.dbFormatSql("SELECT id FROM t WHERE X = 1"))
      .toBe("SELECT id\nFROM t\nWHERE X = 1");
  });

  it("indents AND/OR continuations two spaces under their clause", () => {
    expect(sql.dbFormatSql("select * from t where a = 1 and b = 2 or c = 3"))
      .toBe("select *\nfrom t\nwhere a = 1\n  and b = 2\n  or c = 3");
  });

  it("breaks JOIN lines with their leading keyword, ON stays with the join", () => {
    expect(sql.dbFormatSql("select * from a left join b on a.id = b.id inner join c on b.x = c.x"))
      .toBe("select *\nfrom a\nleft join b on a.id = b.id\ninner join c on b.x = c.x");
  });

  it("splits statements at the top-level semicolon", () => {
    expect(sql.dbFormatSql("select 1; select 2;"))
      .toBe("select 1;\nselect 2;");
  });

  it("keeps parenthesized content on one line and knows its spacing", () => {
    expect(sql.dbFormatSql("insert into t ( a , b ) values ( 1 , 2 )"))
      .toBe("insert into t (a, b)\nvalues (1, 2)");
    expect(sql.dbFormatSql("select t.a,fn( x , y ) from t"))
      .toBe("select t.a, fn (x, y)\nfrom t");
  });

  it("a line comment ends its line — the next token can never join the comment", () => {
    expect(sql.dbFormatSql("select 1 -- keep\n, 2"))
      .toBe("select 1 -- keep\n, 2");
  });

  it("strings ride verbatim: case, inner semicolons, inner whitespace", () => {
    expect(sql.dbFormatSql("select 'a;b -- c  ' from t;"))
      .toBe("select 'a;b -- c  '\nfrom t;");
  });

  it("is idempotent — formatting the formatted output changes nothing", () => {
    var messy = "select   id,name\n\nfrom t  where  x=1 and y = 2 order by\tid";
    var once = sql.dbFormatSql(messy);
    expect(sql.dbFormatSql(once)).toBe(once);
  });
});

describe("dbFormatSql — the running equivalence the format button promises", () => {
  const samples = [
    "SELECT id, name FROM users WHERE age > 21 AND name LIKE 'a%' ORDER BY id DESC LIMIT 10",
    "update  t set a = 1, b = 'x y'   where id = 3;",
    "select * from a left join b on a.id = b.id join c on b.x = c.x where a.q is null or c.r = 2",
    "insert into t (a, b) values (1, 2)",
    "select 1; select 'two; three'; -- tail comment\nselect 4",
    "SELECT o.total, c.name\nFROM orders o JOIN customers c ON o.customer_id = c.id GROUP BY o.total HAVING count(*) > 1",
  ];
  for (var s of samples) {
    it("keeps the token stream of: " + s.slice(0, 40), () => {
      expect(stripWs(sql.dbFormatSql(s))).toBe(stripWs(s));
    });
  }
});

describe("favorites — localStorage swiss.dbFavorites (SPEC §data.console, key renamed SPEC §panel)", () => {
  it("names a query by its first line, folded and truncated", () => {
    expect(sql.dbFavoriteName("select 1\nfrom t")).toBe("select 1");
    expect(sql.dbFavoriteName("  select  a,\nb from t")).toBe("select a,");
    var long = "select " + "x".repeat(100);
    expect(sql.dbFavoriteName(long)).toHaveLength(80);
  });

  it("saves newest-first, never duplicates, caps at 50", () => {
    dbState.dbConn().favorites = [];
    sql.dbFavPush("select 1");
    sql.dbFavPush("select 2");
    expect(dbState.dbConn().favorites).toEqual(["select 2", "select 1"]);
    sql.dbFavPush("select 1"); // a repeat save moves to the top, never duplicates
    expect(dbState.dbConn().favorites).toEqual(["select 1", "select 2"]);
    for (var i = 0; i < 60; i++) sql.dbFavPush("select " + (100 + i));
    expect(dbState.dbConn().favorites.length).toBe(50);
    expect(dbState.dbConn().favorites[0]).toBe("select 159");
    // and it round-trips through storage under the panel's key
    expect(store.get("swiss.dbFavorites")).toBe(JSON.stringify(dbState.dbConn().favorites));
  });
});

describe("the sprite carries the favorites star", () => {
  it("i-star exists", () => {
    const shell = readFileSync(join(admin, "index.html"), "utf8");
    expect(shell).toContain('<symbol id="i-star"');
  });
});

/* The live page once crashed on this exact seam: the wiring queried #dbSqlFormat before the
   skeleton carried it (the DOM-stubbed boot test cannot see a missing id — its $() returns a
   node for anything), so the whole Data view died on mount. The skeleton and the wiring are
   pinned to agree instead.
   SPEC §panel.toolchain: the skeleton is a node tree now — the ids live as h() props and the aria-label
   as an aria bag entry, which is what the emitted source carries. */
describe("the console skeleton carries what its wiring queries", () => {
  it("dbSqlRun exists in the skeleton; the flat row's buttons left with SPEC §data.tabs", () => {
    const src = readFileSync(join(admin, "js", "data-view.js"), "utf8");
    // The console's flat action row folded into the toolbar (SPEC §data.tabs): Run is the
    // toolbar's primary action (data-grid's emit carries it), the rest ride the overflow.
    // What still must agree here is the textarea the paint fills — and that the flat row's
    // ids really left the skeleton.
    expect(src).toContain('id: "dbSql"');
    expect(src).not.toContain('id: "dbSqlRun"');
    expect(src).not.toContain('id: "dbSqlFormat"');
    expect(src).not.toContain('id: "dbSqlFav"');
    expect(src).not.toContain('id: "dbSqlExplain"');
    // The toolbar's Run lives in data-grid's emit and answers the same delegated id.
    const grid = readFileSync(join(admin, "js", "data-grid.js"), "utf8");
    expect(grid).toContain('id: "dbSqlRun"');
  });
});
