/*
 * Copyright 2026 The swiss authors
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

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// @vitest-environment happy-dom

/* The MCP renderers sit in the browser module graph, whose entry modules wire a few DOM
   handles at import time. docs/37 R5 moved the builders onto real nodes, so the suite runs on
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
  g.localStorage = { getItem: () => null, setItem() {} };
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

    const detail = host.querySelector("details.item-detail")!;
    expect(detail).not.toBeNull();
    expect(detail.querySelector(".desc.item-teaser")).not.toBeNull();
    expect(detail.querySelector("summary")!.getAttribute("title")).toBe(description);
    expect(detail.querySelector(".item-full")).not.toBeNull();
    expect(detail.textContent).toContain("Full description");
    expect(detail.textContent).toContain("Input schema");
    expect(detail.textContent).toContain("The SQL statement to execute.");
    expect(host.querySelector('[data-try="mysql_query"]')).not.toBeNull();
    expect(host.querySelector('[data-toggle="mysql_query"]')).not.toBeNull();
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

    expect(host.querySelector(".group.config-summary")).not.toBeNull();
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

  it("pins the one-line teaser and expanded readability in the shipped stylesheet", () => {
    const panel = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets");
    const css = readFileSync(join(panel, "styles", "views.css"), "utf8");
    expect(css).toMatch(/\.item-teaser\s*{[^}]*white-space:\s*nowrap[^}]*text-overflow:\s*ellipsis/);
    expect(css).toMatch(/\.item-detail\[open\] \.item-teaser\s*{[^}]*display:\s*none/);
    expect(css).toMatch(/\.config-target\s*{[^}]*text-overflow:\s*ellipsis/);
  });
});
