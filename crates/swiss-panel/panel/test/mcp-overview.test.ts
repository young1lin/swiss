/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */
// @vitest-environment happy-dom

import { beforeEach, describe, expect, it, vi } from "vitest";
import { mcpOverview, patchMcpOverview } from "../src/mcp-overview.js";
import { install } from "../src/i18n.js";
import zh from "../src/locales/zh.js";
import type { ApiMcpRow } from "../src/types/api.js";

const server = (name: string, state: string): ApiMcpRow => ({
  name, state, type: "proc", source: "config", tag: "npx", description: "A server", lifecycle: "started",
});
beforeEach(() => { install("en", null); document.body.replaceChildren(); });

describe("the desktop MCP landing page (SPEC §panel.pages)", () => {
  it("opens a server only on an explicit click and never starts idle servers by rendering", () => {
    const open = vi.fn(); const add = vi.fn();
    const view = mcpOverview([server("lazy", "idle"), server("local", "up")], open, add);
    document.body.append(view);
    expect(open).not.toHaveBeenCalled();
    const button = Array.from(view.querySelectorAll("button")).find(b => b.textContent === "Open");
    expect(button).toBeTruthy();
    button?.click();
    expect(open).toHaveBeenCalledWith("lazy");
    Array.from(view.querySelectorAll("button")).find(b => b.textContent?.startsWith("Add"))?.click();
    expect(add).toHaveBeenCalledOnce();
  });

  it("renders names as text and caps the overview while the sidebar keeps the complete list", () => {
    const rows = Array.from({length: 12}, (_, i) => server(i === 0 ? '<img src=x onerror=alert(1)>' : "server-" + i, "idle"));
    const view = mcpOverview(rows, vi.fn(), vi.fn());
    expect(view.textContent).toContain(rows[0].name);
    expect(view.querySelector("img")).toBeNull();
    expect(Array.from(view.querySelectorAll("button")).filter(b => b.textContent === "Open")).toHaveLength(6);
  });

  it("uses the Chinese dictionary for its heading and actions", () => {
    install("zh-CN", zh);
    const view = mcpOverview([server("local", "up")], vi.fn(), vi.fn());
    expect(view.textContent).toContain("MCP 工作区");
    expect(view.textContent).toContain("打开");
    expect(view.textContent).not.toContain("Open");
  });

  it("patches polled state without replacing a focused action or invoking a server", () => {
    const open = vi.fn();
    const view = mcpOverview([server("local", "idle")], open, vi.fn());
    document.body.append(view);
    const action = view.querySelector<HTMLElement>("[data-overview-server] button");
    action?.focus();
    expect(patchMcpOverview([server("local", "up")])).toBe(true);
    expect(document.activeElement).toBe(action);
    expect(view.querySelector("[data-overview-state]")?.getAttribute("title")).toBe("up");
    expect(open).not.toHaveBeenCalled();
  });

  it("hands an empty inventory back to the page so it can show the real empty state", () => {
    document.body.append(mcpOverview([server("last-server", "idle")], vi.fn(), vi.fn()));
    expect(patchMcpOverview([])).toBe(false);
  });
});
