// @vitest-environment happy-dom
/* Actual toolbar choices and received protobuf relays through the real link (SPEC §panel.i18n). */
import { afterAll, beforeAll, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { ConsolePeer } from "./console-bus-fixture.js";
import { fromBinary } from "../src/bus/codec.js";
import { Kind, Module } from "../src/bus/pb/swiss/bus/v1/bus_pb.js";
import { PanelSub, PrefsId, PrefsSchema } from "../src/bus/pb/swiss/bus/v1/panel_pb.js";
import { LANG_KEY, langPref, locale, toggleLang, tr } from "../src/i18n.js";
import { CODE_KEY, THEME_KEY } from "../src/util.js";
import { codeSchemeFor } from "../src/ui/code-scheme.js";

const pages = vi.hoisted(() => ({ navigate: vi.fn(async () => {}) }));
vi.mock("../src/page-registry.js", () => ({
  initPages: async () => {}, pageHasPendingChanges: () => false, pageUsesSidebar: () => false,
  pollPage: () => {}, navigatePage: pages.navigate,
}));
vi.mock("../src/polling.js", () => ({ loadMemory: async () => {}, refreshNow: () => {} }));
vi.mock("../src/views/tokens.js", () => ({ loadTokens: async () => {} }));

async function settle(): Promise<void> {
  await vi.dynamicImportSettled();
  for (let i = 0; i < 30; i++) await Promise.resolve();
}
function relays(peer: ConsolePeer) {
  return peer.sent.filter(f => f.kind === Kind.RELAY).map(f => {
    expect(f).toMatchObject({ module: Module.PANEL, sub: PanelSub.PREFS, id: PrefsId.PREFS_CHANGED, key: "" });
    return fromBinary(PrefsSchema, f.body);
  });
}
beforeAll(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  const html = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../src/admin_assets/index.html"), "utf8");
  const ids = [...new Set([...html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)].map(m => m[1]))];
  document.body.innerHTML = ids.map(id => '<div id="'+id+'"></div>').join("");
  document.getElementById("filter")!.outerHTML = '<input id="filter">';
  document.getElementById("themeBtn")!.innerHTML = '<svg><use></use></svg>';
  document.documentElement.setAttribute("data-theme", "light");
  vi.stubGlobal("WebSocket", ConsolePeer);
  vi.stubGlobal("fetch", async () => ({ ok: true, status: 200, json: async () => ({}) }));
  await import("../src/main.js");
  ConsolePeer.current.open();
  await settle();
});
afterAll(() => { vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); });

it("toolbar flips relay once; received choices store and repaint without echo, boot and reconnect stay quiet", async () => {
  let peer = ConsolePeer.current;
  expect(relays(peer)).toEqual([]);
  document.getElementById("themeBtn")!.click();
  expect(localStorage.getItem(THEME_KEY)).toBe("dark");
  expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
  expect(relays(peer)).toMatchObject([{ theme: "dark" }]);
  expect(relays(peer)[0].lang).toBeUndefined();
  expect(relays(peer)[0].codeScheme).toBeUndefined();

  document.getElementById("codeBtn")!.click();
  const darcula = [...document.querySelectorAll<HTMLButtonElement>("#menu button")].find(b => b.textContent === tr("main.codeDarcula"));
  if (!darcula) throw new Error("code scheme menu missing Darcula");
  darcula.click();
  expect(localStorage.getItem(CODE_KEY)).toBe("darcula");
  expect(document.documentElement.getAttribute("data-code")).toBe("darcula");
  expect(relays(peer)).toMatchObject([{ theme: "dark" }, { codeScheme: "darcula" }]);
  expect(relays(peer)[1].theme).toBeUndefined();
  expect(relays(peer)[1].lang).toBeUndefined();

  await toggleLang();
  expect(langPref()).toBe("zh-CN");
  expect(localStorage.getItem(LANG_KEY)).toBe("zh-CN");
  expect(locale()).toBe("zh-CN");
  expect(relays(peer)).toHaveLength(3);
  expect(relays(peer)[2]).toMatchObject({ lang: "zh-CN" });
  expect(relays(peer)[2].theme).toBeUndefined();
  expect(relays(peer)[2].codeScheme).toBeUndefined();

  peer.prefs({ lang: "en", theme: "light", codeScheme: "github" });
  await settle();
  expect(langPref()).toBe("en");
  expect(locale()).toBe("en");
  expect(document.documentElement.lang).toBe("en");
  expect(document.getElementById("filter")!.getAttribute("placeholder")).toBe("Search");
  expect(pages.navigate).toHaveBeenCalledTimes(2);
  expect(localStorage.getItem(THEME_KEY)).toBe("light");
  expect(localStorage.getItem(CODE_KEY)).toBe("github");
  expect(document.documentElement.getAttribute("data-theme")).toBe("light");
  expect(document.documentElement.getAttribute("data-code")).toBe(codeSchemeFor("github", "light"));
  expect(relays(peer)).toHaveLength(3);
  peer.prefs({ lang: "invalid", theme: "invalid", codeScheme: "invalid" });
  await settle();
  expect(langPref()).toBe("en");
  expect(localStorage.getItem(THEME_KEY)).toBe("light");
  expect(localStorage.getItem(CODE_KEY)).toBe("github");
  expect(relays(peer)).toHaveLength(3);

  peer.close();
  await vi.advanceTimersByTimeAsync(600);
  peer = ConsolePeer.current;
  peer.open();
  await settle();
  expect(relays(peer)).toEqual([]);
  expect(peer.subscriptions).toContain("");
  peer.prefs({ theme: "auto", codeScheme: "auto" });
  await settle();
  expect(localStorage.getItem(THEME_KEY)).toBe(null);
  expect(localStorage.getItem(CODE_KEY)).toBe(null);
  expect(relays(peer)).toEqual([]);
});

it("a newer language choice overtakes a delayed repaint without relaying the stale choice", async () => {
  const peer = ConsolePeer.current;
  let finish: (() => void) | undefined;
  pages.navigate.mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve; }));
  const older = toggleLang();
  await settle();
  expect(locale()).toBe("zh-CN");
  expect(relays(peer)).toEqual([]);
  await toggleLang();
  await toggleLang();
  expect(locale()).toBe("zh-CN");
  expect(relays(peer)).toMatchObject([{ lang: "en" }, { lang: "zh-CN" }]);
  if (!finish) throw new Error("first repaint not pending");
  finish();
  await older;
  expect(relays(peer)).toHaveLength(2);
});
