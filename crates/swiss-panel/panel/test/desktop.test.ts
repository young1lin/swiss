/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */
// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { desktopMode, initDesktop } from "../src/desktop.js";

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
});

describe("the desktop shell (SPEC §host.gui)", () => {
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
});
