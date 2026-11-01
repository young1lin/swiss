/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */

/** The window shell's deliberately small bridge (SPEC §host.gui). */
import type { DesktopWindow } from "./types/dom.js"; // Native window bridge.
import { toast } from "./util.js";
import { tr } from "./i18n.js";

export function desktopMode(): boolean {
  const host = window as DesktopWindow;
  return host.__SWISS_DESKTOP__ === true && typeof host.ipc?.postMessage === "function";
}

/** Ask the window shell to go with the gateway it was showing.
 *
 *  The panel's own Quit stops the gateway; a window cannot come back from that, so it would sit
 *  there showing a page that can never load again — and, worse, it holds the single-instance
 *  claim, so the next `swiss gui` could only raise the dead window (2026-11-01). A browser tab
 *  has no shell to close and keeps its "stopping" page, which is why this answers whether the
 *  message went anywhere. */
export function desktopQuit(): boolean {
  if (!desktopMode()) return false;
  (window as DesktopWindow).ipc?.postMessage("quit:");
  return true;
}

export function initDesktop(): () => void {
  if (!desktopMode()) return () => { /* No listeners in a browser. */ };
  const host = window as DesktopWindow;
  const send = (message: string): void => { host.ipc?.postMessage(message); };
  const download = (event: Event): void => {
    const detail: unknown = (event as CustomEvent<unknown>).detail;
    if (!detail || typeof detail !== "object" || !("success" in detail) || typeof detail.success !== "boolean") return;
    toast(tr(detail.success ? "desktop.downloadSaved" : "desktop.downloadFailed"));
  };
  window.addEventListener("swiss-download", download);
  document.documentElement.setAttribute("data-desktop", "true");
  let lastTheme = "";
  let lastTitle = "";
  const sync = (): void => {
    const theme = document.documentElement.getAttribute("data-theme") === "dark" ? "dark" : "light";
    const name = document.getElementById("pageTitle")?.textContent?.trim() || "";
    const title = name ? name + " · swiss" : "swiss";
    if (theme !== lastTheme) { send("theme:" + theme); lastTheme = theme; }
    if (title !== lastTitle) { send("title:" + title); lastTitle = title; }
  };
  const themeObserver = new MutationObserver(sync);
  themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  const titleObserver = new MutationObserver(sync);
  const title = document.getElementById("pageTitle");
  if (title) titleObserver.observe(title, { childList: true, characterData: true, subtree: true });
  const zoom = (event: KeyboardEvent): void => {
    // The modifier the platform actually uses: Cmd on a Mac, Ctrl everywhere else. This was
    // Ctrl-only, so every Mac user's Cmd+= did nothing at all and the app menu has no Zoom
    // item to fall back on (2026-11-01).
    const mac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
    const mod = mac ? event.metaKey : event.ctrlKey;
    if (!mod || event.altKey || event.isComposing) return;
    const step = event.key === "+" || event.key === "=" ? "in" : event.key === "-" ? "out" : event.key === "0" ? "reset" : null;
    if (!step) return;
    // The console owns these keys while it holds focus: it steps its own font size
    // (terminal-core's zoom-in/out/reset). This listener is on `document` in the CAPTURE
    // phase, so without this bail it ran first, preventDefault'ed, and stopped the event
    // before xterm's textarea ever saw it — the terminal's zoom was dead in the desktop app
    // while working in a browser tab (2026-11-01).
    if (event.target instanceof Element && event.target.closest(".xterm")) return;
    event.preventDefault();
    event.stopPropagation();
    send("zoom:" + step);
  };
  document.addEventListener("keydown", zoom, true);
  sync();
  return () => {
    themeObserver.disconnect();
    titleObserver.disconnect();
    document.removeEventListener("keydown", zoom, true);
    window.removeEventListener("swiss-download", download);
  };
}
