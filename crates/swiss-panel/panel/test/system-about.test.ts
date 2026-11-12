/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */
// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { install } from "../src/i18n.js";
import { setGatewayInfo } from "../src/mcp-state.js";
import type { ApiBuildInfo, ApiInfoResponse } from "../src/types/api.js";
import { setCurrentView } from "../src/ui-state.js";
import * as system from "../src/views/system.js";
import zh from "../src/locales/zh.js";

const paths: string[] = [];
const wrote: string[] = [];
let failPlugins = false;
let updateReply: (() => Promise<Response>) | null = null;

const NAMES = [
  "acme_plugin", "Acme Database", "acme_off", "Acme Hidden",
  "acme_mcp", "acme_failed", "acme_idle", "acme_group",
  "mysql://acme:secret@db.example/app", "db.example",
  "/home/jdoe/acme", "ACME_TOKEN", "tok_acme_012345",
];

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status });
}

function stamp(build: ApiBuildInfo): void {
  const info: ApiInfoResponse = { tokenEnv: "ACME_TOKEN", panelVersion: "stamp-1", build };
  setGatewayInfo(info);
}

beforeEach(() => {
  install("en", null);
  setCurrentView("system");
  document.body.innerHTML = '<main id="pane"></main><div id="sheet" hidden></div><div id="toast"></div>';
  paths.length = 0;
  wrote.length = 0;
  failPlugins = false;
  updateReply = null;
  stamp({ hash: "abc1234", time: "2026-11-12T05:58:00Z", version: "0.2.2", target: "x86_64-pc-windows-msvc" });
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: (text: string) => { wrote.push(text); return Promise.resolve(); } },
  });
  vi.stubGlobal("fetch", async (path: string) => {
    paths.push(path);
    if (path === "/api/update/check") return updateReply ? updateReply() : jsonResponse(404, {});
    if (path === "/api/plugins") {
      if (failPlugins) return jsonResponse(500, { error: "plugins down" });
      return jsonResponse(200, {
        plugins: [
          { id: "acme_plugin", label: "Acme Database", enabled: true, token: "tok_acme_012345" },
          { id: "acme_off", label: "Acme Hidden", enabled: false },
        ],
      });
    }
    if (path === "/api/mcps") {
      return jsonResponse(200, {
        groups: ["acme_group"],
        mcps: [
          { name: "acme_mcp", state: "up", url: "mysql://acme:secret@db.example/app" },
          { name: "acme_failed", state: "error", path: "/home/jdoe/acme" },
          { name: "acme_idle", state: "idle" },
        ],
      });
    }
    if (path === "/api/memory") return jsonResponse(200, { gatewayMb: 41.2, host: "db.example" });
    if (path === "/api/skills") return jsonResponse(200, { skills: [] });
    if (path === "/api/autostart" || path === "/api/user-path") {
      return jsonResponse(200, { enabled: false, detail: "machine" });
    }
    return jsonResponse(404, {});
  });
});

afterEach(() => {
  system.unmount();
  setGatewayInfo(null);
  install("en", null);
  vi.unstubAllGlobals();
});

function updateHits(): number {
  return paths.filter((path) => path === "/api/update/check").length;
}

describe("Settings → System About", () => {
  it("does not ask about updates until the operator clicks", async () => {
    await system.mount();
    expect(document.querySelector("#system-about .sec-cap")?.textContent).toBe("About");
    expect(document.querySelector("#system-about-check")?.textContent).toBe("Check for updates");
    expect(updateHits()).toBe(0);
  });

  it("copies the version line and marks a dirty hash", async () => {
    stamp({ hash: "abc1234-dirty", time: "2026-11-12T05:58:00Z", version: "0.2.2", target: "x86_64-pc-windows-msvc" });
    await system.mount();
    const version = document.querySelector("[data-about-version]");
    expect(version?.querySelector(".lrow-name")?.textContent).toBe("swiss 0.2.2");
    expect(version?.querySelector("code")?.textContent).toBe("abc1234-dirty");
    expect(version?.textContent).toContain("built");
    const dirty = version?.querySelector(".tag");
    expect(dirty?.textContent).toBe("dirty");
    expect(dirty?.className).toContain("warn");
    document.querySelector<HTMLButtonElement>("[data-about-copy]")?.click();
    await vi.waitFor(() => { expect(wrote).toEqual(["swiss 0.2.2 (abc1234-dirty, 2026-11-12T05:58:00Z)"]); });
  });

  it("shows the hash when an older gateway sent no version", async () => {
    stamp({ hash: "abc1234", time: "2026-11-12T05:58:00Z" });
    await system.mount();
    const version = document.querySelector("[data-about-version]");
    expect(version?.querySelector(".lrow-name")?.textContent).toBe("swiss");
    expect(version?.querySelector(".tag")).toBeNull();
    document.querySelector<HTMLButtonElement>("[data-about-copy]")?.click();
    await vi.waitFor(() => { expect(wrote).toEqual(["swiss (abc1234, 2026-11-12T05:58:00Z)"]); });
  });

  it("shows each update outcome, and checks again from an end state", async () => {
    let hung = false;
    let finish: (response: Response) => void = () => {};
    const queued: Response[] = [];
    updateReply = () => {
      if (!hung) {
        hung = true;
        return new Promise((resolve) => { finish = resolve; });
      }
      return Promise.resolve(queued.shift() ?? jsonResponse(404, {}));
    };
    await system.mount();
    document.querySelector<HTMLButtonElement>("#system-about-check")?.click();
    expect(document.querySelector("[data-about-update]")?.getAttribute("data-about-update")).toBe("checking");
    expect(document.querySelector("#system-about .spin")).not.toBeNull();
    expect(document.querySelector<HTMLButtonElement>("#system-about-check")?.disabled).toBe(true);
    finish(jsonResponse(200, {
      current: "0.2.2", currentVersion: "0.2.2 (abc1234)", newer: true,
      latest: { tag: "v9.9.9", url: "https://example.com/swiss/releases/v9.9.9" },
    }));
    await vi.waitFor(() => {
      expect(document.querySelector("[data-about-update]")?.getAttribute("data-about-update")).toBe("newer");
    });
    const link = document.querySelector<HTMLAnchorElement>("#system-about a");
    expect(link?.textContent).toBe("Release notes");
    expect(link?.getAttribute("target")).toBe("_blank");
    expect(link?.getAttribute("rel")).toBe("noopener");
    expect(link?.getAttribute("href")).toBe("https://example.com/swiss/releases/v9.9.9");
    expect(document.querySelector("[data-about-update]")?.textContent).toContain("v9.9.9 is available");
    queued.push(jsonResponse(200, {
      current: "0.2.2", currentVersion: "0.2.2 (abc1234)", newer: false,
      latest: { tag: "v0.2.2", url: "https://example.com/swiss/releases/v0.2.2" },
    }));
    document.querySelector<HTMLButtonElement>("#system-about-check")?.click();
    await vi.waitFor(() => {
      expect(document.querySelector("[data-about-update]")?.textContent).toContain("You have the newest release");
    });
  });

  it("says when nothing has been published", async () => {
    updateReply = () => Promise.resolve(jsonResponse(200, {
      current: "0.2.2", currentVersion: "0.2.2 (abc1234)", latest: null, newer: false,
    }));
    await system.mount();
    document.querySelector<HTMLButtonElement>("#system-about-check")?.click();
    await vi.waitFor(() => {
      expect(document.querySelector("[data-about-update]")?.textContent).toContain("No releases published yet");
    });
  });

  it("shows the served refusal and retries", async () => {
    const queued = [
      jsonResponse(502, { error: "cannot reach github: offline", code: "update.unreachable" }),
      jsonResponse(200, { current: "0.2.2", currentVersion: "0.2.2 (abc1234)", latest: null, newer: false }),
    ];
    updateReply = () => Promise.resolve(queued.shift() ?? jsonResponse(404, {}));
    await system.mount();
    document.querySelector<HTMLButtonElement>("#system-about-check")?.click();
    await vi.waitFor(() => {
      expect(document.querySelector("[data-about-update]")?.textContent).toContain("The update server could not be reached.");
    });
    expect(document.querySelector("#system-about")?.textContent).not.toContain("cannot reach github");
    expect(document.querySelector("#system-about-check")?.textContent).toBe("Retry");
    document.querySelector<HTMLButtonElement>("#system-about-check")?.click();
    await vi.waitFor(() => {
      expect(document.querySelector("[data-about-update]")?.textContent).toContain("No releases published yet");
    });
  });

  it("removes the updates row after a 503 until the page is left", async () => {
    updateReply = () => Promise.resolve(jsonResponse(503, { error: "update check is not available in this composition" }));
    await system.mount();
    document.querySelector<HTMLButtonElement>("#system-about-check")?.click();
    await vi.waitFor(() => { expect(document.querySelector("#system-about-check")).toBeNull(); });
    expect(document.querySelector("[data-about-version]")).not.toBeNull();
    expect(updateHits()).toBe(1);
    await system.mount();
    expect(document.querySelector("#system-about-check")).toBeNull();
    expect(updateHits()).toBe(1);
    system.unmount();
    await system.mount();
    expect(document.querySelector("#system-about-check")?.textContent).toBe("Check for updates");
    expect(updateHits()).toBe(1);
  });

  it("copies counts and states and none of the fixture names", async () => {
    await system.mount();
    document.querySelector<HTMLButtonElement>("#system-about-diagnostics")?.click();
    await vi.waitFor(() => { expect(wrote.length).toBe(1); });
    const text = wrote[0] ?? "";
    const lines = text.split("\n");
    expect(lines).toEqual([
      "swiss 0.2.2 (abc1234, 2026-11-12T05:58:00Z)",
      "target: x86_64-pc-windows-msvc",
      "panel: stamp-1",
      "ui: en · " + navigator.userAgent,
      "plugins: 1 on, 1 off",
      "mcps: 3 (1 running, 1 failed)",
      "memory: 41.2 MB resident",
    ]);
    for (const name of NAMES) expect(text, name).not.toContain(name);
    expect(document.querySelector("#toast")?.textContent).toBe("Diagnostics copied");
  });

  it("keeps the other lines when one read fails", async () => {
    failPlugins = true;
    await system.mount();
    document.querySelector<HTMLButtonElement>("#system-about-diagnostics")?.click();
    await vi.waitFor(() => { expect(wrote.length).toBe(1); });
    const text = wrote[0] ?? "";
    expect(text).toContain("plugins: unavailable");
    expect(text).toContain("mcps: 3 (1 running, 1 failed)");
    expect(text).toContain("memory: 41.2 MB resident");
    expect(text).not.toContain("acme_mcp");
  });

  it("toasts when the clipboard refuses the diagnostics", async () => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: () => Promise.reject(new Error("denied")) },
    });
    document.execCommand = () => false;
    await system.mount();
    document.querySelector<HTMLButtonElement>("#system-about-diagnostics")?.click();
    await vi.waitFor(() => { expect(document.querySelector("#toast")?.textContent).toBe("Copy failed"); });
    expect(document.querySelector("#toast")?.className).toContain("err");
  });

  it("uses the Chinese copy", async () => {
    install("zh-CN", zh);
    await system.mount();
    expect(document.querySelector("#system-about .sec-cap")?.textContent).toBe("关于");
    expect(document.querySelector("#system-about-check")?.textContent).toBe("检查更新");
    expect(document.querySelector("#system-about-diagnostics")?.textContent).toBe("复制诊断信息");
  });
});
