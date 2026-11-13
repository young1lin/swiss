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
const skillWrites: { name: string; root?: string; dir?: string }[] = [];
const seatPosts: { kind?: string; dir?: string }[] = [];
const seatDeletes: { dir?: string }[] = [];
let refreshPosts = 0;
const installedTargets = new Set<string>();
let installWait: Promise<void> | null = null;
let installFailure = false;
let markOutdated = false;
let missingSeat = false;
let seatInvalid = false;
let installRefused = false;
let forgetRefused = false;
let brokenLedger = false;
function skillDocument(): { skills: unknown[]; seats?: unknown[]; ledger?: unknown; replaced?: string[]; failed?: string[] } {
  const skills = ["swiss", "swiss-memory", "swiss-remote"].map(name => ({
    name, locations: [".claude", ".agents"].map(root => {
      const outdated = markOutdated && name === "swiss" && root === ".agents";
      const unreadable = name === "swiss" && root === ".agents" && !outdated;
      const installed = outdated || installedTargets.has(name + ":" + root) || (name === "swiss" && root === ".claude");
      return {
        root, path: "/user/" + root + "/skills/" + name + "/SKILL.md",
        installed: unreadable ? null : installed,
        ...(outdated ? { state: "outdated" } : {}),
        ...(unreadable ? { error: "permission denied" } : {}),
      };
    }),
  }));
  const seats = missingSeat ? [
    { kind: "claude", dir: "/user/.claude/skills", source: "env", exists: true },
    { kind: "agents", dir: "/user/.agents/skills", source: "env", exists: true },
    { kind: "claude", dir: "/user/gone", source: "ledger", exists: false },
  ] : undefined;
  const ledger = brokenLedger
    ? { path: "/data/skill-seats.json", error: "skill seats at /data/skill-seats.json could not be read (expected value); the file was not changed" }
    : undefined;
  return { skills, ...(seats ? { seats } : {}), ...(ledger ? { ledger } : {}) };
}
function skillReply(): Response {
  return new Response(JSON.stringify(skillDocument()));
}
beforeEach(() => {
  install("en", null);
  setCurrentView("system");
  document.body.innerHTML = '<main id="pane"></main><div id="sheet" hidden></div><div id="toast"></div>';
  writes.length = 0; skillReads = 0;
  skillWrites.length = 0; seatPosts.length = 0; seatDeletes.length = 0; refreshPosts = 0;
  installedTargets.clear(); installWait = null; installFailure = false;
  markOutdated = false; missingSeat = false; seatInvalid = false;
  installRefused = false; forgetRefused = false; brokenLedger = false;
  vi.stubGlobal("fetch", async (path: string, init?: RequestInit) => {
    if (path === "/api/skills/install") {
      const target = JSON.parse(String(init?.body)) as { name: string; root?: string; dir?: string };
      skillWrites.push(target);
      if (installWait) await installWait;
      if (installRefused) {
        return new Response(JSON.stringify({ error: "unknown skill directory", code: "skills.unknownDir" }), { status: 400 });
      }
      if (installFailure) return new Response(JSON.stringify({ error: "permission denied" }), { status: 500 });
      installedTargets.add(target.name + ":" + (target.root || target.dir));
      return skillReply();
    }
    if (path === "/api/skills/refresh") {
      refreshPosts++;
      const body = skillDocument();
      body.replaced = ["/user/.agents/skills/swiss/SKILL.md"];
      body.failed = [];
      return new Response(JSON.stringify(body));
    }
    if (path === "/api/skills/seats" && init?.method === "DELETE") {
      seatDeletes.push(JSON.parse(String(init.body)) as { dir?: string });
      if (forgetRefused) {
        return new Response(JSON.stringify({ error: "default seat: /user/gone comes back from the environment", code: "skills.seatDefault" }), { status: 400 });
      }
      return skillReply();
    }
    if (path === "/api/skills/seats") {
      seatPosts.push(JSON.parse(String(init?.body)) as { kind?: string; dir?: string });
      if (seatInvalid) return new Response(JSON.stringify({ error: "not absolute", code: "skills.seatInvalid" }), { status: 400 });
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
    expect(rows).toHaveLength(6);
    expect(document.body.textContent).toContain("Installed");
    expect(document.body.textContent).toContain("Cannot check");
    expect(document.body.textContent).toContain("Not installed");
    expect(document.querySelector('[data-skill-root=".agents"]')?.getAttribute("title")).toContain("permission denied");
    expect(writes).toEqual([]);
    expect(skillWrites).toEqual([]);
    expect(seatPosts).toEqual([]);
    expect(refreshPosts).toBe(0);
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

  it("updates every outdated copy once, and only when asked", async () => {
    markOutdated = true;
    await system.mount();
    expect(refreshPosts).toBe(0);
    expect(document.body.textContent).toContain("1 outdated");
    document.querySelector<HTMLButtonElement>("#system-skills-update")?.click();
    await vi.waitFor(() => { expect(document.querySelector("#toast")?.textContent).toContain("Updated 1 copy"); });
    expect(document.querySelector("#toast")?.textContent).not.toContain("copies");
    expect(refreshPosts).toBe(1);
    expect(skillWrites).toEqual([]);
  });

  it("shows the served reason when a Claude directory cannot be added", async () => {
    seatInvalid = true;
    await system.mount();
    document.querySelector<HTMLButtonElement>("#system-skills-add")?.click();
    const input = document.querySelector<HTMLInputElement>("#g-name");
    expect(input).not.toBeNull();
    if (input) input.value = "relative/claude";
    document.querySelector<HTMLButtonElement>("#g-save")?.click();
    await vi.waitFor(() => { expect(document.querySelector("#g-err")?.textContent).toContain("not an absolute directory"); });
    expect(seatPosts[0]).toEqual({ kind: "claude", dir: "relative/claude" });
  });

  it("forgets a missing recorded directory and does not offer install there", async () => {
    missingSeat = true;
    await system.mount();
    const missing = document.querySelector("[data-skill-missing]");
    expect(missing?.textContent).toContain("folder missing");
    expect(missing?.querySelector("[data-skill-install]")).toBeNull();
    missing?.querySelector<HTMLButtonElement>("[data-skill-forget]")?.click();
    expect(document.querySelector("#skill-forget-confirm")).not.toBeNull();
    document.querySelector<HTMLButtonElement>("#skill-forget-confirm")?.click();
    await vi.waitFor(() => { expect(seatDeletes).toEqual([{ dir: "/user/gone" }]); });
  });

  it("says a refused install or Forget in the panel's words, not the server's English", async () => {
    installRefused = true;
    forgetRefused = true;
    missingSeat = true;
    await system.mount();
    document.querySelector<HTMLButtonElement>('[data-skill-install="swiss-memory"][data-skill-directory=".agents"]')?.click();
    await vi.waitFor(() => { expect(document.querySelector("#toast")?.textContent).toBe("That directory is not a known skill seat."); });
    document.querySelector<HTMLButtonElement>("[data-skill-missing] [data-skill-forget]")?.click();
    document.querySelector<HTMLButtonElement>("#skill-forget-confirm")?.click();
    await vi.waitFor(() => {
      expect(document.querySelector("#toast")?.textContent).toBe("A default skill directory comes back from the environment and cannot be forgotten.");
    });
    expect(document.querySelector("#toast")?.textContent).not.toContain("default seat:");
  });

  it("names a skill-seats file it could not read above the tables", async () => {
    brokenLedger = true;
    await system.mount();
    const line = document.querySelector("[data-skills-ledger]");
    expect(line?.textContent).toContain("/data/skill-seats.json");
    expect(line?.textContent).toContain("fix or remove it");
    expect(line?.getAttribute("title")).toContain("the file was not changed");
    expect(document.querySelectorAll("[data-skill]")).toHaveLength(6);
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
