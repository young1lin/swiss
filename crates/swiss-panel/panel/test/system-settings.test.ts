/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */
// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import * as machineView from "../src/views/system.js";
import * as system from "../src/views/system.js";
import { install } from "../src/i18n.js";
import { setCurrentView } from "../src/ui-state.js";
import { assetsDir } from "./styles.js";

const writes: { path: string; enabled: boolean }[] = [];
let skillReads = 0;
const skillWrites: { name: string; root: string }[] = [];
const installedTargets = new Set<string>();
let installWait: Promise<void> | null = null;
let installFailure = false;
function skillReply(): Response {
  return new Response(JSON.stringify({ skills: ["swiss", "swiss-memory", "swiss-remote"].map(name => ({
    name, locations: [".claude", ".agents"].map(root => {
      const unreadable = name === "swiss" && root === ".agents";
      const installed = installedTargets.has(name + ":" + root) || (name === "swiss" && root === ".claude");
      return {
        root, path: "/user/" + root + "/skills/" + name + "/SKILL.md",
        installed: unreadable ? null : installed,
        ...(unreadable ? { error: "permission denied" } : {}),
      };
    }),
  })) }));
}
beforeEach(() => {
  install("en", null);
  setCurrentView("system");
  document.body.innerHTML = '<main id="pane"></main><div id="sheet" hidden></div><div id="toast"></div>';
  writes.length = 0; skillReads = 0;
  skillWrites.length = 0; installedTargets.clear(); installWait = null; installFailure = false;
  vi.stubGlobal("fetch", async (path: string, init?: RequestInit) => {
    if (path === "/api/skills/install") {
      const target = JSON.parse(String(init?.body)) as { name: string; root: string };
      skillWrites.push(target);
      if (installWait) await installWait;
      if (installFailure) return new Response(JSON.stringify({ error: "permission denied" }), { status: 500 });
      installedTargets.add(target.name + ":" + target.root);
      return skillReply();
    }
    if (path === "/api/skills") {
      skillReads++;
      return skillReply();
    }
    if (path === "/api/autostart" || path === "/api/user-path") {
      const enabled = init?.method === "PUT" ? Boolean(JSON.parse(String(init.body)).enabled) : false;
      if (init?.method === "PUT") writes.push({ path, enabled });
      return new Response(JSON.stringify({ enabled, detail: "machine registration", command: "swiss start", dir: "/bin" }));
    }
    return new Response("{}", { status: 404 });
  });
});
afterEach(() => { system.unmount(); vi.unstubAllGlobals(); });

describe("System machine settings and AI skills (SPEC §panel.settings)", () => {
  it("owns the startup and PATH controls, preserving their existing write contract", async () => {
    await system.mount();
    const startup = document.querySelector<HTMLButtonElement>("[data-autostart-toggle]");
    const path = document.querySelector<HTMLButtonElement>("[data-userpath-toggle]");
    expect(startup).not.toBeNull(); expect(path).not.toBeNull();
    startup?.click();
    await vi.waitFor(() => { expect(startup?.getAttribute("aria-checked")).toBe("true"); });
    expect(writes).toContainEqual({ path: "/api/autostart", enabled: true });
    path?.click();
    await vi.waitFor(() => { expect(path?.getAttribute("aria-checked")).toBe("true"); });
    expect(writes).toContainEqual({ path: "/api/user-path", enabled: true });
  });

  it("shows every skill's two locations and distinguishes missing from an unreadable path", async () => {
    await system.mount();
    const rows = document.querySelectorAll("[data-skill]");
    expect(rows).toHaveLength(3);
    const first = rows[0];
    expect(first?.textContent).toContain("swiss");
    expect(first?.textContent).toContain("Installed");
    expect(first?.textContent).toContain("Cannot check");
    expect(rows[1]?.textContent).toContain("Not installed");
    expect(first?.querySelector('[data-skill-root=".agents"]')?.getAttribute("title")).toContain("permission denied");
    expect(writes).toEqual([]);
    expect(skillReads).toBe(1);
  });

  it("refreshes the skill inspection on request without toggling machine settings", async () => {
    await system.mount();
    document.querySelector<HTMLButtonElement>("#system-skills-refresh")?.click();
    await vi.waitFor(() => { expect(skillReads).toBe(2); });
    expect(writes).toEqual([]);
  });

  it("ships visible sprite geometry for every skill installation button", async () => {
    await system.mount();
    const shell = document.createElement("template");
    shell.innerHTML = readFileSync(join(assetsDir, "index.html"), "utf8");
    const icons = document.querySelectorAll("[data-skill-install] use");
    expect(icons).toHaveLength(6);
    for (const icon of icons) {
      const target = icon.getAttribute("href") || "#missing";
      expect(shell.content.querySelector(target)?.children.length || 0, target).toBeGreaterThan(0);
    }
  });

  it("installs only the chosen skill and directory and offers updates for existing copies", async () => {
    await system.mount();
    expect(document.querySelectorAll("[data-skill-install]")).toHaveLength(6);
    const selector = '[data-skill-install="swiss-memory"][data-skill-directory=".agents"]';
    expect(document.querySelector(selector)?.getAttribute("aria-label")).toBe("Install swiss-memory in .agents");
    document.querySelector<HTMLButtonElement>(selector)?.click();
    await vi.waitFor(() => { expect(document.querySelector(selector)?.getAttribute("aria-label")).toBe("Update swiss-memory in .agents"); });
    expect(skillWrites).toEqual([{ name: "swiss-memory", root: ".agents" }]);
    expect(document.querySelector('[data-skill="swiss-remote"] [data-skill-root=".agents"]')?.textContent).toContain("Not installed");
    expect(writes).toEqual([]);
  });

  it("disables just the pending target and restores it after an installation failure", async () => {
    let finish: (() => void) | undefined;
    installWait = new Promise(resolve => { finish = resolve; });
    installFailure = true;
    await system.mount();
    const selector = '[data-skill-install="swiss-memory"][data-skill-directory=".agents"]';
    document.querySelector<HTMLButtonElement>(selector)?.click();
    expect(document.querySelector<HTMLButtonElement>(selector)?.disabled).toBe(true);
    expect(document.querySelector<HTMLButtonElement>('[data-skill-install="swiss-remote"][data-skill-directory=".agents"]')?.disabled).toBe(false);
    finish?.();
    await vi.waitFor(() => { expect(document.querySelector<HTMLButtonElement>(selector)?.disabled).toBe(false); });
    expect(document.querySelector(selector)?.getAttribute("aria-label")).toBe("Install swiss-memory in .agents");
    expect(document.querySelector("#toast")?.textContent).toContain("permission denied");
  });

  it("does not repaint another page when an installation finishes after navigation", async () => {
    let finish: (() => void) | undefined;
    installWait = new Promise(resolve => { finish = resolve; });
    await system.mount();
    document.querySelector<HTMLButtonElement>('[data-skill-install="swiss-memory"][data-skill-directory=".agents"]')?.click();
    system.unmount(); setCurrentView("mcp"); document.querySelector("#pane")!.textContent = "another page";
    finish?.();
    await vi.waitFor(() => { expect(installedTargets.size).toBe(1); });
    expect(document.querySelector("#pane")?.textContent).toBe("another page");
  });
});

/* Start-at-sign-in (the Plugins page's Startup section): the OS-level host setting rendered
   with the same row - name + one grey line + the panel's switch. */
describe("System start-at-sign-in row", () => {
  it("switches with the panel's switch and reflects the OS answer", () => {
    const row = machineView.startupRowNode({
      enabled: true,
      detail: "registry: HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
      command: '"C:/Program Files/swiss/swiss.exe" start --no-open',
    });
    expect(row).not.toBeNull();
    const sw = row?.querySelector("[data-autostart-toggle]");
    expect(sw?.getAttribute("role")).toBe("switch");
    expect(sw?.getAttribute("aria-checked")).toBe("true");
    expect(row?.textContent).toContain("registry: HKCU");
    // The title is the raw command, held as an attribute value - nothing to escape.
    expect(row?.getAttribute("title")).toBe('"C:/Program Files/swiss/swiss.exe" start --no-open');
    expect(row?.textContent).not.toContain("· off");
  });

  it("an off row is the switch checked off - no dot and no \"off\" word to repeat it", () => {
    const row = machineView.startupRowNode({ enabled: false, detail: "launch agent: /Users/a/Library/LaunchAgents/dev.swiss.swiss.plist", command: "c" });
    expect(row?.querySelector("[data-autostart-toggle]")?.getAttribute("aria-checked")).toBe("false");
    expect(row?.querySelector(".dot")).toBeNull();
    expect(row?.textContent).toBe("Start swiss when you sign inlaunch agent: /Users/a/Library/LaunchAgents/dev.swiss.swiss.plist");
  });

  it("renders nothing without an answer — an old gateway never grows a dead control", () => {
    expect(machineView.startupRowNode(null)).toBeNull();
  });
});

/* The PATH row: the startup row's twin over /api/user-path. */
describe("System PATH row", () => {
  it("says where the entry lives and titles the folder a terminal will find", () => {
    const row = machineView.pathRowNode({ enabled: true, detail: "registry: HKCU\\Environment (value: Path)", dir: "C:\\Users\\jdoe\\swiss" });
    const sw = row?.querySelector("[data-userpath-toggle]");
    expect(sw?.getAttribute("role")).toBe("switch");
    expect(sw?.getAttribute("aria-checked")).toBe("true");
    expect(sw?.getAttribute("aria-label")).toBe("Toggle swiss on your PATH");
    expect(row?.textContent).toBe("Put swiss on your PATHregistry: HKCU\\Environment (value: Path)");
    expect(row?.getAttribute("title")).toBe("C:\\Users\\jdoe\\swiss");
  });

  it("renders nothing without an answer", () => {
    expect(machineView.pathRowNode(null)).toBeNull();
  });
});
