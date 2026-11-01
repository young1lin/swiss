/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */
// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { desktopMode, desktopQuit, desktopTerminalMenu, initDesktop } from "../src/desktop.js";
import { toast } from "../src/util.js";
import { tr } from "../src/i18n.js";
import { closeMenu } from "../src/ui/menu.js";

vi.mock("../src/util.js", () => ({ toast: vi.fn() }));

const post = vi.fn();
let dispose = (): void => { /* Not mounted yet. */ };
const settle = async (): Promise<void> => { await Promise.resolve(); await Promise.resolve(); };

beforeEach(() => {
  post.mockClear();
  document.body.innerHTML = '<header><span id="pageTitle">MCP</span></header>';
  document.documentElement.setAttribute("data-theme", "dark");
  Object.assign(window, { __SWISS_DESKTOP__: true, ipc: { postMessage: post } });
});
afterEach(() => {
  dispose();
  Reflect.deleteProperty(window, "__SWISS_DESKTOP__");
  Reflect.deleteProperty(window, "ipc");
  document.documentElement.removeAttribute("data-desktop");
  closeMenu();
  vi.restoreAllMocks();
});

describe("the desktop shell (SPEC §host.gui)", () => {
  it("reports native download completion and releases the callback", () => {
    vi.mocked(toast).mockClear();
    dispose = initDesktop();
    window.dispatchEvent(new CustomEvent("swiss-download", { detail: { success: false } }));
    expect(toast).toHaveBeenLastCalledWith(tr("desktop.downloadFailed"));
    window.dispatchEvent(new CustomEvent("swiss-download", { detail: { success: true } }));
    expect(toast).toHaveBeenLastCalledWith(tr("desktop.downloadSaved"));
    dispose();
    vi.mocked(toast).mockClear();
    window.dispatchEvent(new CustomEvent("swiss-download", { detail: { success: false } }));
    expect(toast).not.toHaveBeenCalled();
  });
  it("sends the resolved theme and location immediately, then follows both", async () => {
    dispose = initDesktop();
    expect(document.documentElement.getAttribute("data-desktop")).toBe("true");
    expect(post).toHaveBeenCalledWith("theme:dark");
    expect(post).toHaveBeenCalledWith("title:MCP · swiss");
    document.documentElement.setAttribute("data-theme", "light");
    document.getElementById("pageTitle")!.textContent = "Data";
    await settle();
    expect(post).toHaveBeenCalledWith("theme:light");
    expect(post).toHaveBeenCalledWith("title:Data · swiss");
  });

  it("owns only the desktop zoom chords and releases its listeners on disposal", () => {
    dispose = initDesktop();
    for (const [key, message] of [["+", "zoom:in"], ["-", "zoom:out"], ["0", "zoom:reset"]]) {
      const event = new KeyboardEvent("keydown", { key, ctrlKey: true, bubbles: true, cancelable: true });
      document.dispatchEvent(event);
      expect(event.defaultPrevented).toBe(true);
      expect(post).toHaveBeenLastCalledWith(message);
    }
    post.mockClear();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "r", ctrlKey: true, bubbles: true }));
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "+", ctrlKey: true, altKey: true, bubbles: true }));
    expect(post).not.toHaveBeenCalled();
    dispose();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "+", ctrlKey: true, bubbles: true }));
    expect(post).not.toHaveBeenCalled();
  });

  it("closes the window with the gateway it was showing, and says nothing in a browser", () => {
    // The panel's Quit stops the gateway; a shell left standing after that can never load again
    // and holds the single-instance claim, so the next `swiss gui` could only raise it.
    expect(desktopQuit()).toBe(true);
    expect(post).toHaveBeenCalledWith("quit:");
    Reflect.deleteProperty(window, "__SWISS_DESKTOP__");
    expect(desktopQuit(), "a browser tab has no shell to close").toBe(false);
  });

  it("leaves the console's own font zoom alone, and takes Cmd on a Mac", () => {
    dispose = initDesktop();
    post.mockClear(); // init posts the theme and the title once; this test is about the keys
    // The terminal steps its own size from keydown on its textarea (terminal-core's zoom-in);
    // this listener runs in the CAPTURE phase, so without the bail it ate the event first.
    document.body.innerHTML = '<div class="xterm"><textarea></textarea></div>';
    const typed = new KeyboardEvent("keydown", { key: "+", ctrlKey: true, bubbles: true, cancelable: true });
    document.querySelector("textarea")!.dispatchEvent(typed);
    expect(typed.defaultPrevented, "the console keeps its own zoom").toBe(false);
    expect(post).not.toHaveBeenCalled();
    // A Mac's modifier is Cmd, and the application menu has no Zoom item to fall back on.
    const platform = Object.getOwnPropertyDescriptor(window.navigator, "platform");
    Object.defineProperty(window.navigator, "platform", { value: "MacIntel", configurable: true });
    const cmd = new KeyboardEvent("keydown", { key: "=", metaKey: true, bubbles: true, cancelable: true });
    document.dispatchEvent(cmd);
    expect(cmd.defaultPrevented).toBe(true);
    expect(post).toHaveBeenLastCalledWith("zoom:in");
    if (platform) Object.defineProperty(window.navigator, "platform", platform);
    else Reflect.deleteProperty(window.navigator as unknown as Record<string, unknown>, "platform");
  });

  it("does not enable desktop styling or intercept keys in an ordinary browser", () => {
    Reflect.deleteProperty(window, "__SWISS_DESKTOP__");
    expect(desktopMode()).toBe(false);
    dispose = initDesktop();
    const event = new KeyboardEvent("keydown", { key: "+", ctrlKey: true, cancelable: true });
    document.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(false);
    expect(document.documentElement.hasAttribute("data-desktop")).toBe(false);
    expect(post).not.toHaveBeenCalled();
  });

  it("offers desktop editing actions while leaving a page's own context menu in control", () => {
    dispose = initDesktop();
    document.body.insertAdjacentHTML("beforeend", '<input value="hello"><div id="custom">custom</div>');
    const input = document.querySelector("input")!;
    input.focus();
    input.setSelectionRange(0, 5);
    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2 });
    input.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    const labels = document.getElementById("menu")!.textContent!;
    expect(labels).toContain(tr("desktop.copy"));
    expect(labels).toContain(tr("desktop.paste"));
    expect(labels).toContain(tr("desktop.selectAll"));
    document.getElementById("menu")!.remove();
    const custom = document.getElementById("custom")!;
    custom.addEventListener("contextmenu", (ev) => { ev.preventDefault(); });
    custom.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2 }));
    expect(document.getElementById("menu")).toBeNull();
  });

  it("keeps ordinary browser context menus untouched", () => {
    Reflect.deleteProperty(window, "__SWISS_DESKTOP__");
    dispose = initDesktop();
    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2 });
    document.body.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(false);
    expect(document.getElementById("menu")).toBeNull();
  });

  it("pastes into the original input selection and never offers password copying", async () => {
    dispose = initDesktop();
    document.body.insertAdjacentHTML("beforeend", '<input value="hello"><input type="password" value="secret">');
    const input = document.querySelector("input")!;
    input.focus();
    input.setSelectionRange(1, 4);
    const readText = vi.spyOn(navigator.clipboard, "readText").mockResolvedValue("X");
    const changed = vi.fn();
    input.addEventListener("input", changed);
    input.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2 }));
    const paste = Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button")).find((b) => b.textContent === tr("desktop.paste"))!;
    paste.click();
    await settle();
    expect(readText).toHaveBeenCalledOnce();
    expect(document.activeElement).toBe(input);
    expect(input.value).toBe("hXo");
    expect(changed).toHaveBeenCalledOnce();
    const password = document.querySelector<HTMLInputElement>('input[type="password"]')!;
    password.focus();
    password.select();
    password.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2 }));
    expect(document.getElementById("menu")!.textContent).not.toContain(tr("desktop.copy"));
    expect(document.getElementById("menu")!.textContent).not.toContain(tr("desktop.cut"));
  });

  it("offers the terminal's own selection and paste actions for its desktop menu", async () => {
    const copy = vi.fn(); const paste = vi.fn(); const focus = vi.fn();
    vi.spyOn(navigator.clipboard, "readText").mockResolvedValue("terminal text");
    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    document.body.dispatchEvent(event);
    desktopTerminalMenu(event, { hasSelection: true, copy, paste, focus });
    expect(event.defaultPrevented).toBe(true);
    let buttons = Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button"));
    buttons.find((b) => b.textContent === tr("desktop.copy"))!.click();
    expect(copy).toHaveBeenCalledOnce();
    desktopTerminalMenu(event, { hasSelection: false, copy, paste, focus });
    buttons = Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button"));
    expect(buttons.find((b) => b.textContent === tr("desktop.copy"))!.disabled).toBe(true);
    buttons.find((b) => b.textContent === tr("desktop.paste"))!.click();
    await settle();
    expect(paste).toHaveBeenCalledWith("terminal text");
    expect(focus).toHaveBeenCalledTimes(2);
  });

  it("offers link actions and copies the link's address", async () => {
    dispose = initDesktop();
    document.body.insertAdjacentHTML("beforeend", '<a href="https://example.test/path">link</a>');
    const write = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();
    const link = document.querySelector("a")!;
    link.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2 }));
    const buttons = Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button"));
    expect(buttons.map((b) => b.textContent)).toContain(tr("desktop.openLink"));
    buttons.find((b) => b.textContent === tr("desktop.copyLink"))!.click();
    await settle();
    expect(write).toHaveBeenCalledWith("https://example.test/path");
  });
});
