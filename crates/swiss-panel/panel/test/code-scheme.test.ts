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

// @vitest-environment happy-dom

/* SPEC §panel.code - the code colour schemes and the token roles every printer shares. The
   scheme a preference resolves to is decided in three places (ui/code-scheme.ts and the two
   pages' pre-paint scripts), so the copies are run here against the library's answer; the SQL
   highlighter's roles are pinned on the shapes DataGrip paints without a schema - a DDL as
   SHOW CREATE TABLE prints it, a qualified column, a function, a PostgreSQL quoted name. */
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { CODE_SCHEMES, codeSchemeFor, tokenCodeNode, tokenSpans } from "../src/ui/index.js";
import { assetsDir, sheet } from "./styles.js";

/* The Data modules wire the shell's controls when they load: give them the shell's ids first. */
const shellIds = Array.from(readFileSync(join(assetsDir, "index.html"), "utf8").matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
document.body.innerHTML = Array.from(new Set(shellIds)).map((id) => '<div id="' + id + '"></div>').join("");
const { dbShellTokens, dbSqlTokens } = await import("../src/data-filters.js");

/** The roles a text got, as "class:text" for every classed token. */
function roles(tokens: [string, string][]): string[] {
  return tokens.filter((t) => t[0]).map((t) => t[0].slice(3) + ":" + t[1]);
}

describe("SPEC §panel.code - a preference resolves to one concrete scheme", () => {
  it("auto and github follow the theme; the IDE schemes stay what they are", () => {
    expect(CODE_SCHEMES).toEqual(["auto", "intellij-light", "darcula", "intellij-dark", "github"]);
    expect(codeSchemeFor("auto", "light")).toBe("intellij-light");
    expect(codeSchemeFor("auto", "dark")).toBe("intellij-dark");
    expect(codeSchemeFor(null, "dark")).toBe("intellij-dark");
    expect(codeSchemeFor("nonsense", "light")).toBe("intellij-light");
    expect(codeSchemeFor("github", "light")).toBe("github-light");
    expect(codeSchemeFor("github", "dark")).toBe("github-dark");
    for (const theme of ["light", "dark"] as const) {
      expect(codeSchemeFor("darcula", theme)).toBe("darcula");
      expect(codeSchemeFor("intellij-light", theme)).toBe("intellij-light");
    }
  });

  /** Run a page's inline pre-paint script against a stored preference and an OS theme; return
   *  what it put on <html>. */
  function prePaint(page: string, stored: Record<string, string>, osDark: boolean, search = ""): Record<string, string> {
    const html = readFileSync(join(assetsDir, page), "utf8");
    const script = /<script>([\s\S]*?)<\/script>/.exec(html)![1];
    const attrs: Record<string, string> = {};
    const doc = { documentElement: { setAttribute: (k: string, v: string) => { attrs[k] = v; }, lang: "en" } };
    const storage = { getItem: (k: string) => stored[k] ?? null };
    const loc = { search };
    const mm = () => ({ matches: osDark });
    new Function("document", "localStorage", "location", "window", "matchMedia", "URLSearchParams", script)(
      doc, storage, loc, { matchMedia: mm }, mm, URLSearchParams);
    return attrs;
  }

  it("both pre-paint scripts agree with codeSchemeFor on every preference and theme", () => {
    for (const page of ["index.html", "ui.html"]) {
      for (const pref of [...CODE_SCHEMES, "bogus"]) {
        for (const osDark of [false, true]) {
          const got = prePaint(page, { swiss_code: pref }, osDark);
          expect(got["data-code"], page + " " + pref + " dark=" + osDark).toBe(codeSchemeFor(pref, osDark ? "dark" : "light"));
        }
      }
      // An explicit light theme on a dark OS: the scheme follows the THEME, not the OS.
      expect(prePaint(page, { swiss_theme: "light", swiss_code: "github" }, true)["data-code"]).toBe("github-light");
    }
    // The gallery's ?code= wins over the panel's own preference.
    expect(prePaint("ui.html", { swiss_code: "github" }, false, "?code=darcula")["data-code"]).toBe("darcula");
  });

  it("every concrete scheme has its token block, and each block sets every code token", () => {
    const base = sheet("base.css");
    const tokens = ["--code-bg", "--code-fg", "--code-edge", "--code-gutter", "--code-sel", "--code-caret",
      "--syn-kw", "--syn-str", "--syn-num", "--syn-com", "--syn-fn", "--syn-key", "--syn-lit", "--syn-punct"];
    const block = (sel: string): string => {
      const at = base.indexOf(sel + " {");
      expect(at, sel).toBeGreaterThan(-1);
      return base.slice(at, base.indexOf("}", at));
    };
    // IntelliJ Light is the :root default - the scheme the page has before any data-code.
    const root = block(":root");
    for (const t of tokens) expect(root, ":root " + t).toContain(t + ":");
    for (const id of ["darcula", "intellij-dark", "github-light", "github-dark"]) {
      const b = block(':root[data-code="' + id + '"]');
      for (const t of tokens) expect(b, id + " " + t).toContain(t + ":");
    }
    // The dark theme no longer carries syntax colours: the scheme does.
    expect(block(':root[data-theme="dark"]')).not.toContain("--syn-");
  });
});

describe("SPEC §panel.code - the SQL highlighter's roles", () => {
  it("a DDL as SHOW CREATE TABLE prints it: keywords and types, columns, literals, the plain rest", () => {
    const ddl = "CREATE TABLE `users` (\n  `id` int NOT NULL AUTO_INCREMENT,\n  `name` varchar(64) DEFAULT NULL,\n" +
      "  `balance` decimal(12,2) DEFAULT '0.00',\n  PRIMARY KEY (`id`),\n  KEY `idx_city` (`city`)\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4";
    const r = roles(dbSqlTokens(ddl));
    expect(r).toEqual([
      "w:CREATE", "w:TABLE",
      "k:`id`", "w:int", "w:NOT", "l:NULL", "w:AUTO_INCREMENT",
      "k:`name`", "w:varchar", "n:64", "w:DEFAULT", "l:NULL",
      "k:`balance`", "w:decimal", "n:12", "n:2", "w:DEFAULT", "s:'0.00'",
      "w:PRIMARY", "w:KEY", "k:`id`",
      "w:KEY", "k:`city`",
      "w:ENGINE", "w:DEFAULT", "w:CHARSET",
    ]);
    // The table and the index keep the plain text colour, as DataGrip leaves them.
    expect(dbSqlTokens(ddl).find((t) => t[1] === "`users`")?.[0]).toBe("");
    expect(dbSqlTokens(ddl).find((t) => t[1] === "`idx_city`")?.[0]).toBe("");
  });

  it("a query: functions, a qualified column, comments, a table after its schema", () => {
    const r = roles(dbSqlTokens("-- recent\nSELECT count(*), u.name FROM app.users u WHERE u.joined > now() /* x */"));
    expect(r).toEqual(["c:-- recent", "w:SELECT", "f:count", "k:name", "w:FROM", "w:WHERE", "k:joined", "f:now", "c:/* x */"]);
    // INSERT's column list names columns; the table before it is not a call.
    expect(roles(dbSqlTokens("INSERT INTO t(a, b) VALUES (1, 'x')"))).toEqual(["w:INSERT", "w:INTO", "k:a", "k:b", "w:VALUES", "n:1", "s:'x'"]);
    expect(roles(dbSqlTokens("ALTER TABLE t ADD COLUMN c int"))).toEqual(["w:ALTER", "w:TABLE", "w:ADD", "w:COLUMN", "k:c", "w:int"]);
    expect(roles(dbSqlTokens("CREATE INDEX i ON users(city)"))).toEqual(["w:CREATE", "w:INDEX", "w:ON", "k:city"]);
  });

  it("a double-quoted run is a PostgreSQL identifier, and a MySQL string", () => {
    const sql = "CREATE TABLE \"public\".\"Users\" (\n  \"Id\" integer NOT NULL\n)";
    expect(roles(dbSqlTokens(sql, { dquoteIdent: true }))).toEqual(["w:CREATE", "w:TABLE", "k:\"Id\"", "w:integer", "w:NOT", "l:NULL"]);
    expect(roles(dbSqlTokens("SELECT \"x\"")).pop()).toBe("s:\"x\"");
  });

  it("every character survives the stream, a <script> included", () => {
    const sql = "SELECT '<script>alert(1)</script>' AS x; -- done\n";
    expect(dbSqlTokens(sql).map((t) => t[1]).join("")).toBe(sql);
    const pre = document.createElement("pre");
    pre.append(...tokenSpans(dbSqlTokens(sql)).nodes);
    expect(pre.querySelector("script")).toBeNull();
    expect(pre.textContent).toBe(sql);
  });

  it("the mongo console: keys, constructors, literals", () => {
    const r = roles(dbShellTokens("{ find: \"orders\", filter: { _id: ObjectId(\"65f0\"), paid: true, \"a.b\": 1 } } // c"));
    expect(r).toEqual(["k:find", "s:\"orders\"", "k:filter", "k:_id", "f:ObjectId", "s:\"65f0\"", "k:paid", "l:true", "k:\"a.b\"", "n:1", "c:// c"]);
  });
});

describe("SPEC §panel.code - numbered lines", () => {
  const sql = "SELECT 1\n\n  FROM t\n";

  it("one .jv-line per line, the blank one kept, the trailing break not numbered; a copy is the text", () => {
    const { nodes, lines } = tokenSpans(dbSqlTokens(sql), { lined: true });
    expect(lines).toBe(4);
    expect(nodes.length).toBe(3);
    expect(nodes.every((n) => (n as HTMLElement).className === "jv-line")).toBe(true);
    const pre = document.createElement("pre");
    pre.append(...nodes);
    expect(pre.textContent).toBe(sql);
    expect((nodes[1] as HTMLElement).textContent).toBe("\n");
  });

  it("a token that spans lines is split per line, keeping its class", () => {
    const { nodes } = tokenSpans([["jv-c", "/* a\nb */"], ["", "\n"], ["jv-w", "SELECT"]], { lined: true });
    expect(nodes.map((n) => (n as HTMLElement).querySelector(".jv-c, .jv-w")?.textContent)).toEqual(["/* a", "b */", "SELECT"]);
  });

  it("tokenCodeNode({ lined }) is the library block with a gutter; the cap still counts every line", () => {
    const many = Array.from({ length: 30 }, (_, i) => "SELECT " + i).join("\n");
    const p = tokenCodeNode(dbSqlTokens(many), { lined: true, limit: 10 });
    expect(p.node.matches("pre.jv")).toBe(true);
    expect(p.node.querySelectorAll(".jv-line").length).toBe(10);
    expect(p.lines).toBe(30);
  });

  it("the gutter is generated content, so a selection never copies a number", () => {
    const ui = sheet("ui.css");
    expect(ui).toMatch(/\.jv-line::before \{[^}]*content: counter\(jv-ln\)[^}]*user-select: none/);
    expect(ui).toMatch(/\.jv-line:first-child \{ counter-reset: jv-ln; \}/);
  });
});
