/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */

/** The window shell's deliberately small bridge (SPEC §host.gui). */
import type { DesktopWindow } from "./types/dom.js"; // Native window bridge.

export function desktopMode(): boolean {
  const host = window as DesktopWindow;
  return host.__SWISS_DESKTOP__ === true && typeof host.ipc?.postMessage === "function";
}

export function initDesktop(): () => void {
  if (!desktopMode()) return () => { /* No listeners in a browser. */ };
  const host = window as DesktopWindow;
  const send = (message: string): void => { host.ipc?.postMessage(message); };
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
    if (!event.ctrlKey || event.altKey || event.metaKey || event.isComposing) return;
    const step = event.key === "+" || event.key === "=" ? "in" : event.key === "-" ? "out" : event.key === "0" ? "reset" : null;
    if (!step) return;
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
  };
}
