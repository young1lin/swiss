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

import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// @vitest-environment happy-dom

/* The MCP renderers sit in the browser module graph, whose entry modules wire a few DOM
   handles at import time. SPEC §panel.toolchain moved the builders onto real nodes, so the suite runs on
   a real DOM (happy-dom) with the served shell's id soup in place before the import — the same
   contract as the other view suites — instead of a permissive proxy the assertions could not
   read a built tree through. */
function shellSkeleton(): string {
  const html = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
let logs: any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let history: any;

beforeAll(async () => {
  const g = globalThis as unknown as Record<string, unknown>;
  g.window = { addEventListener() {}, innerWidth: 1440, innerHeight: 900 };
  g.location = { origin: "http://127.0.0.1:19999" };
  // happy-dom's localStorage is a getter: vitest 5 writes a global assignment through to
  // the window, where it throws, so the stub goes in the documented way.
  vi.stubGlobal("localStorage", { getItem: () => null, setItem() {} });
  g.fetch = async () => ({ ok: true, status: 200, json: async () => ({}) });
  document.body.innerHTML = shellSkeleton();
  logs = await import("../src/logs.js");
  history = await import("../src/run-history.js");
});

function loadedKind(items: unknown[]) {
  return {
    items,
    nextCursor: undefined,
    total: items.length,
    loaded: true,
    loading: false,
    error: null,
    cursors: [""],
    pageSize: 50,
    disabled: [],
  };
}

describe("MCP detail progressive disclosure", () => {
  it("keeps each tool description to one hoverable line and expands the complete metadata", () => {
    const description = "Runs a deliberately long database query description that should not turn the tool list into a wall of prose.";
    const d = {
      tools: loadedKind([{
        name: "mysql_query",
        description,
        inputSchema: {
          type: "object",
          required: ["sql"],
          properties: {
            sql: { type: "string", description: "The SQL statement to execute." },
            limit: { type: "integer", description: "Maximum rows to return." },
          },
        },
      }]),
    };
    const host = document.createElement("div");
    host.append(logs.kindBodyNode(d, "tools", { lifecycle: "started" }));

    // SPEC §panel.pages: the library row with a record behind it (row({ detail })).
    const detail = host.querySelector("details.lrow-disc")!;
    expect(detail).not.toBeNull();
    expect(detail.querySelector("summary .lrow-name")!.textContent).toBe("mysql_query");
    expect(detail.querySelector("summary .lrow-name code"), "a tool name is a value: mono").not.toBeNull();
    expect(detail.querySelector("summary .lrow-sub")!.textContent).toBe(description);
    expect(detail.closest(".lrow")!.getAttribute("title"), "the cut line reads whole on hover").toBe(description);
    const record = detail.querySelector(".lrow-detail")!;
    expect([...record.querySelectorAll(".vblock-cap")].map((n) => n.textContent)).toEqual(["Full description", "Input schema"]);
    expect(record.querySelector(".vblock-text")!.textContent).toBe(description);
    expect(record.querySelector("pre.jv")!.textContent, "the schema is the panel's one code block").toContain('"The SQL statement to execute."');
    const tryBtn = host.querySelector('[data-try="mysql_query"]')!;
    const toggle = host.querySelector('[data-toggle="mysql_query"]')!;
    expect(detail.contains(tryBtn) || detail.contains(toggle), "the row's controls never open the record").toBe(false);
    expect(toggle.getAttribute("role")).toBe("switch");
  });

  it("a prompt's record names its arguments - or says it has none - never an input schema", () => {
    const withArgs = document.createElement("div");
    withArgs.append(logs.kindBodyNode({ prompts: loadedKind([{ name: "summarize", description: "Sum up.", arguments: [{ name: "topic", required: true }] }]) }, "prompts", { lifecycle: "started" }));
    expect([...withArgs.querySelectorAll(".vblock-cap")].map((n) => n.textContent)).toEqual(["Full description", "Arguments"]);
    expect(withArgs.querySelector(".lrow-detail pre.jv")!.textContent).toContain('"topic"');
    expect(withArgs.querySelector("[data-try], [role=switch]"), "a prompt has no Try and no client switch").toBeNull();
    const bare = document.createElement("div");
    bare.append(logs.kindBodyNode({ prompts: loadedKind([{ name: "rules" }]) }, "prompts", { lifecycle: "started" }));
    expect([...bare.querySelectorAll(".vblock-text")].map((n) => n.textContent)).toEqual(["No description provided.", "no arguments"]);
  });

  it("turns read-only configuration into a short overview with every setting behind disclosure", () => {
    const host = document.createElement("div");
    host.append(...[history.configBodyNode({
      source: "config",
      editing: false,
      config: {
        type: "mysql",
        description: "Local orders database",
        host: "127.0.0.1",
        port: 3306,
        user: "root",
        password: "••••••••",
        database: "acme_app_dev",
        maxRows: 200,
        lazy: false,
      },
      revisions: [],
      tunnels: [],
    })].flat().filter((n): n is Node => n != null));

    expect(host.querySelector(".group > .config-summary")).not.toBeNull();
    // SPEC §panel.pages: every setting is a library label / value row, the value mono.
    expect(host.querySelector(".config-rows > .kv .kv-v.mono")).not.toBeNull();
    expect(host.querySelector(".config-target")!.textContent).toContain("127.0.0.1:3306 / acme_app_dev");
    expect(host.querySelector(".config-badges")).not.toBeNull();
    expect(host.querySelector(".config-badges")!.textContent).toContain("Starts at boot");
    expect(host.querySelector("details.config-more")).not.toBeNull();
    expect(host.querySelector("details.config-more > summary")!.textContent).toContain("All settings");
    expect(host.querySelector(".config-rows")!.textContent).toContain("Default row limit");
    expect(host.querySelector(".config-rows")!.textContent).toContain("••••••••");
    expect(host.querySelector("#c-edit")).not.toBeNull();
    expect(host.querySelector("#c-replace")).not.toBeNull();
  });

  it("summarizes each adapter shape without dropping its connection target", () => {
    const cases = [
      [{ type: "proc", command: "uvx mcp-server-git" }, "uvx mcp-server-git"],
      [{ type: "redis", host: "127.0.0.1", port: 6379, db: 2 }, "127.0.0.1:6379 / 2"],
      [{ type: "pg", url: "postgres://localhost/app" }, "postgres://localhost/app"],
      [{ type: "http", url: "https://example.test/mcp", auth: "oauth" }, "https://example.test/mcp"],
      [{ type: "rest", baseUrl: "https://api.example.test" }, "https://api.example.test"],
      [{ type: "figma", description: "Design MCP" }, "https://mcp.figma.com/mcp"],
      [{ type: "zai-vision", mode: "OPENAI" }, "OPENAI endpoint"],
    ] as const;

    for (const [config, target] of cases) {
      const host = document.createElement("div");
      host.append(...[history.configBodyNode({ source: "managed", editing: false, config, revisions: [], tunnels: [] })].flat().filter((n): n is Node => n != null));
      expect(host.querySelector(".config-target")!.textContent).toContain(target);
      expect(host.querySelector("details.config-more > summary")!.textContent).toContain("All settings");
      expect(host.querySelector("#c-edit")).not.toBeNull();
      expect(host.querySelector("#c-replace")).not.toBeNull();
    }
  });

  it("a database's host is masked in the summary and the settings, with no title to hover it out", () => {
    // redacted() (2026-09-28): the page gets screenshotted. The address is nowhere in the
    // markup - not text, not a title - until its eye is pressed; loopback and a public API's
    // domain stay plain.
    const cases = [
      [{ type: "mysql", host: "203.0.113.7", port: 3306, database: "app" }, "203.0.113.7", "••••••:3306 / app"],
      [{ type: "redis", host: "db.internal.test", port: 6379 }, "db.internal.test", "••••••:6379"],
      [{ type: "pg", url: "postgresql://app:***@198.51.100.4:5432/app" }, "198.51.100.4", "postgresql://app:***@••••••:5432/app"],
      [{ type: "http", url: "http://192.0.2.10:8080/mcp" }, "192.0.2.10", "http://••••••:8080/mcp"],
    ] as const;
    for (const [config, addr, shown] of cases) {
      const host = document.createElement("div");
      host.append(...[history.configBodyNode({ source: "managed", editing: false, config, revisions: [], tunnels: [] })].flat().filter((n): n is Node => n != null));
      expect(host.querySelector(".config-target")!.textContent, config.type).toBe(shown);
      expect(host.innerHTML, config.type).not.toContain(addr);
      expect(host.querySelector(".config-rows .redact-eye"), config.type).not.toBeNull();
    }
    for (const config of [{ type: "http", url: "https://api.example.test/mcp" }, { type: "mysql", host: "127.0.0.1", port: 3306 }]) {
      const host = document.createElement("div");
      host.append(...[history.configBodyNode({ source: "managed", editing: false, config, revisions: [], tunnels: [] })].flat().filter((n): n is Node => n != null));
      expect(host.querySelector(".redact-eye"), config.type).toBeNull();
    }
  });

  it("pins the one-line teaser and expanded readability in the shipped stylesheet", () => {
    const panel = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets");
    const css = readFileSync(join(panel, "styles", "views.css"), "utf8");
    const ui = readFileSync(join(panel, "styles", "ui.css"), "utf8");
    expect(ui).toMatch(/\.lrow-sub, \.lrow-err\s*{[^}]*overflow:\s*hidden;[^}]*text-overflow:\s*ellipsis;[^}]*white-space:\s*nowrap/);
    expect(ui).toMatch(/\.lrow-disc\[open\] \.lrow-sub\s*{[^}]*display:\s*none/);
    expect(css).toMatch(/\.config-target\s*{[^}]*text-overflow:\s*ellipsis/);
  });
});
