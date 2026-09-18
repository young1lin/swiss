/*
 * Copyright 2026 The swiss authors
 * 
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 * 
 *     https://www.apache.org/licenses/LICENSE-2.0
 * 
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

import type { ApiTerminalSessionRow, ApiTerminalTargets } from "../src/types/api.js";
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  FONT_DEFAULT, FONT_MAX, FONT_MIN, clampGeometry, configPutBody, embeddedNewlines, frameStatus,
  isPinned, keyAction, localShellLabel, mouseAction, nextFontSize, nextReconnectDelay,
  readBellMode, readCopyOnSelect, readFontSize, resizeFrame, sessionAlive, sessionLabel,
  streamUrl, tabLabel, targetRows, ticketUrl, trimSelection, wheelAction, withLocalConfig,
} from "../src/terminal-core.js";

/** The pure half of the terminal view (docs/14 §8): everything the page decides without
 *  a DOM is pinned here, because these ARE the wire contract's client side — URL shapes,
 *  reconnect pacing inside the 60 s grace, the close-frame stories, geometry clamping to
 *  the PTY bounds, and the target picker's honesty about an absent remote side. */

describe("terminal route URLs", () => {
  it("builds the stream URL absolute: the WebSocket constructor throws on a relative one", () => {
    // In the browser the page origin decides the scheme. This is the regression the first
    // real browser run caught: a relative URL made new WebSocket() throw a SyntaxError
    // before any connection was attempted, so a session opened server-side and never attached.
    const withLocation = (loc: { protocol: string; host: string }, fn: () => void) => {
      const prev = (globalThis as Record<string, unknown>).location;
      (globalThis as Record<string, unknown>).location = loc;
      try { fn(); } finally {
        if (prev === undefined) delete (globalThis as Record<string, unknown>).location;
        else (globalThis as Record<string, unknown>).location = prev;
      }
    };
    withLocation({ protocol: "http:", host: "127.0.0.1:19998" }, () => {
      expect(streamUrl("abc", "t+ok/")).toBe("ws://127.0.0.1:19998/api/terminal/sessions/abc/stream?ticket=t%2Bok%2F");
    });
    withLocation({ protocol: "https:", host: "gw.example" }, () => {
      expect(streamUrl("a b", "c")).toBe("wss://gw.example/api/terminal/sessions/a%20b/stream?ticket=c");
    });
  });
  it("returns the bare path outside a browser, keeping the helper pure for node callers", () => {
    expect(streamUrl("abc", "t")).toBe("/api/terminal/sessions/abc/stream?ticket=t");
  });
  it("nests the ticket mint route under the session", () => {
    expect(ticketUrl("s1")).toBe("/api/terminal/sessions/s1/ticket");
  });
});

describe("terminal reconnect pacing", () => {
  it("starts fast and doubles to a cap", () => {
    expect(nextReconnectDelay(0)).toBe(500);
    expect(nextReconnectDelay(1)).toBe(1000);
    expect(nextReconnectDelay(2)).toBe(2000);
    expect(nextReconnectDelay(3)).toBe(4000);
    expect(nextReconnectDelay(4)).toBe(5000);
    expect(nextReconnectDelay(50)).toBe(5000);
  });
  it("treats a nonsense attempt as the first one rather than NaN", () => {
    expect(nextReconnectDelay(-1)).toBe(500);
    expect(nextReconnectDelay(undefined as unknown as number)).toBe(500);
  });
});

describe("terminal control-frame stories", () => {
  it("narrates exit, error and stalled, and ignores everything else", () => {
    expect(frameStatus({ t: "exit", code: 0 })).toBe("the shell exited with code 0");
    expect(frameStatus({ t: "exit", code: 127 })).toBe("the shell exited with code 127");
    expect(frameStatus({ t: "exit", code: null })).toBe("the shell exited");
    expect(frameStatus({ t: "error", message: "closed: the terminal plugin is stopping" }))
      .toBe("closed: the terminal plugin is stopping");
    expect(frameStatus({ t: "error" })).toBe("closed");
    expect(frameStatus({ t: "stalled" })).toContain("stalled");
    expect(frameStatus({ t: "resize", cols: 1, rows: 2 } as Parameters<typeof frameStatus>[0])).toBeNull();
    expect(frameStatus("not json" as unknown as Parameters<typeof frameStatus>[0])).toBeNull();
    expect(frameStatus(null)).toBeNull();
  });
});

describe("terminal session liveness", () => {
  const listing = [{ id: "a" }, { id: "b" }];
  it("follows the listing, which is the truth after a socket drops", () => {
    expect(sessionAlive(listing, "a")).toBe(true);
    expect(sessionAlive(listing, "gone")).toBe(false);
    expect(sessionAlive([], "a")).toBe(false);
    expect(sessionAlive(null as unknown as [], "a")).toBe(false);
  });
});

describe("terminal geometry", () => {
  it("clamps both axes into the PTY bounds before anything is sent", () => {
    expect(clampGeometry(120, 30)).toEqual({ cols: 120, rows: 30 });
    expect(clampGeometry(0, 30)).toEqual({ cols: 1, rows: 30 });
    expect(clampGeometry(2001, 30)).toEqual({ cols: 1000, rows: 30 });
    expect(clampGeometry(80.6, 24.4)).toEqual({ cols: 81, rows: 24 });
  });
  it("falls back to the classics when fit has nothing to say", () => {
    expect(clampGeometry(undefined, undefined)).toEqual({ cols: 80, rows: 24 });
    expect(clampGeometry(NaN, NaN)).toEqual({ cols: 80, rows: 24 });
  });
  it("serializes the resize frame as the one control object the wire allows", () => {
    expect(JSON.parse(resizeFrame(120, 30))).toEqual({ t: "resize", cols: 120, rows: 30 });
    expect(JSON.parse(resizeFrame(0, 99999))).toEqual({ t: "resize", cols: 1, rows: 1000 });
  });
});

describe("terminal target picker rows", () => {
  it("offers local only when the config says so, with its program in the label", () => {
    expect(targetRows({ local: { enabled: false, shell: "pwsh" }, remote: { presence: "serving", targets: [] } } as unknown as ApiTerminalTargets).rows)
      .toEqual([]);
    const withLocal = targetRows({ local: { enabled: true, shell: "pwsh.exe" }, remote: { presence: "serving", targets: [] } } as unknown as ApiTerminalTargets);
    expect(withLocal.rows).toEqual([{ id: "local", label: "local · pwsh.exe" }]);
  });
  it("labels remote targets user@host, port only when it is not the default", () => {
    const { rows } = targetRows({
      remote: { presence: "serving", targets: [
        { id: "box", label: "the box", host: "box.example", port: 22, username: "dev", state: "connected" },
        { id: "alt", label: "alt", host: "alt.example", port: 2222, username: "", state: "connected" },
      ] },
    } as ApiTerminalTargets);
    expect(rows.map((r: { label: string }) => r.label)).toEqual([
      "the box · dev@box.example",
      "alt · alt.example:2222",
    ]);
  });
  it("carries the gateway's own reason when the remote side is absent", () => {
    const { rows, note } = targetRows({ remote: { presence: "absent", reason: "the tunnels plugin is disabled — enable it to reach remote hosts", targets: [] } } as unknown as ApiTerminalTargets);
    expect(rows).toEqual([]);
    expect(note).toBe("the tunnels plugin is disabled — enable it to reach remote hosts");
  });
  it("explains itself when there is simply nothing to open", () => {
    const { note } = targetRows({ remote: { presence: "serving", targets: [] } } as unknown as ApiTerminalTargets);
    expect(note).toContain("no terminal targets");
  });
  it("labels session tabs by target, local spelled out", () => {
    expect(sessionLabel({ target: "local" } as ApiTerminalSessionRow)).toBe("local");
    expect(sessionLabel({ target: "box" } as ApiTerminalSessionRow)).toBe("box");
    expect(sessionLabel(null)).toBe("?");
  });
  it("prefers the listing row's label over the raw target id", () => {
    // A remote tab without this showed the connection UUID; the listing carries the
    // human name ("jdoe-demo") and the tab must use it.
    expect(sessionLabel({ target: "8fb67a6e-f244-4241-a556-1ec72f81d5ad", label: "jdoe-demo" } as ApiTerminalSessionRow)).toBe("jdoe-demo");
  });
});

describe("terminal key bindings (Windows Terminal semantics, docs/15 §1)", () => {
  /** The duck-typed event keyAction takes: a plain object, not a KeyboardEvent, so a
   *  test can press any combination without a DOM — the same shape xterm hands the
   *  custom key handler. */
  type Ev = {
    type: string; key: string;
    ctrlKey?: boolean; shiftKey?: boolean; altKey?: boolean; metaKey?: boolean;
  };
  const down = (over: Partial<Ev>): Ev => ({
    type: "keydown", key: "", ctrlKey: false, shiftKey: false, altKey: false, metaKey: false,
    ...over,
  });

  it("Ctrl+V, Ctrl+Shift+V and Shift+Insert paste, selection or not", () => {
    // A terminal has no "replace the selection with the paste" concept; the paste just
    // lands at the cursor. Both selection states must paste.
    for (const sel of [false, true]) {
      expect(keyAction(down({ key: "v", ctrlKey: true }), sel)).toBe("paste");
      expect(keyAction(down({ key: "V", ctrlKey: true, shiftKey: true }), sel)).toBe("paste");
      expect(keyAction(down({ key: "Insert", shiftKey: true }), sel)).toBe("paste");
    }
  });
  it("Ctrl+C copies a selection and otherwise stays the interrupt", () => {
    // The one binding that must not drift: a terminal where Ctrl+C cannot send ^C is
    // broken, and one where a selection swallows the interrupt strands a flooding
    // program. Selection present -> copy (and no ^C); absent -> ^C.
    expect(keyAction(down({ key: "c", ctrlKey: true }), false)).toBe("sigint");
    expect(keyAction(down({ key: "c", ctrlKey: true }), true)).toBe("copy");
    expect(keyAction(down({ key: "C", ctrlKey: true, shiftKey: true }), true)).toBe("copy");
  });
  it("Ctrl+Shift+C and Ctrl+Insert are copy-only: with no selection they do nothing", () => {
    expect(keyAction(down({ key: "C", ctrlKey: true, shiftKey: true }), false)).toBeNull();
    expect(keyAction(down({ key: "Insert", ctrlKey: true }), false)).toBeNull();
    expect(keyAction(down({ key: "Insert", ctrlKey: true }), true)).toBe("copy");
  });
  it("leaves Alt+V, Cmd+V and non-keydown events alone", () => {
    // Alt combos belong to the browser and window menus, Cmd+V to a mac whose native
    // paste already works, and xterm hands keypress and keyup to the custom handler
    // too — deciding on those would double-fire every action.
    expect(keyAction(down({ key: "v", altKey: true }), false)).toBeNull();
    expect(keyAction(down({ key: "v", ctrlKey: true, altKey: true }), false)).toBeNull();
    expect(keyAction(down({ key: "v", metaKey: true }), false)).toBeNull();
    expect(keyAction({ ...down({ key: "v", ctrlKey: true }), type: "keyup" }, false)).toBeNull();
    expect(keyAction({ ...down({ key: "v", ctrlKey: true }), type: "keypress" }, false)).toBeNull();
    expect(keyAction(null, false)).toBeNull();
  });
});

describe("terminal font zoom (Windows Terminal keys)", () => {
  const down = (key: string, over: Record<string, boolean> = {}) => ({
    type: "keydown", key, ctrlKey: true, shiftKey: false, altKey: false, metaKey: false, ...over,
  });
  it("Ctrl+= / Ctrl++ zoom in, Ctrl+- / Ctrl+_ zoom out, Ctrl+0 resets", () => {
    // The shifted spellings are what the browser reports when Shift is held for the
    // "+" on a US layout, and "_" on the same key with Shift; both must count.
    expect(keyAction(down("="), false)).toBe("zoom-in");
    expect(keyAction(down("+", { shiftKey: true }), false)).toBe("zoom-in");
    expect(keyAction(down("-"), false)).toBe("zoom-out");
    expect(keyAction(down("_", { shiftKey: true }), false)).toBe("zoom-out");
    expect(keyAction(down("0"), false)).toBe("zoom-reset");
    // A selection changes nothing about zoom, and the bare keys stay typing.
    expect(keyAction(down("="), true)).toBe("zoom-in");
    expect(keyAction({ ...down("="), ctrlKey: false }, false)).toBeNull();
    expect(keyAction({ ...down("0"), ctrlKey: false }, false)).toBeNull();
  });
  it("steps one pixel at a time inside the bounds and resets to the default", () => {
    expect(nextFontSize(13, "zoom-in")).toBe(14);
    expect(nextFontSize(13, "zoom-out")).toBe(12);
    expect(nextFontSize(20, "zoom-reset")).toBe(FONT_DEFAULT);
    expect(nextFontSize(FONT_MAX, "zoom-in")).toBe(FONT_MAX);
    expect(nextFontSize(FONT_MIN, "zoom-out")).toBe(FONT_MIN);
  });
  it("reads a stored size back as an integer inside the bounds, or the default", () => {
    // localStorage hands back strings; a corrupt or absent value must never wedge the
    // terminal at 0px or NaN.
    expect(readFontSize("15")).toBe(15);
    expect(readFontSize(15.4)).toBe(15);
    expect(readFontSize(null)).toBe(FONT_DEFAULT);
    expect(readFontSize("")).toBe(FONT_DEFAULT);
    expect(readFontSize("abc")).toBe(FONT_DEFAULT);
    expect(readFontSize("0")).toBe(FONT_DEFAULT);
    expect(readFontSize("999")).toBe(FONT_DEFAULT);
    // A corrupt current value on the way in is also the default, then stepped.
    expect(nextFontSize("garbage", "zoom-in")).toBe(FONT_DEFAULT + 1);
  });
  it("Ctrl+wheel zooms by direction; a plain wheel is scrollback", () => {
    expect(wheelAction({ ctrlKey: true, deltaY: -100 })).toBe("zoom-in");
    expect(wheelAction({ ctrlKey: true, deltaY: 100 })).toBe("zoom-out");
    expect(wheelAction({ ctrlKey: false, deltaY: 100 })).toBeNull();
    expect(wheelAction({ ctrlKey: true, deltaY: 0 })).toBeNull();
    expect(wheelAction(null)).toBeNull();
  });
});

describe("terminal right-click (Windows Terminal semantics, docs/15 §1)", () => {
  it("pastes with no selection, copies one away, Shift keeps the browser menu", () => {
    const right = (shift: boolean) => ({ button: 2, shiftKey: shift });
    expect(mouseAction(right(false), false)).toBe("paste");
    expect(mouseAction(right(false), true)).toBe("copy");
    expect(mouseAction(right(true), false)).toBe("menu");
    expect(mouseAction(right(true), true)).toBe("menu");
    expect(mouseAction({ button: 0, shiftKey: false }, true)).toBeNull();
    expect(mouseAction({ button: 1, shiftKey: false }, false)).toBeNull();
  });
});

describe("local shell settings (docs/15 §2)", () => {
  it("labels the local row with the candidate's name when one matches", () => {
    const local = {
      enabled: true,
      shell: "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
      shells: [
        { program: "c:\\program files\\powershell\\7\\PWSH.EXE", label: "PowerShell 7" },
        { program: "C:\\Windows\\system32\\cmd.exe", label: "cmd" },
      ],
    };
    // Case and slashes differ on purpose: Windows paths are case-insensitive and a config
    // value may carry forward slashes.
    expect(localShellLabel(local)).toBe("PowerShell 7");
    expect(targetRows({ local, remote: { presence: "serving", targets: [] } }).rows)
      .toEqual([{ id: "local", label: "local · PowerShell 7" }]);
  });
  it("falls back to the file name when the shell is unknown or the list is absent", () => {
    expect(localShellLabel({ enabled: true, shell: "pwsh.exe" } as Parameters<typeof localShellLabel>[0])).toBe("pwsh.exe");
    expect(localShellLabel({ enabled: true, shell: "C:/Git/bin/bash.exe", shells: [] } as Parameters<typeof localShellLabel>[0])).toBe("bash.exe");
    expect(localShellLabel({ enabled: true } as unknown as Parameters<typeof localShellLabel>[0])).toBe("shell");
  });
  it("flags a switched-off local shell and keeps the remote reason separate", () => {
    const absent = targetRows({
      local: { enabled: false, shell: "pwsh.exe" },
      remote: { presence: "absent", reason: "the tunnels plugin is disabled", targets: [] },
    } as unknown as ApiTerminalTargets);
    expect(absent.rows).toEqual([]);
    expect(absent.localOff).toBe(true);
    expect(absent.reason).toBe("the tunnels plugin is disabled");
    expect(targetRows({ local: { enabled: false }, remote: { presence: "serving", targets: [] } } as unknown as ApiTerminalTargets).localOff).toBe(true);
    expect(targetRows({ remote: { presence: "serving", targets: [] } } as unknown as ApiTerminalTargets).localOff).toBe(false);
    expect(targetRows({ local: { enabled: true, shell: "pwsh.exe" }, remote: { presence: "serving", targets: [] } } as unknown as ApiTerminalTargets).localOff).toBe(false);
  });
  it("builds the save payload from the current config, touching only local", () => {
    const current = { maxSessions: 8, recording: false, local: { enabled: false, shell: "cmd.exe" } };
    const body = configPutBody(current, 12, true, "  pwsh.exe  ");
    expect(body.revision).toBe(12);
    expect(body.config.local).toEqual({ enabled: true, shell: "pwsh.exe" });
    expect(body.config.maxSessions).toBe(8);
    expect(body.config.recording).toBe(false);
    expect(current.local).toEqual({ enabled: false, shell: "cmd.exe" }); // the input is not mutated
    // An emptied shell field is omitted, not sent as "" — that is how "platform default"
    // stays expressible after a save.
    const cleared = withLocalConfig(current, false, "   ");
    expect(cleared.local).toEqual({ enabled: false });
    expect(cleared.maxSessions).toBe(8);
  });
});

describe("terminal tab labels (rename > shell title > target, docs/22 consensus 1)", () => {
  it("prefers a manual rename over the shell's title over the target", () => {
    const s = { target: "local", label: "Local shell" } as ApiTerminalSessionRow;
    expect(tabLabel(s, "vim ~/.bashrc", "my tab")).toBe("my tab");
    expect(tabLabel(s, "vim ~/.bashrc", null)).toBe("vim ~/.bashrc");
  });
  it("treats an empty shell title as a reset and falls back to the session label", () => {
    const s = { target: "local", label: "Local shell" } as ApiTerminalSessionRow;
    expect(tabLabel(s, "", null)).toBe("Local shell");
    expect(tabLabel(s, "   ", undefined)).toBe("Local shell");
    expect(tabLabel({ target: "box-one" } as ApiTerminalSessionRow, null, null)).toBe("box-one");
  });
});

describe("scroll pinning (Tabby's rule, docs/22 §2.10)", () => {
  it("rides the bottom within one line of the base", () => {
    expect(isPinned(10, 10)).toBe(true);
    expect(isPinned(9, 10)).toBe(true);   // baseY - 1: the wrap edge still counts as riding
    expect(isPinned(8, 10)).toBe(false);
  });
  it("counts unknown values as pinned", () => {
    // A wrongly-pinned terminal merely scrolls; a wrongly-unpinned one yanks scrollback.
    expect(isPinned(NaN, 10)).toBe(true);
    expect(isPinned(3, undefined)).toBe(true);
  });
});

describe("multiline paste risk", () => {
  it("ignores the one trailing Enter that ends every paste", () => {
    expect(embeddedNewlines("ls")).toBe(0);
    expect(embeddedNewlines("ls\n")).toBe(0);
  });
  it("counts each embedded newline, CRLF and lone CR included", () => {
    expect(embeddedNewlines("a\nb\n")).toBe(1);
    expect(embeddedNewlines("a\r\nb\r\n")).toBe(1);
    expect(embeddedNewlines("a\rb")).toBe(1);
    expect(embeddedNewlines("git pull\ngit push\n")).toBe(1);
    expect(embeddedNewlines("")).toBe(0);
    expect(embeddedNewlines(null)).toBe(0);
  });
});

describe("stored terminal preferences", () => {
  it("bell defaults to badge and only accepts the known modes", () => {
    expect(readBellMode(null)).toBe("badge");
    expect(readBellMode("garbage")).toBe("badge");
    expect(readBellMode("badge-sound")).toBe("badge-sound");
    expect(readBellMode("off")).toBe("off");
  });
  it("copy-on-select defaults on with 'off' as the escape hatch", () => {
    expect(readCopyOnSelect(null)).toBe(true);
    expect(readCopyOnSelect("off")).toBe(false);
    expect(readCopyOnSelect("nonsense")).toBe(true);
  });
  it("trims trailing whitespace from selections", () => {
    expect(trimSelection("ls -la  \nfoo\t\n")).toBe("ls -la\nfoo");
    expect(trimSelection("clean")).toBe("clean");
    expect(trimSelection(null)).toBe("");
  });
});

describe("terminal tab shortcuts (Alt-combos) and search (docs/22 P0/P1)", () => {
  const ev = (over: Record<string, unknown>) =>
    ({ type: "keydown", key: "1", ...over }) as Parameters<typeof keyAction>[0];
  it("Alt+digits jump, Alt+arrows cycle, Alt+W closes", () => {
    expect(keyAction(ev({ key: "3", altKey: true }), false)).toBe("tab-3");
    expect(keyAction(ev({ key: "ArrowLeft", altKey: true }), false)).toBe("tab-prev");
    expect(keyAction(ev({ key: "ArrowRight", altKey: true }), false)).toBe("tab-next");
    expect(keyAction(ev({ key: "w", altKey: true }), false)).toBe("tab-close");
  });
  it("keeps every other Alt combo and Ctrl-modified digits alone", () => {
    expect(keyAction(ev({ key: "v", altKey: true }), false)).toBeNull();
    expect(keyAction(ev({ key: "1", altKey: true, ctrlKey: true }), false)).toBeNull();
    expect(keyAction(ev({ key: "1", altKey: true, metaKey: true }), false)).toBeNull();
  });
  it("Ctrl+Shift+F is search; plain Ctrl+F stays the shell's own", () => {
    expect(keyAction(ev({ key: "f", ctrlKey: true, shiftKey: true }), false)).toBe("search");
    expect(keyAction(ev({ key: "f", ctrlKey: true }), false)).toBeNull();
  });
});

describe("the view's package contract (source-level)", () => {
  /** views/terminal.js is never imported by this suite - it needs a DOM - so the seam
   * between its load() mapping and the constructors wireTerminal news up is otherwise
   * only checked by a browser. It once shipped broken: the mapping said Unicode11 /
   * WebLinks while the code constructed got.Unicode11Addon / got.WebLinksAddon, and
   * every Open died on "not a constructor" with all five packages loaded. This pins
   * the two name sets against each other by reading the actual view source. */
  const view = readFileSync(
    fileURLToPath(new URL("../src/views/terminal.ts", import.meta.url)), "utf8"
  );
  // Only the load() mapping assigns from the Promise.all slots (got[N]); other
  // object returns in the view (tab state, session rows) must not be caught.
  const mapped = [...view.matchAll(/return \{ ([^}]*got\[\d+\][^}]*) \};/g)]
    .flatMap((m) => m[1].split(",")).filter((s) => /got\[\d+\]/.test(s))
    .map((s) => s.trim().split(":")[0].trim()).filter(Boolean);
  const constructed = [...new Set([...view.matchAll(/new got\.(\w+)\(/g)].map((m) => m[1]))];

  it("load() maps every constructor wireTerminal news up", () => {
    expect(constructed.length).toBeGreaterThanOrEqual(5);
    for (const name of constructed) expect(mapped).toContain(name);
  });
  it("maps nothing the view never constructs", () => {
    expect(mapped.sort()).toEqual([...constructed].sort());
  });
});

describe("the view's audit fixes stay fixed (source-level, fresh-eyes audit 2026-09-13)", () => {
  /** Four regressions a fresh-eyes audit caught in the 8aa6bbc layer, pinned the
   *  same way the package contract above is pinned: the view cannot run here (it
   *  needs a DOM), but the seam statements that carry each fix can be asserted
   *  textually. Removing any of these lines is a review flag, not a refactor. */
  const view = readFileSync(
    fileURLToPath(new URL("../src/views/terminal.ts", import.meta.url)), "utf8",
  );

  it("resets the paintTabs memo wherever the DOM changed outside paintTabs", () => {
    // B1a: render() rebuilt the pane (fresh empty bar); B1b: done() replaced a node
    // docs/37 M3: the memo moved from the function object (paintTabs.last) to a
    // module-scoped paintTabsLast - the reset contract is the same.
    const resets = view.match(/paintTabsLast = null/g) ?? [];
    expect(resets.length).toBeGreaterThanOrEqual(2);
    expect(view).toContain("if (html === paintTabsLast) return;");
  });

  it("keeps the page-level Ctrl+Shift+F out of the terminal's own key path", () => {
    // B2: xterm consults its custom handler without checking defaultPrevented, so a
    // document-capture call on terminal targets would double-fire and toggle shut
    expect(view).toContain('el.closest(".term-holder")) return;');
    expect(view).toContain('tag === "TEXTAREA"');
    // the guard above is only as good as the class name it matches - pin the writer too
    expect(view).toContain('className = "term-holder"');
    expect(view).toContain('tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA"');
  });

  it("suppresses copy-on-select while search navigation moves the selection", () => {
    // B3: the addon SELECTS every match; unguarded that is a clipboard overwrite per keystroke
    expect(view).toMatch(/m\.suppressSelect = true/);
    expect(view).toMatch(/finally \{ m\.suppressSelect = false; \}/);
    // the read side: deleting THIS check alone would bring the clipboard clobber back
    // while both pins above stay green - one-sided pins were audit suggestion 1
    expect(view).toContain("copyOnSelect || m.suppressSelect");
  });

  it("writes the mouse/bracket reset modes when the stream attaches", () => {
    // a stale client would otherwise replay ghost mouse reports onto the new host;
    // the literal write is the whole contract (audit suggestion 4: zero coverage)
    expect(view).toContain("\\x1b[?1000l");
    expect(view).toContain("\\x1b[?2004l");
  });

  it("carries the three guidance tiers (docs/22 P0 guidance layer)", () => {
    // tier 1: the empty state teaches the headline keys
    expect(view).toContain('<p class="term-keys-hint"><kbd>Ctrl+Shift+F</kbd>');
    // tier 2: one first-attach hint, stored so it never returns
    expect(view).toContain('const HINT_KEY = "swiss.terminal.hint"');
    expect(view).toContain('localStorage.setItem(HINT_KEY, "1")');
    // tier 3: the ? reference button in the bar, the sheet it opens, the key that opens it
    expect(view).toContain('id="term-help"');
    expect(view).toContain("help.onclick = openHelpSheet");
    expect(view).toContain('if (ev.key === "?")');
    expect(view).toContain('"Terminal shortcuts"');
  });

  it("draws the gear from the sprite, not a Unicode glyph", () => {
    expect(view).toContain('+ icon("gear") +');
    expect(view).not.toContain(">\u2699<");
  });

  it("nulls m.term after dispose so in-flight write callbacks hit the guard", () => {
    const nullings = view.match(/m\.term = null/g) ?? [];
    expect(nullings.length).toBeGreaterThanOrEqual(4);   // open-fail, close, releaseModels, unmount
  });
});
