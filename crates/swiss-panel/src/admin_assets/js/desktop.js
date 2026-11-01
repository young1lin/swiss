/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */

/** The window shell's deliberately small bridge (SPEC §host.gui). */
                                                     // Native window bridge.
import { toast } from "./util.js";
import { tr } from "./i18n.js";
import { popupMenu } from "./ui/menu.js";
                                             

/** The terminal owns its selection and paste API rather than DOM text selection. */
export function desktopTerminalMenu(event            , actions   
                                                                                            
 )       {
  event.preventDefault();
  const target = event.target instanceof Element ? event.target : document.body;
  const rect = target.getBoundingClientRect();
  const x = event.clientX || rect.left;
  const y = event.clientY || rect.bottom;
  popupMenu({ left: x, top: y, bottom: y }, [
    { label: tr("desktop.copy"), disabled: !actions.hasSelection, fn: () => { actions.copy(); actions.focus(); } },
    { label: tr("desktop.paste"), fn: () => {
      void navigator.clipboard.readText().then((text) => { actions.paste(text); actions.focus(); }).catch(() => {
        toast(tr("desktop.clipboardFailed"), true); actions.focus();
      });
    } },
  ]);
}

/** Only handle context menus the current page did not already claim. */
function desktopContextMenu(event            )       {
  if (event.defaultPrevented || !(event.target instanceof Element)) return;
  const target = event.target;
  const input = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement ? target : null;
  const editable = target.closest             ('[contenteditable=""], [contenteditable="true"], [contenteditable="plaintext-only"]');
  const writable = input ? !input.readOnly && !input.disabled : !!editable;
  const start = input?.selectionStart ?? 0;
  const end = input?.selectionEnd ?? 0;
  const selection = window.getSelection();
  const range = selection?.rangeCount ? selection.getRangeAt(0).cloneRange() : null;
  const selected = input ? (input.type === "password" ? "" : input.value.slice(start, end)) : (selection?.toString() || "");
  const focus = ()       => {
    if (input) {
      input.focus();
      if (input.selectionStart !== null) input.setSelectionRange(start, end);
    } else if (editable) {
      editable.focus();
      if (range && selection) { selection.removeAllRanges(); selection.addRange(range); }
    }
  };
  const failed = ()       => { toast(tr("desktop.clipboardFailed"), true); };
  const insert = (text        )       => {
    focus();
    if (typeof document.execCommand === "function" && document.execCommand("insertText", false, text)) return;
    if (input && input.selectionStart !== null) {
      input.setRangeText(text, start, end, "end");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    } else failed();
  };
  const items             = [];
  if (selected && writable) items.push({ label: tr("desktop.cut"), fn: () => {
    void navigator.clipboard.writeText(selected).then(() => { insert(""); }).catch(failed);
  } });
  if (selected) items.push({ label: tr("desktop.copy"), fn: () => { void navigator.clipboard.writeText(selected).catch(failed); } });
  if (writable) items.push({ label: tr("desktop.paste"), fn: () => { void navigator.clipboard.readText().then(insert).catch(failed); } });
  if (input || editable) items.push({ label: tr("desktop.selectAll"), fn: () => {
    focus();
    if (input && input.selectionStart !== null) input.select();
    else document.execCommand("selectAll");
  } });
  const link = target.closest                   ("a[href]");
  if (link && /^https?:$/.test(link.protocol)) {
    if (items.length) items.push({ sep: true });
    items.push({ label: tr("desktop.openLink"), fn: () => { window.open(link.href, "_blank", "noopener"); } });
    items.push({ label: tr("desktop.copyLink"), fn: () => { void navigator.clipboard.writeText(link.href).catch(failed); } });
  }
  event.preventDefault();
  if (!items.length) return;
  const rect = target.getBoundingClientRect();
  const x = event.clientX || rect.left;
  const y = event.clientY || rect.bottom;
  popupMenu({ left: x, top: y, bottom: y }, items);
}

export function desktopMode()          {
  const host = window                 ;
  return host.__SWISS_DESKTOP__ === true && typeof host.ipc?.postMessage === "function";
}

/** Ask the window shell to go with the gateway it was showing.
 *
 *  The panel's own Quit stops the gateway; a window cannot come back from that, so it would sit
 *  there showing a page that can never load again — and, worse, it holds the single-instance
 *  claim, so the next `swiss gui` could only raise the dead window (2026-11-01). A browser tab
 *  has no shell to close and keeps its "stopping" page, which is why this answers whether the
 *  message went anywhere. */
export function desktopQuit()          {
  if (!desktopMode()) return false;
  (window                 ).ipc?.postMessage("quit:");
  return true;
}

export function initDesktop()             {
  if (!desktopMode()) return () => { /* No listeners in a browser. */ };
  const host = window                 ;
  const send = (message        )       => { host.ipc?.postMessage(message); };
  const download = (event       )       => {
    const detail          = (event                        ).detail;
    if (!detail || typeof detail !== "object" || !("success" in detail) || typeof detail.success !== "boolean") return;
    toast(tr(detail.success ? "desktop.downloadSaved" : "desktop.downloadFailed"));
  };
  window.addEventListener("swiss-download", download);
  document.documentElement.setAttribute("data-desktop", "true");
  let lastTheme = "";
  let lastTitle = "";
  const sync = ()       => {
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
  const zoom = (event               )       => {
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
  document.addEventListener("contextmenu", desktopContextMenu);
  sync();
  return () => {
    themeObserver.disconnect();
    titleObserver.disconnect();
    document.removeEventListener("keydown", zoom, true);
    document.removeEventListener("contextmenu", desktopContextMenu);
    window.removeEventListener("swiss-download", download);
  };
}
