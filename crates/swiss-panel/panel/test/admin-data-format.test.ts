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
  dbFormatSql: (text: string, o?: { dialect?: string; indent?: number }) => string;
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

/* SPEC §data.console - the formatter lays a query out the way a JetBrains IDE's SQL formatter
   does with its defaults (DataGrip's code style; the owner's ask: "the same as IDEA's Data Grip"),
   and it changes NOTHING but whitespace: case, tokens, strings and comments stay byte-identical,
   so a formatted statement runs exactly as it did. */
const fmt = (text: string, o?: { dialect?: string; indent?: number }): string => sql.dbFormatSql(text, o);
describe("dbFormatSql - DataGrip's layout", () => {
  it("a select list chops one element a line, aligned after the keyword; the clauses start at the query's column", () => {
    expect(fmt("select id, name from t where x = 1 order by id"))
      .toBe("select id,\n       name\nfrom t\nwhere x = 1\norder by id");
    expect(fmt("SELECT id FROM t WHERE X = 1"), "case is the author's").toBe("SELECT id\nFROM t\nWHERE X = 1");
    expect(fmt("select distinct a, b from t"), "a modifier stays on the keyword's line")
      .toBe("select distinct a,\n                b\nfrom t");
  });

  it("AND/OR start their lines right-aligned under WHERE; a BETWEEN keeps its AND", () => {
    expect(fmt("select * from t where a = 1 and b = 2 or c = 3"))
      .toBe("select *\nfrom t\nwhere a = 1\n  and b = 2\n   or c = 3");
    expect(fmt("select * from t where a between 1 and 5 and b = 2"))
      .toBe("select *\nfrom t\nwhere a between 1 and 5\n  and b = 2");
  });

  it("a join starts its line four past the tables, ON with it; comma tables line up", () => {
    expect(fmt("select * from a left join b on a.id = b.id inner join c on b.x = c.x"))
      .toBe("select *\nfrom a\n         left join b on a.id = b.id\n         inner join c on b.x = c.x");
    expect(fmt("select * from a, b where a.id = b.id")).toBe("select *\nfrom a,\n     b\nwhere a.id = b.id");
  });

  it("the whole query: group by and order by stay on a line while they fit, limit keeps its offset", () => {
    expect(fmt("select u.id, u.name, count(*) as n from users u left join orders o on u.id = o.user_id " +
      "where u.age > 21 and o.total > 100 group by u.id, u.name having count(*) > 2 order by n desc limit 10 offset 5"))
      .toBe([
        "select u.id,",
        "       u.name,",
        "       count(*) as n",
        "from users u",
        "         left join orders o on u.id = o.user_id",
        "where u.age > 21",
        "  and o.total > 100",
        "group by u.id, u.name",
        "having count(*) > 2",
        "order by n desc",
        "limit 10 offset 5",
      ].join("\n"));
  });

  it("a subquery is laid out the same way at the column after its bracket", () => {
    expect(fmt("select * from t where id in (select user_id from orders where total > 10 and status = 'paid')"))
      .toBe("select *\nfrom t\nwhere id in (select user_id\n             from orders\n             where total > 10\n               and status = 'paid')");
    expect(fmt("select x from (select a as x from t) s where s.x > 1"))
      .toBe("select x\nfrom (select a as x\n      from t) s\nwhere s.x > 1");
  });

  it("insert, update, delete, with, union: each the IDE's way", () => {
    expect(fmt("insert into t ( a , b ) values ( 1 , 2 )")).toBe("insert into t (a, b)\nvalues (1, 2)");
    expect(fmt("insert into t (a, b) values (1, 2), (3, 4)")).toBe("insert into t (a, b)\nvalues (1, 2),\n       (3, 4)");
    expect(fmt("update t set a = 1, b = 2 where id = 3")).toBe("update t\nset a = 1,\n    b = 2\nwhere id = 3");
    expect(fmt("delete from t where id = 1")).toBe("delete\nfrom t\nwhere id = 1");
    expect(fmt("with a as (select 1 x), b as (select 2 y) select * from a"))
      .toBe("with a as (select 1 x),\n     b as (select 2 y)\nselect *\nfrom a");
    expect(fmt("select a from t union all select b from u")).toBe("select a\nfrom t\nunion all\nselect b\nfrom u");
    expect(fmt("insert into t (a) values (1) on duplicate key update a = 2, b = 3"))
      .toBe("insert into t (a)\nvalues (1)\non duplicate key update a = 2,\n                        b = 3");
  });

  it("CASE stays on its line with one WHEN; more, and each branch takes a line, END with them", () => {
    expect(fmt("select case when a = 1 then 'x' else 'z' end as c from t"))
      .toBe("select case when a = 1 then 'x' else 'z' end as c\nfrom t");
    expect(fmt("select case when a = 1 then 'x' when a = 2 then 'y' else 'z' end as c from t"))
      .toBe("select case\n           when a = 1 then 'x'\n           when a = 2 then 'y'\n           else 'z'\n           end as c\nfrom t");
  });

  it("spacing: none inside brackets or before a comma, a function on its bracket, a keyword or a table name spaced, a unary minus on its operand", () => {
    expect(fmt("select t.a,fn( x , y ), -1, a - -1 from t")).toBe("select t.a,\n       fn(x, y),\n       -1,\n       a - -1\nfrom t");
    expect(fmt("insert into t(a, b) values(1, 2)")).toBe("insert into t (a, b)\nvalues (1, 2)");
    expect(fmt("create index idx on t (a, b)")).toBe("create index idx on t (a, b)");
    expect(fmt("select * from t where x in (1,2) and exists(select 1)")).toBe("select *\nfrom t\nwhere x in (1, 2)\n  and exists (select 1)");
  });

  it("a comma list past 120 columns wraps under its bracket", () => {
    const ids = Array.from({ length: 10 }, (_, i) => String(i + 1).repeat(10)).join(", ");
    const out = fmt("select * from t where id in (" + ids + ")").split("\n");
    expect(out.length).toBe(4);
    expect(out[2]!.length).toBeLessThanOrEqual(120);
    expect(out[3]).toMatch(/^ {13}\d/);
  });

  it("statements split at the top-level semicolon; a blank line - where Run splits blocks - is kept, one", () => {
    expect(fmt("select 1; select 2;")).toBe("select 1;\nselect 2;");
    expect(fmt("SELECT\n  a,\n  b\nFROM t\nWHERE x = 1;\n\n\n\nUPDATE t SET a = 2 WHERE id = 1;"))
      .toBe("SELECT a,\n       b\nFROM t\nWHERE x = 1;\n\nUPDATE t\nSET a = 2\nWHERE id = 1;");
    expect(fmt("select a from t; -- end\nselect b from u")).toBe("select a\nfrom t; -- end\nselect b\nfrom u");
  });

  it("comments: a line comment ends its line, a comment on its own line keeps it", () => {
    expect(fmt("select 1 -- keep\n, 2")).toBe("select 1 -- keep\n       , 2");
    expect(fmt("select\n  a, -- first\n  b -- second\nfrom t")).toBe("select a, -- first\n       b -- second\nfrom t");
    expect(fmt("-- head\nselect a from t\n-- before where\nwhere x = 1")).toBe("-- head\nselect a\nfrom t\n-- before where\nwhere x = 1");
    expect(fmt("select a # mysql\nfrom t")).toBe("select a # mysql\nfrom t");
  });

  it("strings ride verbatim: inner semicolons, comment marks, blanks, backslash escapes", () => {
    expect(fmt("select 'a;b -- c  ' from t;")).toBe("select 'a;b -- c  '\nfrom t;");
    expect(fmt("select 'a\\'b', \"c\\\"d\" from t")).toBe("select 'a\\'b',\n       \"c\\\"d\"\nfrom t");
    expect(fmt("select a\nfrom t\nwhere x = 'multi\nline'")).toBe("select a\nfrom t\nwhere x = 'multi\nline'");
  });

  it("PostgreSQL: its operators, casts, parameters and dollar quotes", () => {
    expect(fmt("select data#>>'{a,b}', $1::int, j ? 'k', b[1] from t where x <= -1", { dialect: "pg" }))
      .toBe("select data #>> '{a,b}',\n       $1::int,\n       j ? 'k',\n       b[1]\nfrom t\nwhere x <= -1");
    expect(fmt("select $tag$ a;b $tag$, E'x\\'y' from t", { dialect: "pg" })).toBe("select $tag$ a;b $tag$,\n       E'x\\'y'\nfrom t");
    expect(fmt("insert into t (a) values (1) on conflict (a) do update set a = excluded.a returning id, a", { dialect: "pg" }))
      .toBe("insert into t (a)\nvalues (1)\non conflict (a) do update set a = excluded.a\nreturning id,\n          a");
  });

  it("DDL: create table puts a column a line; a statement of another kind keeps its lines; explain and create view lay out the query", () => {
    expect(fmt("create table t (id int primary key, name varchar(20) not null) engine=InnoDB"))
      .toBe("create table t\n(\n    id int primary key,\n    name varchar(20) not null\n) engine = InnoDB");
    expect(fmt("alter table t\n  add column x int,\n  drop column y")).toBe("alter table t\n  add column x int,\n  drop column y");
    expect(fmt("show   tables")).toBe("show tables");
    expect(fmt("explain select * from t where a = 1")).toBe("explain\nselect *\nfrom t\nwhere a = 1");
    expect(fmt("create view v as select a, b from t")).toBe("create view v as\nselect a,\n       b\nfrom t");
  });

  it("a selection mid-line: its first line stays where it stands, the rest line up under it", () => {
    expect(fmt("select a, b from t where x = 1", { indent: 4 }))
      .toBe("select a,\n           b\n    from t\n    where x = 1");
  });

  it("unbalanced brackets and stray words never lose a token; idempotent", () => {
    expect(fmt("select ((a + b)) * c from t where (x = 1")).toBe("select ((a + b)) * c\nfrom t\nwhere (x = 1");
    expect(fmt("select a) from t")).toBe("select a)\nfrom t");
    const messy = "select   id,name\n\nfrom t  where  x=1 and y = 2 order by\tid";
    const once = fmt(messy);
    expect(fmt(once)).toBe(once);
    expect(fmt("   \n ")).toBe("");
  });

  it("a big text formats in well under a second", () => {
    const rows = Array.from({ length: 5000 }, (_, i) => "(" + i + ", 'n " + i + "', coalesce(a, b, " + i + "))").join(", ");
    const t0 = Date.now();
    expect(fmt("insert into t (a, b, c) values " + rows).split("\n")).toHaveLength(5001);
    expect(Date.now() - t0).toBeLessThan(2000);
  });
});

describe("dbFormatSql — the running equivalence the format button promises", () => {
  const samples = [
    "SELECT id, name FROM users WHERE age > 21 AND name LIKE 'a%' ORDER BY id DESC LIMIT 10",
    "update  t set a = 1, b = 'x y'   where id = 3;",
    "select * from a left join b on a.id = b.id join c on b.x = c.x where a.q is null or c.r = 2",
    "insert into t (a, b) values (1, 2)",
    "select 1; select 'two; three'; -- tail comment\nselect 4",
    "with x as (select a, case when b then 1 when c then 2 end from t) select * from x join y using (id) union select 1",
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
