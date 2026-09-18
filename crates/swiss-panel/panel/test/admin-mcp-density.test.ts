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

// The MCP renderers sit in the browser module graph, whose entry modules wire a few DOM
// handles at import time. These tests exercise pure HTML builders, so a permissive stub is enough.
function permissive(): unknown {
  return new Proxy(function () {} as unknown as Record<string, unknown>, {
    get: () => permissive(),
    set: () => true,
    apply: () => permissive(),
  });
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
let logs: any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let history: any;

beforeAll(async () => {
  const g = globalThis as unknown as Record<string, unknown>;
  g.document = {
    addEventListener() {},
    getElementById: () => permissive(),
    createElement: () => permissive(),
    querySelector: () => null,
    querySelectorAll: () => [],
    body: permissive(),
    documentElement: permissive(),
    activeElement: null,
    hidden: false,
  };
  g.window = { addEventListener() {}, innerWidth: 1440, innerHeight: 900 };
  g.location = { origin: "http://127.0.0.1:19999" };
  g.localStorage = { getItem: () => null, setItem() {} };
  g.fetch = async () => ({ ok: true, status: 200, json: async () => ({}) });
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
    const html = logs.kindBody(d, "tools", { lifecycle: "started" });

    expect(html).toContain('<details class="item-detail">');
    expect(html).toContain('class="desc item-teaser"');
    expect(html).toContain('title="' + description + '"');
    expect(html).toContain('class="item-full"');
    expect(html).toContain("Full description");
    expect(html).toContain("Input schema");
    expect(html).toContain("The SQL statement to execute.");
    expect(html).toContain('data-try="mysql_query"');
    expect(html).toContain('data-toggle="mysql_query"');
  });

  it("turns read-only configuration into a short overview with every setting behind disclosure", () => {
    const html = history.configBody({
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
    });

    expect(html).toContain('class="group config-summary"');
    expect(html).toContain('class="config-target"');
    expect(html).toContain("127.0.0.1:3306 / acme_app_dev");
    expect(html).toContain('class="config-badges"');
    expect(html).toContain("Starts at boot");
    expect(html).toContain('<details class="config-more">');
    expect(html).toContain("All settings");
    expect(html).toContain("Default row limit");
    expect(html).toContain("••••••••");
    expect(html).toContain('id="c-edit"');
    expect(html).toContain('id="c-replace"');
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
      const html = history.configBody({ source: "managed", editing: false, config, revisions: [], tunnels: [] });
      expect(html).toContain(target);
      expect(html).toContain("All settings");
      expect(html).toContain('id="c-edit"');
      expect(html).toContain('id="c-replace"');
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
