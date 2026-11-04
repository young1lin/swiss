/*
 * Copyright 2026 young1lin
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

// @vitest-environment happy-dom

/* SPEC §panel.ui - the ui/ component library, one describe per module. Each pins the markup a
   page and the stylesheet agree on (class, role, aria, data hooks) and the option that
   changes it, so a component cannot drift from ui.css or from the delegated listeners that
   address it. The last block renders every component with every option and checks each class
   it drew is styled by base.css or ui.css - the two sheets the gallery links - so a typo in a
   class name fails here instead of rendering as an unstyled box. */
import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { URL as NodeURL } from "node:url";
import { h } from "../src/h.js";
import { install, tr } from "../src/i18n.js";
import zh from "../src/locales/zh.js";
import {
  checkField as checkFieldFn, field as fieldFn, form as formFn, formActions, formCap as formCapFn, formFold as formFoldFn, hint as hintFn,
  pair, stackSheet,
} from "../src/ui/index.js";
import {
  anchoredMenu, btn, card, closeMenu, closeSheet, collapseRuns, dayLabel, decodeStrings, dot, emptyNode, failNote, filterInput,
  ACTION_ICON, actionBtn, fmtMs, groupNode, heldDot, iconBtn, iconNode, inlineForm, jsonCodeNode, kvRow, menuOpen, moreBtn, note, openFieldSheet, pageFoot,
  setActionBtn, setActionBusy,
  objTab, pager, paneBody, paneHead, pickPanel, pickerRow, portChip, popupMenu, resHead, row, section, seg, sheet as sheetFrame, spinner, styleSelect, sw, tag,
  relTime, speaker, timeLabel, timeline, timelineMeta, timelineToggle, toTop, transcript, transcriptText, transcriptTurn, toolChip, valueBlock,
} from "../src/ui/index.js";
import {
  catChip, docHead, dotSize, graphElements, graphHops, graphLayoutOf, graphLegend, graphPalette, graphStyle, graphView, groupElementId, markdownNode,
  mixHex, packBoxes, pickList, pickRow, pickSelect, refLink, sideRow, workspace, wsBar, wsHead, wsScroll, wsSide, wsSplit,
} from "../src/ui/index.js";
import { chatFollow, chatPane, chatQueue, chatStats, copyBtn, copyText, dock, dockShow, grip, gripFold, gripFolded, gripState } from "../src/ui/index.js";
import { diffFiles, diffNode, tokenCodeNode } from "../src/ui/index.js";
import { resultTable, resultChart } from "../src/ui/index.js";
import type { GraphEdge, GraphEls, GraphGroup, GraphNode, GraphSpec, TimelineItem } from "../src/ui/index.js";
import { allClassesOf, parseCss } from "./css-rules.js";
import type { CssDecl } from "./css-rules.js";
import { drawn, fakeEngine } from "./graph-fake.js";
import { sheet } from "./styles.js";

afterEach(() => { install("en", null); });

describe("immutable result views (SPEC §data.analysis)", () => {
  it("keeps decimal strings intact, escapes cell content and selects by server row id", () => {
    const select = vi.fn();
    const table = resultTable({ columns: ["amount", "note"], rows: [{ amount: "9007199254740993.01", note: "<script>bad</script>" }], ids: ["result:4"], selected: new Set(), label: "rows", selectLabel: id => id, onSelect: select });
    expect(table.textContent).toContain("9007199254740993.01");
    expect(table.querySelector("script")).toBeNull();
    const input = table.querySelector<HTMLInputElement>("input")!;
    input.checked = true; input.dispatchEvent(new Event("change"));
    expect(select).toHaveBeenCalledWith("result:4", true);
  });
  it("draws negative and positive values with finite geometry and an accessible name", () => {
    const chart = resultChart({ title: "Totals", kind: "bar", points: [{ label: "a", value: -2 }, { label: "b", value: 3 }, { label: "bad", value: NaN }] });
    expect(chart.querySelector("svg")?.getAttribute("aria-label")).toBe("Totals");
    expect(chart.querySelectorAll("rect")).toHaveLength(2);
    expect(chart.innerHTML).not.toMatch(/NaN|Infinity/);
  });
  it("places scatter points on their real numeric x axis", () => {
    // SPEC §data.analysis: x=1,2,100 must not become three evenly spaced categories.
    const chart = resultChart({ title: "Response time", kind: "scatter", points: [
      { label: "1", x: 1, value: 10 }, { label: "2", x: 2, value: 20 }, { label: "100", x: 100, value: 30 },
    ] });
    const xs = Array.from(chart.querySelectorAll("circle")).map(c => Number(c.getAttribute("cx")));
    expect(xs).toHaveLength(3);
    expect((xs[2] - xs[1]) / (xs[1] - xs[0])).toBeCloseTo(98, 6);
  });
});

/* The mermaid bundle is 5.5MB of browser code that needs a layout engine; vendor-markdown.test.ts
   holds it to its contract. Here a stand-in answers the way mermaid's sandbox level does - one
   sandboxed iframe carrying the drawing - fails "not a diagram" the way a parse error does, and
   for "escape" hands back what a sandbox must never be: bare SVG, and a frame allowed scripts. */
const mermaidFake = vi.hoisted(() => ({
  initialize: vi.fn(),
  render: async (id: string, text: string): Promise<{ svg: string }> => {
    if (text.startsWith("not a diagram")) throw new Error("Parse error on line 1");
    if (text.startsWith("escape svg")) return { svg: `<svg id="${id}"><text>x</text></svg>` };
    const sandbox = text.startsWith("escape scripts") ? "allow-scripts allow-popups" : "allow-top-navigation-by-user-activation allow-popups";
    return {
      svg: `<iframe style="width:100%;height:120px;border:0;margin:0" src="about:blank#${id}" sandbox="${sandbox}"` +
        ` title="${text.replace(/&/g, "&amp;").replace(/"/g, "&quot;")}">The "iframe" tag is not supported by your browser.</iframe>`,
    };
  },
}));
vi.mock("../../src/admin_assets/js/vendor/mermaid/12.1.0/index.js", () => ({ loadMermaid: async () => mermaidFake }));

function useHref(root: Element): string | null {
  const use = root.querySelector("use");
  return use ? use.getAttribute("href") : null;
}

describe("ui/icon - iconNode", () => {
  it("is an SVG-namespace <svg class=ic> over one <use href=#i-name>, hidden from AT", () => {
    const svg = iconNode("plus");
    expect(svg.namespaceURI).toBe("http://www.w3.org/2000/svg");
    expect(svg.getAttribute("class")).toBe("ic");
    expect(svg.getAttribute("aria-hidden")).toBe("true");
    expect(useHref(svg)).toBe("#i-plus");
  });

  it("a label makes the glyph speak instead", () => {
    const svg = iconNode("server", "stdio");
    expect(svg.getAttribute("role")).toBe("img");
    expect(svg.getAttribute("aria-label")).toBe("stdio");
    expect(svg.hasAttribute("aria-hidden")).toBe(false);
  });
});

describe("ui/button", () => {
  it("btn is a type=button .btn with the word as its text", () => {
    const b = btn("Save");
    expect(b.tagName).toBe("BUTTON");
    expect(b.type).toBe("button");
    expect(b.className).toBe("btn");
    expect(b.textContent).toBe("Save");
    expect(b.querySelector("svg")).toBeNull();
  });

  it("kind, a leading glyph, the data hook, id, title and disabled", () => {
    const b = btn("Run", { kind: "primary", icon: "play", id: "go", title: "Run it now", data: { act: "run" }, disabled: true });
    expect(b.className).toBe("btn primary with-ic");
    expect(b.firstElementChild?.getAttribute("class")).toBe("ic");
    expect(useHref(b)).toBe("#i-play");
    expect(b.textContent).toBe("Run");
    expect(b.id).toBe("go");
    expect(b.title).toBe("Run it now");
    expect(b.dataset.act).toBe("run");
    expect(b.disabled).toBe(true);
    expect(btn("Remove", { kind: "danger" }).className).toBe("btn danger");
    expect(btn("Cancel", { kind: "ghost" }).className).toBe("btn ghost");
  });

  it("a disabled primary keeps its accent ground, dimmed (ui.css)", () => {
    // Found on a walk of SPEC §panel.pages: .btn:disabled repainted every disabled button's ground to
    // --card, a primary's too, and its --accent-text word is white - a DDL sheet's "Add column"
    // was white on white until a name was typed, a foot with Cancel and a hole beside it.
    const rules = parseCss(sheet("ui.css")).filter((r) => !r.at);
    const off = rules.find((r) => r.selectors.includes(".btn.primary:disabled"));
    expect(off, ".btn.primary:disabled has a rule").toBeTruthy();
    expect(off!.decls.find((d) => d.prop === "background")?.value).toBe("var(--accent)");
    // It must also outrank the primary's own hover, which sits at the same specificity.
    const at = (sel: string): number => rules.findIndex((r) => r.selectors.includes(sel));
    expect(at(".btn.primary:disabled")).toBeGreaterThan(at(".btn.primary:hover"));
  });

  it("a primary icon keeps its contrasting ink instead of the quiet toolbar ink", () => {
    const b = iconBtn("arrow-up", "Send", { kind: "primary" });
    expect(b.className).toBe("btn icon primary");
    expect(b.getAttribute("aria-label")).toBe("Send");
    const rules = parseCss(sheet("ui.css")).filter(r => !r.at);
    const ink = rules.find(r => r.selectors.includes(".btn.icon.primary"));
    expect(ink?.decls.find(d => d.prop === "color")?.value).toBe("var(--accent-text)");
  });

  it("iconBtn: the word is the aria-label and, by default, the tooltip", () => {
    const b = iconBtn("copy", "Copy");
    expect(b.className).toBe("btn icon");
    expect(b.getAttribute("aria-label")).toBe("Copy");
    expect(b.title).toBe("Copy");
    expect(b.hasAttribute("aria-pressed")).toBe(false);
    expect(b.textContent).toBe("");
    expect(useHref(b)).toBe("#i-copy");
  });

  it("iconBtn: a toggle says its state; ghost and an own title", () => {
    const off = iconBtn("maximize", "Focus", { pressed: false, ghost: true, title: "Focus mode (F)" });
    expect(off.getAttribute("aria-pressed")).toBe("false");
    expect(off.className).toBe("btn icon ghost");
    expect(off.title).toBe("Focus mode (F)");
    expect(off.getAttribute("aria-label")).toBe("Focus");
    expect(iconBtn("maximize", "Focus", { pressed: true }).getAttribute("aria-pressed")).toBe("true");
    // Present but not shown yet, like btn({ hidden }): Data's pane ⋯ waits for a SQL connection.
    expect(iconBtn("copy", "Copy", { hidden: true }).hidden).toBe(true);
    expect(moreBtn("More", { id: "m", hidden: true }).hidden).toBe(true);
  });

  it("the action table: one glyph per verb, and the opposite halves never share one", () => {
    // SPEC §panel.design rule 7. The pair is what a reader clicks blind: with one picture for both
    // halves the difference is only in a tooltip nobody hovers.
    expect(ACTION_ICON.enable).not.toBe(ACTION_ICON.disable);
    expect(ACTION_ICON.start).not.toBe(ACTION_ICON.stop);
    expect(ACTION_ICON.startAll).not.toBe(ACTION_ICON.start);
    expect(ACTION_ICON.bookmark).not.toBe(ACTION_ICON.unbookmark);
    expect(ACTION_ICON.refresh).not.toBe(ACTION_ICON.restart);
    expect(ACTION_ICON.new).not.toBe(ACTION_ICON.newGroup);
    // A resume IS a start, and says so: nothing on the panel pauses, so both wear the same play.
    expect(ACTION_ICON.resume).toBe(ACTION_ICON.start);
    expect(ACTION_ICON.pause).not.toBe(ACTION_ICON.stop);
  });

  it("actionBtn carries the verb it performs - the glyph, the name and the data-action hook", () => {
    const b = actionBtn("disable", "Disable this MCP", { id: "primaryBtn", title: "Disable this MCP server" });
    expect(b.className).toBe("btn icon");
    expect(useHref(b)).toBe("#i-power-off");
    expect(b.dataset.action).toBe("disable");
    expect(b.getAttribute("aria-label")).toBe("Disable this MCP");
    expect(b.title).toBe("Disable this MCP server");
    // The pair drawn side by side: two pictures, not one picture and one tooltip.
    expect(useHref(actionBtn("enable", "Enable"))).toBe("#i-power");
    expect(useHref(actionBtn("stop", "Stop"))).not.toBe(useHref(actionBtn("start", "Start")));
  });

  it("setActionBtn moves glyph, name, tooltip and verb together - and never empties the sprite", () => {
    // The trap this exists for: a poll that wrote `textContent = label` onto a state-flipping
    // button deleted its <svg> and left a wordless, iconless box (§panel.design rule 7).
    const b = actionBtn("disable", "Disable", { title: "Disable this tunnel" });
    setActionBtn(b, "enable", "Enable", "Enable this tunnel");
    expect(b.querySelectorAll("svg").length, "the glyph is still there").toBe(1);
    expect(useHref(b)).toBe("#i-power");
    expect(b.dataset.action).toBe("enable");
    expect(b.getAttribute("aria-label")).toBe("Enable");
    expect(b.title).toBe("Enable this tunnel");
    // The started/stopped pair of a port forward turns the same way.
    setActionBtn(b, "stop", "Stop", "Stop this port forward");
    setActionBtn(b, "start", "Start", "Start this port forward");
    expect(useHref(b)).toBe("#i-play");
    // An own title is restated from the label when the caller passes none.
    setActionBtn(b, "stop", "Stop");
    expect(b.title).toBe("Stop");
  });

  it("moreBtn is the ghost ellipsis with the caller's hook", () => {
    const b = moreBtn("More actions", { data: { more: "fs" } });
    expect(b.className).toBe("btn icon ghost");
    expect(useHref(b)).toBe("#i-ellipsis");
    expect(b.getAttribute("aria-label")).toBe("More actions");
    expect(b.dataset.more).toBe("fs");
    // Every ⋯ opens a menu, and says so to assistive tech (SPEC §panel.pages: Data's said it by hand).
    expect(b.getAttribute("aria-haspopup")).toBe("menu");
  });
});

/* One action button through its states (SPEC §panel.design rules 2 and 7): a first render and a
   restated button end with the same attributes, a Stop restated as a Start loses its red, and a
   request in flight disables the button and says the wait in words - the node, the glyph and the
   listener stay the caller's. */
describe("ui/action - one button through its states", () => {
  // Every attribute a reader, a screen reader or the stylesheet can see, as one comparable value.
  const snapshot = (b: HTMLButtonElement) => {
    return {
      attrs: Object.fromEntries(Array.from(b.attributes).map((a): [string, string] => { return [a.name, a.value]; })),
      href: useHref(b),
      svgs: b.querySelectorAll("svg").length,
      disabled: b.disabled,
    };
  };

  it("a Start restated as a Stop ends with the attributes a first Stop render has", () => {
    const first = actionBtn("stop", "Stop", { title: "Stop this port forward" });
    const flipped = actionBtn("start", "Start", { title: "Start this port forward" });
    setActionBtn(flipped, "stop", "Stop", "Stop this port forward");
    expect(snapshot(flipped)).toEqual(snapshot(first));
  });

  it("Stop, Start, Stop: each state wears its own glyph, name, tooltip and tone, and the red leaves with the Stop", () => {
    const b = actionBtn("stop", "Stop", { title: "Stop this port forward" });
    expect(b.dataset.tone).toBe("stop");
    setActionBtn(b, "start", "Start", "Start this port forward");
    expect(useHref(b)).toBe("#i-play");
    expect(b.getAttribute("aria-label")).toBe("Start");
    expect(b.title).toBe("Start this port forward");
    expect(b.dataset.action).toBe("start");
    expect(b.hasAttribute("data-tone"), "a Start carries no stop tone").toBe(false);
    setActionBtn(b, "stop", "Stop", "Stop this port forward");
    expect(useHref(b)).toBe("#i-stop");
    expect(b.getAttribute("aria-label")).toBe("Stop");
    expect(b.title).toBe("Stop this port forward");
    expect(b.dataset.tone).toBe("stop");
    // Restated, never rebuilt: one glyph, and no words written over it.
    expect(b.querySelectorAll("svg"), "the glyph is still there").toHaveLength(1);
    expect(b.textContent).toBe("");
  });

  it("only a stop wears the tone; start, pause, resume, delete and close never do", () => {
    expect(actionBtn("stop", "Stop").dataset.tone).toBe("stop");
    for (const action of ["start", "pause", "resume", "delete", "close"] as const) {
      expect(actionBtn(action, action).hasAttribute("data-tone"), action).toBe(false);
    }
  });

  it("in flight the button is disabled and says the wait; a failed request gives back the Stop", () => {
    const b = actionBtn("stop", "Stop", { title: "Stop this run" });
    const svg = b.querySelector("svg");
    const idle = { label: "Stop", title: "Stop this run" };
    setActionBusy(b, "stop", idle, "stopping…");
    expect(b.disabled).toBe(true);
    expect(b.getAttribute("aria-label")).toBe("stopping…");
    expect(b.title).toBe("stopping…");
    expect(useHref(b)).toBe("#i-stop");
    expect(b.querySelector("svg"), "the same glyph node").toBe(svg);
    // The request failed: the verb is back, enabled, with its own words and its red.
    setActionBusy(b, "stop", idle, null);
    expect(b.disabled).toBe(false);
    expect(b.getAttribute("aria-label")).toBe("Stop");
    expect(b.title).toBe("Stop this run");
    expect(b.dataset.tone).toBe("stop");
    expect(b.querySelector("svg")).toBe(svg);
  });

  it("the node and its listener survive every flip: the click still reaches the caller's handler", () => {
    const b = actionBtn("stop", "Stop", { data: { hstop: "r-1" } });
    const seen: string[] = [];
    b.addEventListener("click", () => { seen.push(b.dataset.hstop ?? ""); });
    setActionBusy(b, "stop", { label: "Stop" }, "stopping…");
    setActionBusy(b, "stop", { label: "Stop" }, null);
    setActionBtn(b, "start", "Start");
    setActionBtn(b, "stop", "Stop");
    b.click();
    expect(seen).toEqual(["r-1"]);
  });

  it("ui.css: a pressable stop is --red, its pale seat is never solid red, and a disabled stop is grey", () => {
    const rules = parseCss(sheet("ui.css"));
    const stopRules = rules.filter((r) => r.selectors.some((s) => s.includes('data-tone="stop"')));
    const decl = (selector: string, prop: string): string | undefined => {
      return stopRules.find((r) => r.selectors.includes(selector))?.decls.find((d: CssDecl) => d.prop === prop)?.value;
    };
    // The square takes the theme's red and nothing else about the button is coloured by it.
    expect(decl('.btn.icon[data-tone="stop"]', "color")).toBe("var(--red)");
    // Hover and focus take a pale seat: a tint of the red, never the solid red itself.
    const seat = decl('.btn.icon[data-tone="stop"]:not(:disabled):hover', "background");
    expect(seat).toContain("color-mix");
    expect(seat).not.toBe("var(--red)");
    expect(decl('.btn.icon[data-tone="stop"]:not(:disabled):focus-visible', "background")).toBe(seat);
    // Disabled: grey like every disabled glyph, never the red that would still shout danger.
    expect(decl('.btn.icon[data-tone="stop"]:disabled', "color")).toBe("var(--text-2)");
    // No stop rule paints a solid red ground anywhere.
    for (const r of stopRules) {
      for (const d of r.decls) {
        if (d.prop.startsWith("background")) expect(d.value, r.selectors.join(" | ")).not.toBe("var(--red)");
      }
    }
  });
});

describe("ui/form constrained picker", () => {
  it("uses a zero-minimum form track and lets the path field shrink beside Go and Up", () => {
    const rules = parseCss(sheet("ui.css"));
    const formRules = rules.find((rule) => rule.selectors.includes(".form"));
    const pickerRules = rules.find((rule) => rule.selectors.includes(".dirpick-bar input"));
    expect(formRules?.decls.find((d) => d.prop === "grid-template-columns")?.value.replaceAll(" ", "")).toBe("minmax(0,1fr)");
    expect(pickerRules?.decls.find((d) => d.prop === "min-width")?.value).toBe("0");
  });
});

describe("ui/row metadata", () => {
  it("keeps copyable facts separate from errors in a wrapping strip", () => {
    const r = row({ name: "app", err: "port conflict", meta: [h("code", null, "PID 42"), portChip("4317", { tone: "up", title: "listening" })], primary: btn("Start"), secondary: iconBtn("file", "Logs"), more: moreBtn("More") });
    expect(Array.from(r.querySelectorAll(".lrow-acts button")).map((b) => b.textContent || b.getAttribute("aria-label"))).toEqual(["Start", "Logs", "More"]);
    expect(r.querySelector(".lrow-err")?.textContent).toBe("port conflict");
    expect(r.querySelector(".lrow-meta code")?.textContent).toBe("PID 42");
    expect(r.querySelector(".lrow-meta .pchip")?.textContent).toBe("4317");
    const rules = parseCss(sheet("ui.css"));
    const meta = rules.find((rule) => rule.selectors.includes(".lrow-meta"));
    expect(meta?.decls.find((d) => d.prop === "flex-wrap")?.value).toBe("wrap");
  });
});

describe("ui/transcript", () => {
  it("positions speakers by identity, independent of the translated role label", () => {
    const node = transcript([{ side: "user", role: "用户", text: "hello" }, { role: "AI", text: "reply" }]);
    expect(node.querySelector(".tc-turn.user")?.textContent).toContain("hello");
    expect(node.querySelector(".tc-turn.assistant")?.textContent).toContain("reply");
    const rules = parseCss(sheet("ui.css"));
    expect(rules.find((r) => r.selectors.includes(".tc-turn.user"))?.decls.find((d) => d.prop === "align-self")?.value).toBe("flex-end");
  });

  it("marks the reader and the model with a person and a robot, the word kept as their name", () => {
    const node = transcript([{ side: "user", role: speaker("user"), text: "hello" }, { role: speaker("assistant"), text: "reply" }]);
    const marks = Array.from(node.querySelectorAll<HTMLElement>(".tc-head .tc-role > .tc-who"));
    expect(marks.map((m) => m.className)).toEqual(["tc-who user", "tc-who assistant"]);
    expect(marks.map((m) => m.querySelector("use")?.getAttribute("href"))).toEqual(["#i-user", "#i-bot"]);
    expect(marks.map((m) => m.title)).toEqual(["user", "assistant"]);
    expect(marks.map((m) => m.querySelector("svg")?.getAttribute("aria-label"))).toEqual(["user", "assistant"]);
    expect(node.querySelector(".tc-head")?.textContent, "no role word on screen").toBe("");
    install("zh-CN", zh);
    expect(speaker("assistant").title).toBe(tr("llm.history.role.assistant"));
    const sprite = readFileSync(new NodeURL("../../src/admin_assets/index.html", import.meta.url), "utf8");
    for (const id of ["i-user", "i-bot"]) expect(sprite, id).toContain(`<symbol id="${id}"`);
    const rules = parseCss(sheet("ui.css"));
    expect(rules.find((r) => r.selectors.includes(".tc-who"))?.decls.find((d) => d.prop === "border-radius")?.value).toBe("50%");
  });

  it("renders every turn as chat - the role in one word, the content in its register", () => {
    const node = transcript([
      { role: tag("user"), at: "2m ago", text: "read the notes", markdown: true },
      { role: tag("thinking"), text: "weighing it first", dim: true },
      { role: tag("assistant"), code: [toolChip("read", '{ "path": "notes.md" }')] },
      { role: tag("tool"), text: "notes.md\t913 B", mono: true },
    ]);
    const turns = Array.from(node.querySelectorAll(".tc-turn"));
    expect(turns.length).toBe(4);
    // The head is the role word (and the moment when the caller passes one).
    expect(turns[0].querySelector(".tc-head .tag")!.textContent).toBe("user");
    expect(turns[0].querySelector(".tc-at")!.textContent).toBe("2m ago");
    // An answer renders as markdown; reasoning is the prose block, dimmed; data is
    // the prose block, monospace; a tool ask rides in the code slot as the chip.
    expect(turns[0].querySelector(".tc-md")).toBeTruthy();
    expect(turns[1].querySelector(".tc-text.dim")!.textContent).toBe("weighing it first");
    expect(turns[2].querySelector(".tc-chip code")!.textContent).toContain("notes.md");
    expect(turns[3].querySelector(".tc-text.mono")!.textContent).toContain("913 B");
    // Spacing separates turns - no hairline log lines (the ledger owns that look).
    const rules = parseCss(sheet("ui.css"));
    const turn = rules.find((rule) => rule.selectors.includes(".tc-turn"));
    expect(turn?.decls.find((d) => d.prop === "border-top")).toBeUndefined();
  });

  it("transcriptTurn carries the caller's hooks, and an empty turn renders its head alone", () => {
    const one = transcriptTurn({ role: tag("tool"), data: { id: "t7" } });
    expect(one.id).toBe("t7");
    expect(one.querySelector(".tc-text")).toBeNull();
    expect(one.querySelector(".tc-code")).toBeNull();
  });

  it("transcriptText: the prose body alone - data goes mono, reasoning dims", () => {
    expect(transcriptText("plain words").className).toBe("tc-text");
    expect(transcriptText("class A {}", { mono: true }).className).toBe("tc-text mono");
    expect(transcriptText("weighing it first", { dim: true }).className).toBe("tc-text dim");
    expect(transcriptText("weighing it first", { dim: true }).textContent).toBe("weighing it first");
  });
});

describe("ui/status", () => {
  it("dot: the state is the class, the title says it aloud", () => {
    const d = dot("error", "Failed to start");
    expect(d.className).toBe("dot error");
    expect(d.title).toBe("Failed to start");
    expect(d.getAttribute("role")).toBe("img");
    expect(d.getAttribute("aria-label")).toBe("Failed to start");
    // "off" is the bare grey dot: no class for a rule that would only repeat .dot's own.
    expect(dot("off", "Stopped").className).toBe("dot");
  });

  it("tag: sans and toneless by default; mono for a value, a tone only for state", () => {
    expect(tag("proxy").className).toBe("tag");
    expect(tag("exit 2", { mono: true, tone: "bad", title: "non-zero exit" }).className).toBe("tag mono bad");
    expect(tag("slow", { tone: "warn" }).className).toBe("tag warn");
    expect(tag("exit 2", { title: "non-zero exit" }).title).toBe("non-zero exit");
  });

  it("heldDot: the amber mark of buffered writes, its count said aloud (SPEC §panel.pages)", () => {
    const d = heldDot("2 buffered changes");
    expect(d.className).toBe("db-tab-dot");
    expect(d.title).toBe("2 buffered changes");
    expect(d.getAttribute("role")).toBe("img");
    expect(d.getAttribute("aria-label")).toBe("2 buffered changes");
    // null: the row that holds it already says so (a menu row standing in for the tab).
    const quiet = heldDot(null);
    expect(quiet.getAttribute("title")).toBeNull();
    expect(quiet.getAttribute("aria-hidden")).toBe("true");
  });
});

describe("ui/sheet - sheet()", () => {
  it("a conversation sheet docks its composer between the scroll body and actions", () => {
    const input = h("textarea", { rows: 3 });
    const s = sheetFrame({ title: "Chat", body: "turns", compose: input, foot: "actions", wide: true });
    expect(s.classList.contains("conversation")).toBe(true);
    expect(Array.from(s.children).map((n) => n.className)).toEqual(["sheet-head", "sheet-body", "sheet-compose", "sheet-foot"]);
    expect(s.querySelector(".sheet-compose textarea")).toBe(input);
    const css = sheet("ui.css");
    expect(css).toMatch(/\.sheet\.conversation \{[^}]*display: flex;[^}]*flex-direction: column;/);
    expect(css).toMatch(/\.sheet\.conversation > \.sheet-body \{[^}]*min-height: 0;[^}]*overflow-y: auto;/);
  });

  it("the frame: a titled dialog, its body, its foot", () => {
    const s = sheetFrame({ title: "Rename key", body: "b", foot: "f" });
    expect(s.getAttribute("role")).toBe("dialog");
    expect(s.getAttribute("aria-label")).toBe("Rename key");
    expect(s.querySelector(".sheet-head h2")!.textContent).toBe("Rename key");
    expect(s.querySelector(".sheet-sub")).toBeNull();
  });

  it("sub: the value the sheet acts on, mono, after the title and not inside it (SPEC §panel.pages)", () => {
    const s = sheetFrame({ title: "note", sub: "demo_shop.orders · pk {\"id\":4}", label: "Edit cell", body: "b", foot: "f" });
    const head = s.querySelector(".sheet-head")!;
    expect(Array.from(head.children).map((c) => c.tagName + "." + c.className)).toEqual(["H2.", "SPAN.sheet-sub"]);
    expect(head.querySelector("h2")!.textContent).toBe("note");
    expect(head.querySelector(".sheet-sub")!.textContent).toBe("demo_shop.orders · pk {\"id\":4}");
    expect(s.getAttribute("aria-label")).toBe("Edit cell");
    const css = sheet("ui.css");
    expect(css).toMatch(/\.sheet-sub \{[^}]*font-family: var\(--mono\);[^}]*color: var\(--text-2\);/);
    expect(css).toMatch(/\.sheet-head \{[^}]*align-items: baseline;/);
  });
});

describe("ui/switch - sw", () => {
  it("is a role=switch button whose aria-checked is the state", () => {
    const on = sw(true, "Enable github", { data: { sw: "github" } });
    expect(on.className).toBe("sw");
    expect(on.type).toBe("button");
    expect(on.getAttribute("role")).toBe("switch");
    expect(on.getAttribute("aria-checked")).toBe("true");
    expect(on.getAttribute("aria-label")).toBe("Enable github");
    expect(on.dataset.sw).toBe("github");
    const off = sw(false, "Enable github", { disabled: true });
    expect(off.getAttribute("aria-checked")).toBe("false");
    expect(off.disabled).toBe(true);
  });
});

describe("ui/page", () => {
  it("paneHead: a description and the actions; no location title unless asked", () => {
    const head = paneHead({ desc: "Forward local ports.", actions: [btn("New", { kind: "primary" })] });
    expect(head.className).toBe("pane-head");
    expect(head.querySelector("h1")).toBeNull();
    expect(head.querySelector(".pane-desc")?.textContent).toBe("Forward local ports.");
    expect(head.querySelector(".pane-actions > .btn.primary")).not.toBeNull();
    const res = paneHead({ title: "github", sub: "stdio · 12 tools" });
    expect(res.querySelector("h1.pane-title")?.textContent).toBe("github");
    expect(res.querySelector(".pane-sub")?.textContent).toBe("stdio · 12 tools");
    expect(res.querySelector(".pane-actions")).toBeNull();
    expect(paneHead({ desc: "x", actions: [] }).querySelector(".pane-actions")).toBeNull();
  });

  it("resHead: the name row, the words, the nav - three layers, each a child of the frame", () => {
    const nav = seg([{ id: "tools", label: "Tools" }], "tools");
    const layers = resHead({ title: "orders-db", desc: "Orders.", sub: "up", actions: [moreBtn("More")], nav });
    expect(layers.map((n) => n.className)).toEqual(["pane-head res", "res-meta", "pane-nav"]);
    const [head, meta, pinned] = layers;
    expect(head.querySelector("h1.pane-title")?.textContent).toBe("orders-db");
    // The name row clips a long name; its tooltip keeps the whole one.
    expect(head.querySelector("h1")?.getAttribute("title")).toBe("orders-db");
    expect(head.querySelector(".pane-actions > .btn.icon")).not.toBeNull();
    expect(meta.querySelector(".pane-desc")?.textContent).toBe("Orders.");
    expect(meta.querySelector(".pane-sub")?.textContent).toBe("up");
    expect(pinned.firstElementChild).toBe(nav);
    const frame = paneBody({ wide: true }, ...layers);
    expect(frame.className).toBe("wide");
    expect(frame.children).toHaveLength(3);
    expect(paneBody({}).getAttribute("class")).toBeNull();
  });

  it("resHead without words or tabs is the name row alone", () => {
    const only = resHead({ title: "x", desc: null, sub: false });
    expect(only.map((n) => n.className)).toEqual(["pane-head res"]);
    expect(only[0].querySelector(".pane-actions")).toBeNull();
  });

  it("note: quiet by default, the spinner first when busy, contained when it is a failure", () => {
    expect(note("restarted").className).toBe("note");
    const busy = note("Loading…", { busy: true, id: "n1" });
    expect(busy.id).toBe("n1");
    expect(busy.firstElementChild?.className).toBe("spin");
    expect(busy.textContent).toBe(" Loading…");
    expect(note("401", { err: true }).className).toBe("note err");
    // The spinner is decoration beside the words that say what is loading.
    expect(spinner().getAttribute("aria-hidden")).toBe("true");
  });

  it("row({ detail }): the name and sub become a disclosure; the controls stay outside it", () => {
    const r = row({ name: "query_orders", sub: "Lists orders.", detail: valueBlock({ label: "Input schema", text: "none" }), primary: btn("Try"), toggle: sw(true, "Visible") });
    expect(r.className).toBe("lrow has-disc");
    const disc = r.querySelector<HTMLDetailsElement>("details.lrow-main.lrow-disc")!;
    expect(disc.querySelector("summary > .lrow-chev use")!.getAttribute("href")).toBe("#i-chevron-right");
    expect(disc.querySelector("summary .lrow-name")!.textContent).toBe("query_orders");
    expect(disc.querySelector("summary .lrow-sub")!.textContent).toBe("Lists orders.");
    expect(disc.querySelector(".lrow-detail .vblock-text")!.textContent).toBe("none");
    expect(disc.open).toBe(false);
    const acts = r.querySelector(".lrow-acts")!;
    expect(disc.contains(acts)).toBe(false);
    expect(acts.querySelectorAll("button").length).toBe(2);
    expect(row({ name: "plain" }).querySelector("details"), "no detail, no disclosure").toBeNull();
  });

  it("forms: a label over its control with star and meta, a check to the right, a pair, the foot", () => {
    const input = document.createElement("input");
    input.id = "r-arg-sql";
    const f = fieldFn({ label: "sql", required: true, meta: "string", control: input, hint: "The statement." });
    expect(f.className).toBe("fld");
    const label = f.querySelector("label.field")!;
    expect(label.firstElementChild!.textContent).toBe("sql *string");
    expect(label.querySelector(".req-star")!.textContent).toBe("*");
    expect(label.querySelector(".field-meta")!.textContent).toBe("string");
    expect(label.querySelector("#r-arg-sql"), "the control is inside the label, so a click on the words focuses it").not.toBeNull();
    expect(f.querySelector(":scope > .hint")!.textContent).toBe("The statement.");
    expect(fieldFn({ label: "x", control: document.createElement("input"), hint: "" }).querySelector(".hint"), "no hint, no empty line").toBeNull();
    const box = document.createElement("input");
    box.type = "checkbox";
    const c = checkFieldFn({ label: "Start", control: box });
    expect(c.querySelector("label.check")!.firstElementChild).toBe(box);
    expect(pair(fieldFn({ label: "a", control: document.createElement("input") }), fieldFn({ label: "b", control: document.createElement("input") })).className).toBe("two");
    const whole = formFn(f, formActions(btn("Run", { kind: "primary" })));
    expect(whole.className).toBe("form");
    expect(whole.querySelector(".form-actions > .btn.primary")).not.toBeNull();
    const refusal = hintFn("taken", { id: "g-err", bad: true, hidden: true, live: true });
    expect(refusal.className).toBe("hint bad");
    expect(refusal.hidden).toBe(true);
    expect(refusal.getAttribute("aria-live")).toBe("polite");
  });

  it("field({ group }): several controls under one caption that clicks none of them", () => {
    const days = h("div", null, btn("Sun"), btn("Mon"));
    const f = fieldFn({ label: "Days", control: days, group: true });
    const g = f.querySelector(":scope > .field")!;
    expect(g.tagName).toBe("DIV"); // not a <label>: its caption must not activate "Sun"
    expect(g.getAttribute("role")).toBe("group");
    const cap = g.querySelector(":scope > span")!;
    expect(cap.textContent).toBe("Days");
    expect(g.getAttribute("aria-labelledby")).toBe(cap.id);
    expect(cap.id).toMatch(/^fld-g\d+$/);
    // Two group fields never share a caption id.
    expect(fieldFn({ label: "x", control: h("div"), group: true }).querySelector("span")!.id).not.toBe(cap.id);
    // The caption styles the same either way: ui.css keys on .field, not label.field.
    const css = sheet("ui.css");
    expect(css).toContain(".field > span:first-child { display: block;");
    expect(css).not.toMatch(/label\.field/);
  });

  it("field({ action }), formCap, formFold: a control with its button, a caption, a folded part", () => {
    const f = fieldFn({ label: "Key", control: document.createElement("input"), action: btn("Browse") });
    const rowEl = f.querySelector(":scope > .field-row")!;
    expect(rowEl.firstElementChild!.className).toBe("field");
    expect(rowEl.lastElementChild!.textContent).toBe("Browse");
    expect(formCapFn("Proxy").className).toBe("form-cap");
    const fold = formFoldFn({ summary: "Advanced", id: "adv" }, fieldFn({ label: "x", control: document.createElement("input") }));
    expect(fold.tagName).toBe("DETAILS");
    expect(fold.id).toBe("adv");
    expect((fold as HTMLDetailsElement).open, "a fold starts folded").toBe(false);
    expect(fold.querySelector(":scope > summary")!.textContent).toBe("Advanced");
    expect(fold.querySelector(":scope > .fold-body > .fld")).not.toBeNull();
  });

  it("a draggable library row shows the drag: dimmed while carried, an accent edge where it lands (ui.css)", () => {
    // Found on the P4 review: row({ draggable }) carried the grab cursor but not the feedback
    // the groups component toggles (.dragging / .drop-before / .drop-after) - those lived on
    // .tun-row, so a Tunnels row on the library dragged with no line showing where it lands.
    const rules = parseCss(sheet("ui.css"));
    const decl = (sel: string, prop: string): string | undefined =>
      rules.find((r) => !r.at && r.selectors.includes(sel))?.decls.find((d) => d.prop === prop)?.value;
    expect(decl(".lrow.dragging", "opacity")).toBe("0.55");
    expect(decl(".lrow.drop-before", "box-shadow")).toBe("inset 0 2px 0 var(--accent)");
    expect(decl(".lrow.drop-after", "box-shadow")).toBe("inset 0 calc(var(--s1) / -2) 0 var(--accent)");
  });

  it("a pair's two fields sit level: the stacking margin never applies inside .two (ui.css)", () => {
    // Found on the P2-3c walk: in a sheet (not a .form) the second half of a pair sat 12px low.
    const rules = parseCss(sheet("ui.css"));
    const rule = rules.find((r) => !r.at && r.selectors.includes(".two > .fld + .fld"));
    expect(rule?.decls.find((d) => d.prop === "margin-top")?.value).toBe("0");
    // ...and a pair is two columns wherever it sits, not only under .form or .sheet-body > (the
    // Add sheet's generated pairs are one div deeper and stacked).
    const grid = rules.find((r) => !r.at && r.selectors.length === 1 && r.selectors[0] === ".two");
    expect(grid?.decls.find((d) => d.prop === "display")?.value).toBe("grid");
    expect(grid?.decls.find((d) => d.prop === "grid-template-columns")?.value).toBe("1fr 1fr");
    // Stacked in a plain container, fields and pairs are a grid step apart; in a grid the gap does it.
    const css = sheet("ui.css");
    expect(css).toMatch(/:is\(\.fld, \.two\) \+ :is\(\.fld, \.two\) \{ margin-top: var\(--s3\); \}/);
    expect(css).toMatch(/:is\(\.form, \.sheet-body, \.fold-body\) > :is\(\.fld, \.two\) \+ :is\(\.fld, \.two\) \{ margin-top: 0; \}/);
  });

  it("a check beside a field in a pair sits on the control's row, not the caption's (ui.css)", () => {
    // Found on a walk of SPEC §panel.pages: the index sheet's Unique sat level with the "Index name"
    // caption, 18px above the input it qualifies (Jobs' timeout + Disabled pair did the same).
    // End-aligned, lifted by the input's own padding and border, it centres on the input.
    const rule = parseCss(sheet("ui.css")).find((r) => !r.at && r.selectors.includes(".two > .fld:has(> label.check)"));
    expect(rule?.decls.find((d) => d.prop === "align-self")?.value).toBe("end");
    expect(rule?.decls.find((d) => d.prop === "padding-bottom")?.value).toBe("calc(var(--s1) + 2px)");
  });

  it("dot(state, null): a dot that defers its hover to what holds it - no title, out of AT", () => {
    const d = dot("off", null);
    expect(d.className).toBe("dot");
    expect(d.hasAttribute("title")).toBe(false);
    expect(d.getAttribute("aria-hidden")).toBe("true");
    expect(d.hasAttribute("role")).toBe(false);
    const said = dot("up", "up");
    expect([said.getAttribute("title"), said.getAttribute("role"), said.getAttribute("aria-label")]).toEqual(["up", "img", "up"]);
  });

  it("pager: newer, a live status, older - a navigation landmark the view patches by id", () => {
    const p = pager({ id: "pg", label: "Pages", statusId: "st", status: "Page 2", prev: btn("Newer"), next: btn("Older", { disabled: true }) });
    expect(p.getAttribute("role")).toBe("navigation");
    expect(p.getAttribute("aria-label")).toBe("Pages");
    expect([...p.children].map((c) => c.className)).toEqual(["btn", "pager-status", "btn"]);
    const st = p.querySelector(".pager-status")!;
    expect(st.id).toBe("st");
    expect(st.getAttribute("aria-live")).toBe("polite");
    expect(st.textContent).toBe("Page 2");
  });

  it("failNote: the sentence, the status only when there is one, the way out", () => {
    const f = failNote({ id: "e", text: "Could not load.", why: "HTTP 502", action: btn("Retry", { id: "r" }) });
    expect(f.getAttribute("role")).toBe("status");
    expect(f.querySelector(".fail-why")!.textContent).toBe("HTTP 502");
    expect(f.querySelector("#r")).not.toBeNull();
    expect(failNote({ text: "x", why: "" }).querySelector(".fail-why"), "no status, no empty span").toBeNull();
  });

  it("filterInput: a search field with a name, not a form field", () => {
    const f = filterInput({ id: "q", placeholder: "Search calls", label: "Search tool calls", value: "GET" });
    expect(f.type).toBe("search");
    expect(f.className).toBe("filter");
    expect(f.getAttribute("aria-label")).toBe("Search tool calls");
    expect(f.value).toBe("GET");
  });

  it("valueBlock: caption, notes, tools on one line, then the body", () => {
    const b = valueBlock({ label: "Result", notes: ["JSON + text"], tools: [iconBtn("copy", "Copy result")], data: { blk: "out:7" } },
      jsonCodeNode({ v: 1 }, false, { oneLine: true }).node);
    expect(b.dataset.blk).toBe("out:7");
    const head = b.firstElementChild!;
    expect([...head.children].map((c) => c.className)).toEqual(["vblock-cap", "vblock-note", "vblock-tools"]);
    expect(b.querySelector("pre.jv.one")!.textContent).toBe('{"v": 1}');
    expect(valueBlock({ label: "Arguments" }).querySelector(".vblock-tools"), "no tools, no empty cell").toBeNull();
  });

  it("timelineMeta: the parts that are there, joined by a middle dot", () => {
    expect(timelineMeta(["via mcp", null, "", "reply 4 chars"]).textContent).toBe("via mcp · reply 4 chars");
  });

  it("section: caption and tools share the head; tools alone keep the head's two columns", () => {
    const s = section({ cap: "Scheduled commands", tools: [iconBtn("plus", "Add")] }, card(row({ name: "backup" })));
    expect(s.tagName).toBe("SECTION");
    expect(s.className).toBe("sec");
    expect(s.querySelector(".sec-head > .sec-cap")?.textContent).toBe("Scheduled commands");
    expect(s.querySelector(".sec-head > .sec-tools > .btn.icon")).not.toBeNull();
    expect(s.querySelector(":scope > .group > .lrow")).not.toBeNull();
    const toolsOnly = section({ tools: [iconBtn("plus", "Add")] });
    const kids = Array.from(toolsOnly.querySelector(".sec-head")?.children || []);
    expect(kids.map((k) => k.className)).toEqual(["", "sec-tools"]);
    expect(section({}, "body").querySelector(".sec-head")).toBeNull();
  });

  it("card is the .group surface over its rows", () => {
    const c = card(row({ name: "a" }), row({ name: "b" }));
    expect(c.className).toBe("group");
    expect(c.querySelectorAll(".lrow").length).toBe(2);
  });

  it("pageFoot: prose left, the revision (mono) right only when there is one", () => {
    const f = pageFoot({ note: "Saved to gateway.config.json", rev: "rev 42" });
    expect(f.className).toBe("page-foot");
    expect(f.firstElementChild?.textContent).toBe("Saved to gateway.config.json");
    expect(f.querySelector(".page-foot-rev")?.textContent).toBe("rev 42");
    expect(pageFoot({ note: "x" }).querySelector(".page-foot-rev")).toBeNull();
  });

  it("row({ cols: [{ w }] }): a fixed-width column that ellipsizes, its whole value in the title", () => {
    const r = row({ name: "backup", cols: [{ v: "At 03:00 AM", w: "m", title: "cron 0 3 * * *" }, { v: "in 13 hr.", w: "s" }, "5 rules"] });
    const cols = Array.from(r.querySelectorAll(".lrow-col"));
    expect(cols.map((c) => c.className)).toEqual(["lrow-col w-m", "lrow-col w-s", "lrow-col"]);
    expect(cols[0].getAttribute("title")).toBe("cron 0 3 * * *");
    const css = sheet("ui.css");
    expect(css).toMatch(/\.lrow-col\.w-s \{ width: calc\(var\(--s8\) \* 2\.5\); \}/);
    expect(css).toMatch(/\.lrow-col:is\(\.w-s, \.w-m, \.w-l\) \{ overflow: hidden; text-overflow: ellipsis; \}/);
  });

  it("relTime: the largest unit that fits, in the locale's short words, both directions", () => {
    const now = Date.UTC(2026, 8, 24, 14, 0, 0);
    const H = 60 * 60 * 1000;
    expect(relTime(now - 8 * H, now)).toBe("8 hr. ago");
    expect(relTime(now + 15 * H, now)).toBe("in 15 hr.");
    expect(relTime(now + 12 * 60 * 1000, now)).toBe("in 12 min.");
    expect(relTime(now - 40 * 1000, now)).toBe("40 sec. ago");
    expect(relTime(now, now)).toBe("now");
    expect(relTime(now - 6 * 24 * H, now)).toBe("6 days ago");
    expect(relTime(now + 30 * H, now)).toBe("tomorrow");
  });

  it("inlineForm is one .inline-form row of the given controls", () => {
    const f = inlineForm(nameInput(), btn("Add", { kind: "primary" }));
    expect(f.className).toBe("inline-form");
    expect(f.children.length).toBe(2);
  });

  it("emptyNode: glyph, title, hint, and an action the view answers by data-empty-action", () => {
    const e = emptyNode({ icon: "server", title: "No MCP servers", hint: "Add one to begin.", action: "Add server" });
    expect(e.className).toBe("empty");
    expect(useHref(e.querySelector(".empty-ic") as Element)).toBe("#i-server");
    expect(e.querySelector("h2")?.textContent).toBe("No MCP servers");
    expect(e.querySelector("p.hint")?.textContent).toBe("Add one to begin.");
    const act = e.querySelector("button.btn.ghost") as HTMLButtonElement;
    expect(act.dataset.emptyAction).toBe("Add server");
    expect(act.textContent).toBe("Add server");
    const bare = emptyNode({ icon: "server", title: "Nothing" });
    expect(bare.querySelector(".hint")).toBeNull();
    expect(bare.querySelector("button")).toBeNull();
  });

  it("the empty hint balances its lines (ui.css)", () => {
    // 44ch is 44 Latin digits but about 22 CJK characters: a 23-character Chinese hint left one
    // character alone on its second line (the gallery's empty scene, P1b-3).
    const rule = parseCss(sheet("ui.css")).find((r) => !r.at && r.selectors.includes(".empty p"));
    expect(rule?.decls.find((d) => d.prop === "text-wrap")?.value).toBe("balance");
  });
});

function nameInput(): HTMLInputElement {
  const i = document.createElement("input");
  i.placeholder = "name";
  return i;
}

describe("ui/row", () => {
  it("row: lead, name, sub - and has-lead moves the separator past the dot", () => {
    const r = row({ lead: dot("up", "Running"), name: "github", sub: "stdio", data: { srv: "github" } });
    expect(r.className).toBe("lrow has-lead");
    expect(r.dataset.srv).toBe("github");
    expect(r.querySelector(":scope > .lrow-lead > .dot.up")).not.toBeNull();
    expect(r.querySelector(".lrow-main > .lrow-name")?.textContent).toBe("github");
    expect(r.querySelector(".lrow-main > .lrow-sub")?.textContent).toBe("stdio");
    expect(r.querySelector(".lrow-acts")).toBeNull();
    expect(r.hasAttribute("draggable")).toBe(false);
  });

  it("an error replaces the sub line and carries its whole text in the tooltip", () => {
    const why = "spawn npx ENOENT: the command was not found on PATH";
    const r = row({ name: "fs", sub: "stdio", err: why });
    const err = r.querySelector(".lrow-err") as HTMLElement;
    expect(err.textContent).toBe(why);
    expect(err.title).toBe(why);
    expect(r.querySelector(".lrow-sub")).toBeNull();
    expect(r.classList.contains("has-lead")).toBe(false);
  });

  it("cols: a plain child is a sans column, a RowCol may be mono with a title", () => {
    const r = row({ name: "pg", cols: ["3 rules", { v: "5432", mono: true, title: "local port" }] });
    const cols = Array.from(r.querySelectorAll<HTMLElement>(".lrow-col"));
    expect(cols.map((c) => c.className)).toEqual(["lrow-col", "lrow-col mono"]);
    expect(cols.map((c) => c.textContent)).toEqual(["3 rules", "5432"]);
    expect(cols[1].title).toBe("local port");
  });

  it("the acts keep their order - switch, the one word, the ellipsis", () => {
    const r = row({
      name: "nightly", muted: true, draggable: true, title: "nightly backup",
      more: moreBtn("More"), primary: btn("Run"), toggle: sw(false, "Enable nightly"),
    });
    const acts = Array.from(r.querySelector(".lrow-acts")?.children || []);
    expect(acts.map((a) => a.className)).toEqual(["sw", "btn", "btn icon ghost"]);
    expect(r.className).toBe("lrow muted");
    expect(r.getAttribute("draggable")).toBe("true");
    expect(r.title).toBe("nightly backup");
  });

  it("kvRow: the key column and a value that is mono only when asked", () => {
    const kv = kvRow("Config file", "C:/swiss/gateway.config.json", { mono: true, title: "copy me" });
    expect(kv.className).toBe("kv");
    expect(kv.querySelector(".kv-k")?.textContent).toBe("Config file");
    const v = kv.querySelector(".kv-v") as HTMLElement;
    expect(v.className).toBe("kv-v mono");
    expect(v.title).toBe("copy me");
    expect(kvRow("Uptime", "3 days").querySelector(".kv-v")?.className).toBe("kv-v");
  });
});

describe("ui/row - sideRow", () => {
  it("one listbox option: dot, name, the badge before the tail", () => {
    const r = sideRow({ name: "swiss", lead: dot("up", "up"), badge: tag("3", { tone: "warn", title: "3 waiting" }), tail: "12", selected: true, data: { mproject: "p-1" } });
    expect(r.getAttribute("role")).toBe("option");
    expect(r.getAttribute("aria-selected")).toBe("true");
    expect(r.dataset.mproject).toBe("p-1");
    expect(Array.from(r.children).map((c) => c.className)).toEqual(["dot up", "side-name", "tag warn", "side-type"]);
    expect(r.querySelector(".tag")?.getAttribute("title")).toBe("3 waiting");
    expect(sideRow({ name: "orders", tail: "0" }).querySelector(".tag")).toBeNull();
  });
});

describe("ui/workspace", () => {
  it("the frame: a side column, its grip, and the main column beside it; no side, the main alone", () => {
    const w = workspace(wsSide({ cap: "Projects" }, sideRow({ name: "swiss" })), h("p", null, "body"));
    expect(w.className).toBe("ws");
    expect(Array.from(w.children).map((c) => c.className)).toEqual(["ws-side", "grip x", "ws-main"]);
    expect(w.querySelector(".grip")?.getAttribute("aria-label"), "named for the side column").toBe("Resize Projects");
    expect(w.querySelector(".grip .grip-btn"), "a workspace side folds away").not.toBeNull();
    expect(w.querySelector(".ws-main > p")?.textContent).toBe("body");
    expect(Array.from(workspace(null, "x").children).map((c) => c.className)).toEqual(["ws-main"]);
  });

  it("the full pane a workspace takes has no measure: its columns divide the whole body", () => {
    // .pane > * caps page content at --measure; a workspace under that cap left the right 40% of
    // a 1600px window blank and squeezed the Memory graph into a 375px column (found on 19998).
    expect(sheet("ui.css")).toMatch(/\.pane\.full > \* \{ max-width: none; \}/);
  });

  it("wsSide: a captioned aside over a listbox of source rows; tools only when given", () => {
    const side = wsSide({ cap: "Projects", id: "rail", tools: [iconBtn("plus", "New project")] }, sideRow({ name: "a" }), sideRow({ name: "b" }));
    expect(side.tagName).toBe("ASIDE");
    expect(side.id).toBe("rail");
    expect(side.getAttribute("aria-label")).toBe("Projects");
    expect(side.querySelector(".ws-side-head > .sec-cap")?.textContent).toBe("Projects");
    expect(side.querySelector(".ws-side-head > .sec-tools .btn.icon")).not.toBeNull();
    const list = side.querySelector("nav.side-list") as HTMLElement;
    expect(list.getAttribute("role")).toBe("listbox");
    expect(list.children.length).toBe(2);
    expect(wsSide({ cap: "c" }).querySelector(".sec-tools")).toBeNull();
  });

  it("wsHead: the name (whole in its tooltip), the sub line, the actions, the tabs", () => {
    const head = wsHead({ id: "hd", title: "swiss", sub: h("code", null, "C:/src"), actions: [btn("New")], nav: seg([{ id: "a", label: "A" }], "a") });
    expect(head.tagName).toBe("HEADER");
    expect(head.id).toBe("hd");
    const title = head.querySelector(".ws-head-row h1.ws-title") as HTMLElement;
    expect(title.textContent).toBe("swiss");
    expect(title.title).toBe("swiss");
    expect(head.querySelector(".ws-sub > code")?.textContent).toBe("C:/src");
    expect(head.querySelector(".ws-acts > .btn")?.textContent).toBe("New");
    expect(head.querySelector(".ws-nav > .seg")).not.toBeNull();
    const bare = wsHead({ title: "t" });
    expect(bare.querySelector(".ws-sub, .ws-acts, .ws-nav")).toBeNull();
  });

  it("wsSplit: the fixed pane named by side, each pane a labelled section", () => {
    const list = wsSplit({ id: "body", fixed: "start", start: "list", end: "detail", startLabel: "Notes", endLabel: "Note" });
    expect(list.id).toBe("body");
    const panes = Array.from(list.children) as HTMLElement[];
    expect(panes.map((p) => p.className)).toEqual(["ws-pane fixed start", "ws-pane"]);
    expect(panes.map((p) => p.getAttribute("aria-label"))).toEqual(["Notes", "Note"]);
    expect(panes.map((p) => p.textContent)).toEqual(["list", "detail"]);
    const insp = wsSplit({ fixed: "end", start: "graph", end: "inspector" });
    expect(Array.from(insp.children).map((p) => p.className)).toEqual(["ws-pane", "ws-pane fixed end"]);
  });

  it("wsBar runs along one line, or a line per row; foot is the count under a pane", () => {
    const bar = wsBar({ id: "b", data: { x: "1" } }, btn("a"), btn("b"));
    expect(bar.className).toBe("ws-bar");
    expect(bar.dataset.x).toBe("1");
    expect(bar.children.length).toBe(2);
    const rows = wsBar({ rows: [[filterInput({ placeholder: "q", label: "q" })], [btn("a"), btn("b")]] });
    expect(rows.className).toBe("ws-bar rows");
    expect(Array.from(rows.children).map((r) => [r.className, r.children.length])).toEqual([["ws-bar-row", 1], ["ws-bar-row", 2]]);
    expect(wsBar({ foot: true }, "3 notes").className).toBe("ws-bar foot");
    expect(wsScroll({}, "x").className).toBe("ws-scroll");
    expect(wsScroll({ pad: true, id: "d" }).outerHTML).toBe("<div class=\"ws-scroll pad\" id=\"d\"></div>");
  });

  it("pickRow: an option of two lines; the selected one is the list's one tab stop", () => {
    const a = pickRow({ lead: catChip(1, "decision", { bare: true }), name: "SQLite over redb", meta: "2 hr.", metaTitle: "updated", sub: "why", selected: true, data: { mnote: "m-1" } });
    const b = pickRow({ name: "redb as the store", muted: true, data: { mnote: "m-6" } });
    const list = pickList({ label: "Notes", id: "l" }, a, b);
    expect(list.getAttribute("role")).toBe("listbox");
    expect(list.getAttribute("aria-label")).toBe("Notes");
    expect(a.tagName).toBe("BUTTON");
    expect(a.type).toBe("button");
    expect(a.getAttribute("role")).toBe("option");
    expect(a.getAttribute("aria-selected")).toBe("true");
    expect(a.tabIndex).toBe(0);
    expect(a.dataset.mnote).toBe("m-1");
    expect(Array.from(a.querySelector(".pick-top")?.children || []).map((c) => c.className)).toEqual(["pick-lead", "pick-name", "pick-meta"]);
    expect(a.querySelector(".pick-name")?.getAttribute("title")).toBe("SQLite over redb");
    expect(a.querySelector(".pick-meta")?.getAttribute("title")).toBe("updated");
    expect(a.querySelector(".pick-sub")?.textContent).toBe("why");
    expect(b.className).toBe("pick-row muted");
    expect(b.getAttribute("aria-selected")).toBe("false");
    expect(b.tabIndex).toBe(-1);
    expect(b.querySelector(".pick-lead, .pick-meta, .pick-sub")).toBeNull();
  });

  it("pickRow: depth indents a row of a tree, at most four steps", () => {
    expect([0, 1, 2, 4, 6, -1].map((depth) => pickRow({ name: "n", depth }).className)).toEqual(["pick-row", "pick-row d1", "pick-row d2", "pick-row d4", "pick-row d4", "pick-row"]);
    expect(pickRow({ name: "n", depth: 1, muted: true }).className).toBe("pick-row d1 muted");
  });

  it("pickSelect moves the mark and the tab stop in place and hands back the row", () => {
    const rows = ["a", "b", "c"].map((id, i) => pickRow({ name: id, selected: i === 0, data: { id } }));
    const list = pickList({ label: "l" }, rows);
    const hit = pickSelect(list, (r) => r.dataset.id === "c");
    expect(hit).toBe(rows[2]);
    expect(rows.map((r) => r.getAttribute("aria-selected"))).toEqual(["false", "false", "true"]);
    expect(rows.map((r) => r.tabIndex)).toEqual([-1, -1, 0]);
    expect(pickSelect(list, () => false)).toBeNull();
    expect(list.querySelectorAll("[aria-selected=true]").length).toBe(0);
  });

  it("docHead: tags, the title, the meta values with a quiet separator, the verbs", () => {
    const d = docHead({ tags: [tag("t")], title: "SQLite over redb", meta: [h("code", null, "m-1"), null, "", "by codex", "rev 2"], actions: [btn("Edit")] });
    expect(d.tagName).toBe("HEADER");
    expect(Array.from(d.children).map((c) => c.className)).toEqual(["doc-tags", "doc-title", "doc-meta", "doc-acts"]);
    expect(d.querySelector("h2.doc-title")?.textContent).toBe("SQLite over redb");
    const meta = d.querySelector(".doc-meta") as HTMLElement;
    expect(meta.textContent).toBe("m-1·by codex·rev 2");
    expect(Array.from(meta.querySelectorAll(".doc-sep")).every((s) => s.getAttribute("aria-hidden") === "true")).toBe(true);
    expect(Array.from(docHead({ title: "t" }).children).map((c) => c.className)).toEqual(["doc-title"]);
  });

  it("docHead: a lede is the paragraph under the title; an empty one draws nothing", () => {
    const d = docHead({ title: "Payments", lede: "Charging and refunds", meta: ["rev 2"] });
    expect(Array.from(d.children).map((c) => c.className)).toEqual(["doc-title", "doc-lede", "doc-meta"]);
    expect(d.querySelector("p.doc-lede")?.textContent).toBe("Charging and refunds");
    expect(docHead({ title: "t", lede: "" }).querySelector(".doc-lede")).toBeNull();
    expect(docHead({ title: "t", lede: null }).querySelector(".doc-lede")).toBeNull();
  });

  it("refLink is a link-looking button", () => {
    const r = refLink("Use UTC", { data: { mopen: "m-4" } });
    expect(r.tagName).toBe("BUTTON");
    expect(r.type).toBe("button");
    expect(r.className).toBe("ref");
    expect(r.title).toBe("Use UTC");
    expect(r.dataset.mopen).toBe("m-4");
    expect(refLink("x", { title: "why" }).title).toBe("why");
  });

  it("catChip: the slot's swatch and its word; bare, the word is its name and tooltip", () => {
    const c = catChip(3, "pitfall");
    expect(c.className).toBe("cat c3");
    expect(c.textContent).toBe("pitfall");
    const bare = catChip(5, "preference", { bare: true });
    expect(bare.className).toBe("cat bare c5");
    expect(bare.textContent).toBe("");
    expect(bare.getAttribute("role")).toBe("img");
    expect(bare.getAttribute("aria-label")).toBe("preference");
    expect(bare.title).toBe("preference");
  });
});

describe("ui/markdown - markdownNode", () => {
  const tags = (el: Element): string[] => Array.from(el.children).map((c) => c.tagName.toLowerCase());
  /** markdown-it puts a newline after each block tag; the shape is what the panel relies on. */
  const flat = (el: Element): string => el.innerHTML.replace(/\n/g, "");
  const html = (src: string): string => flat(markdownNode(src));
  const inline = (src: string): string => flat(markdownNode(src).querySelector("p")!);

  it("draws the blocks a brief is written in: headings, paragraphs, lists, rules, quotes", () => {
    const md = markdownNode("# Brief\n\n2 live notes · 1 need review\n\n## Rules\n\n- one\n- **two**\n\n---\n\n> quoted\n> still", { class: "brief" });
    expect(md.className).toBe("md brief");
    expect(tags(md)).toEqual(["h1", "p", "h2", "ul", "hr", "blockquote"]);
    expect(md.querySelector("h1")!.textContent).toBe("Brief");
    expect(Array.from(md.querySelectorAll("li")).map((l) => l.innerHTML)).toEqual(["one", "<strong>two</strong>"]);
    expect(flat(md.querySelector("blockquote")!)).toBe("<p>quoted<br>still</p>");
    expect(markdownNode("").className).toBe("md");
  });

  it("an author's line break stays; a blank line is a new paragraph", () => {
    expect(html("a\nb\n\nc")).toBe("<p>a<br>b</p><p>c</p>");
    expect(html("Use UTC\n- in the log")).toBe("<p>Use UTC</p><ul><li>in the log</li></ul>");
  });

  it("lists: ordered from where they start, nested by indent, a loose list keeps its paragraphs", () => {
    const md = markdownNode("3. three\n4. four\n   - under\n   - also\n5. five");
    const ol = md.querySelector("ol")!;
    expect(ol.getAttribute("start")).toBe("3");
    expect(tags(ol)).toEqual(["li", "li", "li"]);
    expect(flat(ol.children[1])).toBe("four<ul><li>under</li><li>also</li></ul>");
    expect(markdownNode("1. one").querySelector("ol")!.hasAttribute("start")).toBe(false);
    expect(html("- a\n\n  more\n- b")).toBe("<ul><li><p>a</p><p>more</p></li><li><p>b</p></li></ul>");
    expect(html("- a\n1. b")).toBe("<ul><li>a</li></ul><ol><li>b</li></ol>");
  });

  it("fenced code is kept verbatim with its language; markup inside it stays text", () => {
    const pre = markdownNode("```rust\nlet a = **b**;\n  <i>x</i>\n```").querySelector("pre")!;
    expect(pre.className).toBe("md-code");
    expect(pre.dataset.lang).toBe("rust");
    expect(pre.querySelector("code")!.textContent).toBe("let a = **b**;\n  <i>x</i>");
    expect(pre.querySelector("i, strong")).toBeNull();
    expect(markdownNode("~~~\nx\n~~~").querySelector("pre")!.hasAttribute("data-lang")).toBe(false);
    expect(markdownNode("    indented\n    code").querySelector("pre.md-code > code")!.textContent).toBe("indented\ncode");
    // An unclosed fence runs to the end of the note (CommonMark).
    expect(markdownNode("```\na\nb").querySelector("code")!.textContent).toBe("a\nb");
  });

  it("pipe tables: a header, its rows, an escaped pipe kept, a short row padded, wrapped to scroll", () => {
    const t = markdownNode("| key | value |\n|:---|---:|\n| `a\\|b` | 1 |\n| c |").querySelector(".md-table > table")!;
    expect(Array.from(t.querySelectorAll("th")).map((c) => c.textContent)).toEqual(["key", "value"]);
    const rows = Array.from(t.querySelectorAll("tbody tr")).map((r) => Array.from(r.children).map((c) => c.innerHTML));
    expect(rows).toEqual([["<code>a|b</code>", "1"], ["c", ""]]);
    // A pipe in a sentence is not a table.
    expect(tags(markdownNode("a | b\nnot a rule"))).toEqual(["p"]);
  });

  it("inline: code, strong, emphasis, strikethrough, an escape; snake_case and a lone star are text", () => {
    expect(inline("run `cargo test` **now**, *then* _ship_ ~~never~~"))
      .toBe("run <code>cargo test</code> <strong>now</strong>, <em>then</em> <em>ship</em> <s>never</s>");
    expect(inline("``a ` b``")).toBe("<code>a ` b</code>");
    expect(inline("snake_case_name and 2 * 3 * 4")).toBe("snake_case_name and 2 * 3 * 4");
    expect(inline("\\*not em\\* and `unclosed")).toBe("*not em* and `unclosed");
    expect(inline("**bold with `code`**")).toBe("<strong>bold with <code>code</code></strong>");
    // A star inside a code span closes nothing (a brief's marker names a glob anchor so).
    expect(inline("*anchor changed: `src/*.rs`*")).toBe("<em>anchor changed: <code>src/*.rs</code></em>");
  });

  it("a URL written with its scheme is a link; a file name or a bare address is not", () => {
    const md = markdownNode("see https://example.com/a?b=1. and www.example.org, pipeline.py, a@b.c");
    expect(Array.from(md.querySelectorAll("a")).map((a) => a.getAttribute("href"))).toEqual(["https://example.com/a?b=1"]);
    expect(md.querySelector("p")!.textContent).toBe("see https://example.com/a?b=1. and www.example.org, pipeline.py, a@b.c");
  });

  it("never a note's HTML: tags are text, no image is fetched, a link opens only http, https or mailto", () => {
    const md = markdownNode("<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n[ok](https://example.com) [bad](javascript:alert(1)) [mail](mailto:a@b.c) <javascript:alert(2)>\n\n![pixel](https://example.com/p.png)");
    expect(md.querySelector("script, img")).toBeNull();
    expect(md.children[0].textContent).toBe("<script>alert(1)</script>");
    expect(md.children[1].textContent).toBe("<img src=x onerror=alert(1)>");
    const links = Array.from(md.querySelectorAll("a"));
    expect(links.map((a) => a.getAttribute("href"))).toEqual(["https://example.com", "mailto:a@b.c", "https://example.com/p.png"]);
    expect(links.every((a) => a.target === "_blank" && a.rel === "noopener noreferrer")).toBe(true);
    // A refused link is left as the text it was written as.
    expect(md.children[2].textContent).toBe("ok [bad](javascript:alert(1)) mail <javascript:alert(2)>");
    // An image is never drawn: what is left is its words, linked to where it lives.
    expect(md.children[3].textContent).toBe("!pixel");
  });

  it("a mermaid fence becomes its diagram, in mermaid's sandbox, redrawn on a theme switch", async () => {
    mermaidFake.initialize.mockClear();
    const fence = (src: string): string => "```mermaid\n" + src + "\n```";
    const md = markdownNode(["Flow", fence("flowchart TD\n  A --> B"), fence("not a diagram"), fence("escape svg"), fence("escape scripts")].join("\n\n"));
    document.body.append(md);
    try {
      await vi.waitFor(() => expect(md.querySelectorAll("pre[title]").length).toBe(3));
      expect(mermaidFake.initialize).toHaveBeenLastCalledWith(expect.objectContaining({
        startOnLoad: false, securityLevel: "sandbox", suppressErrorRendering: true, theme: "default",
      }));
      expect(tags(md)).toEqual(["p", "div", "pre", "pre", "pre"]);
      const fig = md.querySelector(".md-mermaid")!;
      const frame = fig.querySelector("iframe")!;
      expect(fig.children.length).toBe(1);
      expect(frame.getAttribute("sandbox")).toBe("allow-top-navigation-by-user-activation allow-popups");
      expect(frame.title).toBe("flowchart TD\n  A --> B");
      // A diagram that does not parse, or comes back outside a sandbox, stays as its code; the reason is on hover.
      const kept = Array.from(md.querySelectorAll<HTMLElement>("pre[data-lang=mermaid]"));
      expect(kept.map((p) => [p.textContent, p.title])).toEqual([
        ["not a diagram", "Parse error on line 1"],
        ["escape svg", "mermaid drew outside its sandbox"],
        ["escape scripts", "mermaid drew outside its sandbox"],
      ]);
      expect(md.querySelector("svg")).toBeNull();

      document.documentElement.setAttribute("data-theme", "dark");
      await vi.waitFor(() => expect(mermaidFake.initialize).toHaveBeenLastCalledWith(expect.objectContaining({ theme: "dark" })));
      await vi.waitFor(() => expect(md.querySelector(".md-mermaid")).not.toBe(fig));
    } finally {
      document.documentElement.removeAttribute("data-theme");
      md.remove();
    }
  });
});

describe("ui/diff - diffFiles, diffNode", () => {
  function q(sel: string, root: Element): HTMLElement {
    const el = root.querySelector<HTMLElement>(sel);
    if (!el) throw new Error("no " + sel);
    return el;
  }
  const PATCH = [
    "diff --git a/src/a.rs b/src/a.rs",
    "index 1..2 100644",
    "--- a/src/a.rs",
    "+++ b/src/a.rs",
    "@@ -12,3 +12,4 @@ fn a() {",
    "     let k = 1;",
    "-    let n = 3;",
    "+    let n = cfg.n();",
    "+    // <script>alert(1)</script>",
    "     run(k, n);",
    "\\ No newline at end of file",
    "diff --git a/docs/b.md b/docs/b.md",
    "new file mode 100644",
    "--- /dev/null",
    "+++ b/docs/b.md",
    "@@ -0,0 +1 @@",
    "+# B",
    "diff --git a/logo.png b/logo.png",
    "deleted file mode 100644",
    "Binary files a/logo.png and /dev/null differ",
    "",
  ].join("\n");

  it("reads git's patch file by file: path, status, counts, and both sides' line numbers", () => {
    const files = diffFiles(PATCH);
    expect(files.map((f) => [f.path, f.status, f.additions, f.deletions])).toEqual([
      ["src/a.rs", "modified", 2, 1], ["docs/b.md", "added", 1, 0], ["logo.png", "deleted", 0, 0],
    ]);
    const lines = files[0].lines.map((l) => [l.kind, l.o ?? null, l.n ?? null]);
    expect(lines).toEqual([
      ["hunk", null, null], ["ctx", 12, 12], ["del", 13, null], ["add", null, 13], ["add", null, 14], ["ctx", 14, 15], ["note", null, null],
    ]);
    // A header line is not a line of the file; a binary file says so.
    expect(files[1].lines.map((l) => l.text)).toEqual(["@@ -0,0 +1 @@", "+# B"]);
    expect(files[2].lines.map((l) => l.kind)).toEqual(["note"]);
    expect(diffFiles("")).toEqual([]);
  });

  it("draws a fold per file, numbers from data attributes, the patch's text as text", () => {
    const node = diffNode(PATCH);
    const folds = Array.from(node.querySelectorAll<HTMLDetailsElement>("details.diff-file"));
    expect(folds.map((d) => d.dataset.dpath)).toEqual(["src/a.rs", "docs/b.md", "logo.png"]);
    // A small patch opens every file.
    expect(folds.every((d) => d.open)).toBe(true);
    expect(q(".diff-st", folds[1]).textContent).toBe("added");
    expect(q(".diff-n", folds[0]).textContent).toBe("+2 −1");
    const add = q(".dl.add", folds[0]);
    expect([add.dataset.o, add.dataset.n]).toEqual(["", "13"]);
    // What a drag-selection copies: the lines as the patch has them - never the numbers, never markup.
    expect(q("pre.jv.diff-body", folds[0]).textContent).toBe(PATCH.split("\n").slice(4, 11).join("\n") + "\n");
    expect(node.querySelector("script")).toBeNull();
    // Opened for one file, only that one is open.
    const one = diffNode(PATCH, { open: "docs/b.md" });
    expect(Array.from(one.querySelectorAll<HTMLDetailsElement>("details")).map((d) => d.open)).toEqual([false, true, false]);
    install("zh-CN", zh);
    expect(q(".diff-st", diffNode(PATCH)).textContent).toBe(zh["ui.diff.modified"]);
  });

  it("the code block's mark, the wide sheet and a ref drawn as code", () => {
    const code = tokenCodeNode([["", "a\nb\nc"]], { all: true, lined: true, mark: 2 }).node;
    expect(Array.from(code.querySelectorAll(".jv-line")).map((l) => l.className)).toEqual(["jv-line", "jv-line hit", "jv-line"]);
    expect(sheetFrame({ title: "f.rs", body: null, foot: null, wide: true }).className).toBe("sheet wide");
    expect(sheetFrame({ title: "f.rs", body: null, foot: null }).className).toBe("sheet");
    const ref = refLink("src/a.rs", { code: true, data: { mpvfile: "src/a.rs" } });
    expect(ref.innerHTML).toBe("<code>src/a.rs</code>");
    expect(ref.title).toBe("src/a.rs");
  });
});

describe("ui/graph", () => {
  const nodes: GraphNode[] = [
    { id: "a", label: "Orders are idempotent", tone: 1 },
    { id: "b", label: "Retry with the same key", tone: 2, pending: true },
    { id: "c", label: "Never retry a refund", tone: 3, dim: true },
    { id: "d", label: "Use UTC", tone: 4, title: "Use UTC - knowledge" },
    { id: "e", label: "Prefer small PRs", tone: 5 },
  ];
  const edges: GraphEdge[] = [
    { from: "a", to: "b", style: "solid", directed: true, label: "supports", title: "supports: same key" },
    { from: "b", to: "c", style: "heavy", directed: true },
    { from: "a", to: "c", style: "dashed", alert: true, label: "contradicts" },
    { from: "c", to: "d", style: "dotted" },
    { from: "a", to: "zz", style: "faint" },
    { from: "a", to: "b", style: "solid", directed: true, label: "supports" },
  ];

  afterEach(() => { document.body.replaceChildren(); });

  it("graphHops: rings over the edges either way; the unreached keep the caller's hop or sit past the farthest", () => {
    const hops = graphHops({ nodes: nodes.concat([{ id: "f", label: "f", tone: 1, hop: 4 }]), edges, center: "a" });
    expect(Object.fromEntries(hops)).toEqual({ a: 0, b: 1, c: 1, d: 2, e: 3, f: 4 });
    expect(graphHops({ nodes, edges, center: null }).size).toBe(0);
    expect(graphHops({ nodes, edges, center: "nobody" }).size).toBe(0);
  });

  it("graphElements: a node per node with its classes; an edge per drawable, distinct link", () => {
    const els = graphElements({ nodes, edges, label: "g" });
    const ns = els.filter((e) => e.group === "nodes");
    // A whole graph: each node's drawn links (the repeat of a-b and the line to zz do not count),
    // a dot that grows with them, and `hub` on the most linked quarter with two links or more.
    expect(ns.map((n) => [n.data.id, n.classes, n.data.links, n.data.size])).toEqual([
      ["a", "t1 hub", 2, dotSize(2)], ["b", "t2 pending", 2, dotSize(2)], ["c", "t3 dim hub", 3, dotSize(3)], ["d", "t4", 1, dotSize(1)], ["e", "t5", 0, 14],
    ]);
    expect(dotSize(4)).toBeGreaterThan(dotSize(1));
    expect(ns[3].data.title).toBe("Use UTC - knowledge");
    expect(ns[0].data.title).toBe("Orders are idempotent");
    // Without a center the force layout starts from a spiral: the first node in the middle.
    expect(ns[0].position).toEqual({ x: 0, y: 0 });
    expect(new Set(ns.map((n) => JSON.stringify(n.position))).size).toBe(5);
    const es = els.filter((e) => e.group === "edges");
    // The edge to a node that is not drawn is dropped, and so is the repeat of a-b.
    expect(es.map((e) => [e.data.source, e.data.target, e.classes])).toEqual([
      ["a", "b", "solid directed"], ["b", "c", "heavy directed"], ["a", "c", "dashed alert"], ["c", "d", "dotted"],
    ]);
    expect(es[0].data.label).toBe("supports");
    expect(es[0].data.title).toBe("supports: same key");
    const centred = graphElements({ nodes, edges, center: "a", label: "g" });
    expect(centred.filter((e) => e.group === "nodes").map((n) => [n.classes, n.data.hop, n.position, n.data.size])).toEqual([
      ["t1 center", 0, undefined, undefined], ["t2 pending", 1, undefined, undefined], ["t3 dim", 1, undefined, undefined],
      ["t4", 2, undefined, undefined], ["t5", 3, undefined, undefined],
    ]);
    const long = graphElements({ nodes: [{ id: "x", label: "x".repeat(80), tone: 1 }], edges: [], label: "g" });
    expect(String(long[0].data.label).length).toBe(64);
    expect(String(long[0].data.label).endsWith("…")).toBe(true);
  });

  it("graphLayoutOf: rings around a center, the center innermost; a force layout without one", () => {
    const rings = graphLayoutOf({ nodes, edges, center: "a", label: "g" });
    expect(rings.name).toBe("concentric");
    const level = rings.concentric as (n: GraphEls) => number;
    const at = (hop: number): GraphEls => ({ data: () => hop }) as unknown as GraphEls;
    expect(level(at(0))).toBeGreaterThan(level(at(1)));
    expect(level(at(1))).toBeGreaterThan(level(at(3)));
    const force = graphLayoutOf({ nodes, edges, label: "g" });
    expect(force.name).toBe("cose");
    expect(force.randomize).toBe(false);
    expect(force.animate).toBe(false);
    expect(graphLayoutOf({ nodes, edges, center: "nobody", label: "g" }).name).toBe("cose");
  });

  it("graphPalette reads the tokens and falls back to the light theme's; mixHex blends to an opaque hex", () => {
    const p = graphPalette((n) => (n === "--cat-1" ? "#000000" : n === "--card" ? "#ffffff" : ""));
    expect(p.tones[0]).toBe("#000000");
    expect(p.tones[1]).toBe("#109d7b");
    expect(p.card).toBe("#ffffff");
    expect(p.red).toBe("#dc2626");
    expect(mixHex("#000000", "#ffffff", 0.5)).toBe("#808080");
    expect(mixHex("#ff0000", "#0000ff", 1)).toBe("#ff0000");
    expect(mixHex("red", "#ffffff", 0.5)).toBe("#ffffff");
  });

  it("graphStyle: a tinted chip per tone, the link styles, a conflict in red, the selection's lift", () => {
    const p = graphPalette(() => "");
    const st = graphStyle(p);
    const by = (sel: string): Record<string, unknown> | undefined => st.find((b) => b.selector === sel)?.style;
    expect(by("node.t1")).toEqual({ "border-color": "#0c84fa", "background-color": mixHex("#0c84fa", "#ffffff", 0.12) });
    expect(by("edge")?.label).toBe("data(label)");
    expect(by("edge.directed")?.["target-arrow-shape"]).toBe("triangle");
    expect(by("edge.dashed")?.["line-style"]).toBe("dashed");
    expect(by("edge.alert")?.["line-color"]).toBe("#dc2626");
    expect(by("node.pending")?.["border-style"]).toBe("dashed");
    expect(by(".faded")?.opacity).toBeLessThan(0.5);
    expect(Number(graphStyle(p, { compact: true }).find((b) => b.selector === "node")?.style["font-size"])).toBeLessThan(Number(by("node")?.["font-size"]));
    expect(st.some((b) => b.selector === "node.hub")).toBe(false);
  });

  it("graphStyle map: dots by links, names one size on screen; the hubs' always, the rest zoomed in or near", () => {
    const p = graphPalette(() => "");
    let zoom = 0.5;
    const st = graphStyle(p, { map: true, zoom: () => zoom });
    // The last block for a selector wins in the engine, as here.
    const by = (sel: string): Record<string, unknown> => Object.assign({}, ...st.filter((b) => b.selector === sel).map((b) => b.style));
    const el = (classes: string[], size = 20): GraphEls => ({ hasClass: (c: string) => classes.includes(c), data: () => size }) as unknown as GraphEls;
    const call = (sel: string, prop: string, e: GraphEls = el([])): unknown => (by(sel)[prop] as (x: GraphEls) => unknown)(e);
    expect(by("node").shape).toBe("ellipse");
    expect(by("node")["text-valign"]).toBe("bottom");
    expect(call("node", "font-size")).toBe(23);
    expect(call("node", "text-max-width")).toBe("340px");
    expect(call("node", "width", el([], 20))).toBeCloseTo(20 / Math.sqrt(0.5));
    expect(call("node", "text-opacity", el(["hub"]))).toBe(1);
    expect(call("node", "text-opacity", el(["near"]))).toBe(1);
    expect(call("node", "text-opacity", el(["hover"]))).toBe(1);
    expect(call("node", "text-opacity", el([]))).toBe(0);
    expect(call("edge", "text-opacity", el(["hl"]))).toBe(1);
    expect(call("edge", "text-opacity", el([]))).toBe(0);
    expect(by("node.t1")["background-color"]).toBe(mixHex("#0c84fa", "#ffffff", 0.45));
    expect(by("node.sel")["underlay-shape"]).toBe("ellipse");
    // Zoomed in to where the layout gave every name its room: all named, at their model size.
    zoom = 1;
    expect(call("node", "text-opacity", el([]))).toBe(1);
    expect(call("edge", "text-opacity", el([]))).toBe(1);
    expect(call("node", "font-size")).toBe(11.5);
    expect(call("node", "width", el([], 20))).toBe(20);
    zoom = 2;
    expect(call("node", "font-size")).toBe(5.75);
  });

  it("graphLegend names every tone and stroke; a solid line adds no class of its own", () => {
    const lg = graphLegend([{ label: "decision", tone: 1 }, { label: "supports", style: "solid", directed: true }, { label: "contradicts", style: "dashed", alert: true }]);
    expect(Array.from(lg.querySelectorAll(":scope > .gv-key")).map((k) => k.textContent)).toEqual(["decision", "supports", "contradicts"]);
    expect(Array.from(lg.querySelectorAll(".gv-sw")).map((s) => s.className)).toEqual([
      "gv-sw gv-t1", "gv-sw gv-line gv-dir", "gv-sw gv-line gv-dashed gv-alert",
    ]);
  });

  it("graphView: a quiet line until the engine draws; then the elements, the layout and the tools", async () => {
    const fake = fakeEngine();
    const v = graphView({ nodes, edges, selected: "b", label: "the graph", keys: [{ label: "decision", tone: 1 }] }, { engine: fake.engine });
    expect(v.el.className).toBe("gv");
    const stage = v.el.querySelector(".gv-stage") as HTMLElement;
    expect(stage.getAttribute("role")).toBe("group");
    expect(stage.getAttribute("aria-label")).toBe("the graph");
    const wait = v.el.querySelector(".gv-wait") as HTMLElement;
    expect(wait.hidden).toBe(false);
    expect(Array.from(v.el.querySelectorAll(".gv-tools [data-gvzoom]")).map((b) => (b as HTMLElement).dataset.gvzoom)).toEqual(["in", "out", "fit"]);
    expect(v.el.querySelector(".gv-legend")).not.toBeNull();
    document.body.append(v.el);
    await drawn();
    expect(fake.cores.length).toBe(1);
    const core = fake.cores[0];
    expect(core.opts.container).toBe(v.el.querySelector(".gv-canvas"));
    expect(core.opts.layout.name).toBe("cose");
    expect(core.opts.userZoomingEnabled).toBe(true);
    expect(wait.hidden).toBe(true);
    // The selection given up front: the node lifted with its links and neighbours, the rest quiet.
    expect(core.cls("b")).toContain("sel");
    expect(core.cls("a>b>supports")).toContain("hl");
    expect(core.cls("e")).toContain("faded");
    expect(core.cls("a")).not.toContain("faded");
    (v.el.querySelector("[data-gvzoom=in]") as HTMLElement).click();
    expect(core.zoomLevel).toBeCloseTo(1.25);
    (v.el.querySelector("[data-gvzoom=fit]") as HTMLElement).click();
    expect(core.fits).toBe(1);
    v.destroy();
    expect(core.destroyed).toBe(true);
  });

  it("graphView: a tap selects in place and says so, the background clears, a double tap opens", async () => {
    const fake = fakeEngine();
    const picked: Array<string | null> = [];
    const opened: string[] = [];
    const v = graphView({ nodes, edges, label: "g" }, { engine: fake.engine, onSelect: (id) => { picked.push(id); }, onOpen: (id) => { opened.push(id); } });
    document.body.append(v.el);
    await drawn();
    const core = fake.cores[0];
    expect(core.cls("a")).not.toContain("faded");
    core.fire("tap", "d");
    expect(picked).toEqual(["d"]);
    expect(core.cls("d")).toContain("sel");
    expect(core.cls("c")).not.toContain("faded");
    expect(core.cls("a")).toContain("faded");
    core.fire("tap", null);
    expect(picked).toEqual(["d", null]);
    expect(core.cls("d")).not.toContain("sel");
    expect(core.cls("a")).not.toContain("faded");
    core.fire("dbltap", "c");
    expect(opened).toEqual(["c"]);
    core.fire("mouseover", "d");
    expect((v.el.querySelector(".gv-stage") as HTMLElement).title).toBe("Use UTC - knowledge");
    // select() from outside moves the same marks, no redraw.
    v.select("e");
    expect(core.cls("e")).toContain("sel");
    expect(fake.cores.length).toBe(1);
    v.destroy();
  });

  it("graphView: compact has no tools and no zoom; a centered graph rings; a failed engine says so", async () => {
    const fake = fakeEngine();
    const v = graphView({ nodes, edges, center: "a", label: "g", compact: true }, { engine: fake.engine });
    expect(v.el.className).toBe("gv compact");
    expect(v.el.querySelector(".gv-tools, .gv-legend")).toBeNull();
    document.body.append(v.el);
    await drawn();
    expect(fake.cores[0].opts.layout.name).toBe("concentric");
    expect(fake.cores[0].opts.userZoomingEnabled).toBe(false);
    expect(fake.cores[0].opts.autoungrabify).toBe(true);
    v.destroy();
    expect(graphView({ nodes, edges, label: "g", fill: true }, { engine: fake.engine }).el.className).toBe("gv fill");
    const bad = graphView({ nodes, edges, label: "g" }, { engine: () => Promise.reject(new Error("no")) });
    await drawn();
    const wait = bad.el.querySelector(".gv-wait") as HTMLElement;
    expect(wait.classList.contains("bad")).toBe(true);
    expect(wait.textContent).toBe("Couldn't load the graph engine - reload the page to try again.");
  });

  it("graphView: a whole graph laid out against its pane turns a quarter; a matching one and rings stay", async () => {
    // A column, as cose leaves a chain, in the default 800 x 600 (wide) pane.
    const column = (_id: string, i: number) => ({ x: 10, y: i * 100 });
    const fake = fakeEngine({ place: column });
    const v = graphView({ nodes, edges, label: "g" }, { engine: fake.engine });
    document.body.append(v.el);
    await drawn();
    const core = fake.cores[0];
    expect(nodes.map((n) => core.at(n.id))).toEqual(nodes.map((_, i) => ({ x: i * 100, y: -10 })));
    expect(core.fits).toBe(1);
    // A layout that stops later (the engine ran it a tick after) is checked again: already turned.
    core.fire("layoutstop", null);
    expect(core.at("e")).toEqual({ x: 400, y: -10 });
    expect(core.fits).toBe(1);
    v.destroy();
    // The same column in a tall pane already runs along it.
    const tall = fakeEngine({ place: column, size: { w: 400, h: 900 } });
    document.body.append(graphView({ nodes, edges, label: "g" }, { engine: tall.engine }).el);
    await drawn();
    expect(tall.cores[0].at("e")).toEqual({ x: 10, y: 400 });
    expect(tall.cores[0].fits).toBe(0);
    // Rings around a center are round by design: never turned.
    const ringed = fakeEngine({ place: column });
    document.body.append(graphView({ nodes, edges, center: "a", label: "g" }, { engine: ringed.engine }).el);
    await drawn();
    expect(ringed.cores[0].at("e")).toEqual({ x: 10, y: 400 });
  });

  it("graphView map: restyled at draw and once a frame while the zoom moves; the pointer and the selection name", async () => {
    const fake = fakeEngine();
    const v = graphView({ nodes, edges, selected: "b", label: "g" }, { engine: fake.engine });
    document.body.append(v.el);
    await drawn();
    const core = fake.cores[0];
    // The sheet built before the engine had a zoom: reapplied once it has one.
    expect(core.restyles).toBe(1);
    core.fire("zoom", null);
    core.fire("zoom", null);
    await new Promise((r) => { requestAnimationFrame(() => { r(null); }); });
    expect(core.restyles).toBe(2);
    // The selection's neighbours are named; the rest of the graph is not lifted.
    expect(["a", "b", "c"].map((id) => core.cls(id).includes("near"))).toEqual([true, true, true]);
    expect(core.cls("d")).not.toContain("near");
    core.fire("mouseover", "d");
    expect(core.cls("d")).toContain("hover");
    core.fire("mouseout", "d");
    expect(core.cls("d")).not.toContain("hover");
    v.destroy();
    // Rings around a center keep their chips: nothing sized against the zoom.
    const ringed = fakeEngine();
    document.body.append(graphView({ nodes, edges, center: "a", label: "g" }, { engine: ringed.engine }).el);
    await drawn();
    ringed.cores[0].fire("zoom", null);
    await new Promise((r) => { requestAnimationFrame(() => { r(null); }); });
    expect(ringed.cores[0].restyles).toBe(0);
  });

  /* --- boxes: a whole graph by group ---------------------------------------------------------- */

  const groups: GraphGroup[] = [
    { id: "pay/refunds", label: "Refunds", parent: "pay" },
    { id: "pay", label: "Payments", title: "pay - money in and out" },
    { id: "ops", label: "Operations" },
    { id: "web", label: "Web", parent: "nowhere" },
  ];
  const homes: Array<string | undefined> = ["pay", "pay", "pay/refunds", "web", undefined];
  const grouped: GraphNode[] = nodes.map((n, i) => (homes[i] ? { ...n, group: homes[i] } : n));

  it("graphElements with groups: the boxes first, parents before children; each node in its own; no hubs", () => {
    const els = graphElements({ nodes: grouped, edges, groups, label: "g" });
    const ns = els.filter((e) => e.group === "nodes");
    // "ops" holds nothing: not drawn. "web" names a parent nobody gave: a box at the top.
    expect(ns.slice(0, 3).map((n) => [n.data.id, n.classes, n.data.parent, n.data.label, n.data.title])).toEqual([
      ["group:pay", "group", undefined, "Payments", "pay - money in and out"],
      ["group:web", "group", undefined, "Web", "Web"],
      ["group:pay/refunds", "group sub", "group:pay", "Refunds", "Refunds"],
    ]);
    // A box is at least its inside wide - one dot's is a cell, room for its name - and a parent's
    // holds its sub-box.
    expect(ns[2].data.width).toBeGreaterThan(100);
    expect(Number(ns[0].data.width)).toBeGreaterThan(Number(ns[2].data.width));
    expect(ns.slice(3).map((n) => [n.data.id, n.data.parent, n.classes])).toEqual([
      ["a", "group:pay", "t1"], ["b", "group:pay", "t2 pending"], ["c", "group:pay/refunds", "t3 dim"], ["d", "group:web", "t4"], ["e", undefined, "t5"],
    ]);
    expect(new Set(ns.slice(3).map((n) => JSON.stringify(n.position))).size).toBe(5);
    // Around a center there are no boxes: rings by hop.
    const centred = graphElements({ nodes: grouped, edges, groups, center: "a", label: "g" });
    expect(centred.some((e) => (e.classes || "").includes("group") || e.data.parent)).toBe(false);
    // Groups that hold nobody draw nothing, and the hubs are back.
    const plain = graphElements({ nodes, edges, groups, label: "g" });
    expect(plain.some((e) => String(e.data.id).startsWith("group:"))).toBe(false);
    expect(plain.find((e) => e.data.id === "a")?.classes).toBe("t1 hub");
    expect(groupElementId("pay")).toBe("group:pay");
  });

  it("packBoxes: members on a grid, a sub-box after its parent's own, no box over another, the biggest first", () => {
    const home = (i: number): string | undefined => (i < 2 ? "small" : i < 9 ? "big" : i < 11 ? "big/sub" : undefined);
    const many: GraphNode[] = Array.from({ length: 12 }, (_, i) => ({ id: "n" + i, label: "n" + i, tone: 1, group: home(i) }));
    const boxes: GraphGroup[] = [{ id: "small", label: "Small" }, { id: "big", label: "Big" }, { id: "big/sub", label: "Sub", parent: "big" }];
    const spec: GraphSpec = { nodes: many, edges: [], groups: boxes, label: "g" };
    const { at, width } = packBoxes(spec);
    expect(at.size).toBe(12);
    expect(new Set(Array.from(at.values()).map((p) => JSON.stringify(p))).size).toBe(12);
    const pos = (id: string): { x: number; y: number } => at.get(id) ?? { x: NaN, y: NaN };
    // A grid: the next member to the right, then a row down under the first.
    expect(pos("n3").y).toBe(pos("n2").y);
    expect(pos("n3").x).toBeGreaterThan(pos("n2").x);
    expect(pos("n4").x).toBe(pos("n2").x);
    expect(pos("n4").y).toBeGreaterThan(pos("n2").y);
    // The biggest box (nine with its sub-box's) is the first, top left.
    const first = Array.from(at.entries()).sort((a, b) => a[1].x + a[1].y - (b[1].x + b[1].y))[0][0];
    expect(first).toBe("n2");
    // Each box's extent: its members, a cell round each. The sub-box's sit past the parent's own,
    // inside the parent; the small box overlaps neither.
    // A cell: the dot 18 in from its left, the name to the right; 230 by 44.
    const extent = (ids: string[]): { x1: number; x2: number; y1: number; y2: number } => ({
      x1: Math.min(...ids.map((id) => pos(id).x)) - 18, x2: Math.max(...ids.map((id) => pos(id).x)) + 212,
      y1: Math.min(...ids.map((id) => pos(id).y)) - 22, y2: Math.max(...ids.map((id) => pos(id).y)) + 22,
    });
    const ids = (f: (i: number) => boolean): string[] => many.filter((_, i) => f(i)).map((n) => n.id);
    const own = extent(ids((i) => home(i) === "big"));
    const sub = extent(ids((i) => home(i) === "big/sub"));
    const big = extent(ids((i) => !!home(i)?.startsWith("big")));
    const small = extent(ids((i) => home(i) === "small"));
    expect(sub.y1 >= own.y2 || sub.x1 >= own.x2).toBe(true);
    const apart = (a: typeof big, b: typeof big): boolean => a.x2 <= b.x1 || b.x2 <= a.x1 || a.y2 <= b.y1 || b.y2 <= a.y1;
    expect(apart(big, small)).toBe(true);
    // The node in no box: placed, unboxed.
    expect(at.has("n11")).toBe(true);
    expect(Array.from(width.keys()).sort()).toEqual(["big", "big/sub", "small"]);
    expect(width.get("big")).toBeGreaterThan(width.get("big/sub") ?? Infinity);
    // The same data, the same picture; the pane's shape sets the shelves: a wide pane puts the
    // small box beside the big one, a tall one under it.
    expect(packBoxes(spec).at).toEqual(at);
    const wide = packBoxes(spec, 6).at;
    expect(wide.get("n0")?.y).toBe(wide.get("n2")?.y);
    const tall = packBoxes(spec, 0.5).at;
    expect(tall.get("n0")?.x).toBe(tall.get("n2")?.x);
    expect(tall.get("n0")?.y).toBeGreaterThan(big.y2);
  });

  it("packBoxes: short boxes stack beside a tall one, not on a shelf under it", () => {
    const home = (i: number): string => (i < 12 ? "tall" : ["a", "b", "c"][i - 12]);
    const many: GraphNode[] = Array.from({ length: 15 }, (_, i) => ({ id: "n" + i, label: "n" + i, tone: 1, group: home(i) }));
    const boxes: GraphGroup[] = ["tall", "a", "b", "c"].map((id) => ({ id, label: id }));
    const at = packBoxes({ nodes: many, edges: [], groups: boxes, label: "g" }).at;
    const pos = (id: string): { x: number; y: number } => at.get(id) ?? { x: NaN, y: NaN };
    const tallRight = Math.max(...many.slice(0, 12).map((n) => pos(n.id).x));
    const tallBottom = Math.max(...many.slice(0, 12).map((n) => pos(n.id).y));
    // a, b and c in one column right of the tall box, each under the last; b, which a shelf would
    // have put under the tall box, beside its notes.
    expect(pos("n12").x).toBeGreaterThan(tallRight);
    expect([pos("n13").x, pos("n14").x]).toEqual([pos("n12").x, pos("n12").x]);
    expect(pos("n12").y).toBeLessThan(pos("n13").y);
    expect(pos("n13").y).toBeLessThan(pos("n14").y);
    expect(pos("n13").y).toBeLessThan(tallBottom);
  });

  it("graphLayoutOf: boxes stay where packBoxes put them; rings around a center still", () => {
    expect(graphLayoutOf({ nodes: grouped, edges, groups, label: "g" })).toEqual({ name: "preset", animate: false, fit: true, padding: 30 });
    expect(graphLayoutOf({ nodes: grouped, edges, groups, center: "a", label: "g" }).name).toBe("concentric");
    expect(graphLayoutOf({ nodes, edges, groups, label: "g" }).name).toBe("cose");
  });

  it("graphStyle map: a box named at one size on screen, sized without its members' names; the lines quiet", () => {
    const p = graphPalette(() => "");
    let zoom = 0.5;
    const st = graphStyle(p, { map: true, zoom: () => zoom });
    const by = (sel: string): Record<string, unknown> => Object.assign({}, ...st.filter((b) => b.selector === sel).map((b) => b.style));
    const box = by("node.group");
    expect(box["compound-sizing-wrt-labels"]).toBe("exclude");
    expect(box["min-width"]).toBe("data(width)");
    expect(box["min-width-bias-right"]).toBe("100%");
    expect(box.label).toBe("data(label)");
    expect(box["text-valign"]).toBe("top");
    expect(box["text-opacity"]).toBe(1);
    expect((box["font-size"] as () => number)()).toBe(25);
    // Its edge holds the bottom row's names: further out as the zoom falls, within the room packed.
    expect((box.padding as () => string)()).toBe("40px");
    expect(by("node.group.sub")["background-color"]).not.toBe(box["background-color"]);
    // A sub-box's name a step quieter than its parent's: never read as the parent's second line.
    const sub = by("node.group.sub");
    expect((sub["font-size"] as () => number)()).toBeLessThan(25);
    expect(sub["font-weight"]).toBeLessThan(box["font-weight"] as number);
    expect(sub.color).not.toBe(box.color);
    expect(by("edge").width).toBe(1);
    expect(by("edge").opacity).toBeLessThan(0.5);
    expect(by("edge.hl").opacity).toBe(1);
    expect(by("edge.faded").opacity).toBeLessThan(by("edge").opacity as number);
    expect(graphStyle(p).some((b) => b.selector === "node.group")).toBe(false);
    // Boxed, every note is named while its cell shows enough of a name, kept to the cell; the one
    // pointed at or picked gets its whole name. Unboxed, the same note is not named at this zoom.
    const el = (classes: string[]): GraphEls => ({ hasClass: (x: string) => classes.includes(x), data: () => 20 }) as unknown as GraphEls;
    const node = (sheet: typeof st): Record<string, unknown> =>
      Object.assign({}, ...sheet.filter((b) => b.selector === "node" || b.selector === "node:childless").map((b) => b.style));
    const boxed = node(graphStyle(p, { map: true, boxed: true, zoom: () => zoom }));
    const call = (prop: string, e: GraphEls): unknown => (boxed[prop] as (x: GraphEls) => unknown)(e);
    expect(call("text-opacity", el([]))).toBe(1);
    expect(call("text-max-width", el([]))).toBe("180px");
    // A row: the name right of its dot, the notes drawn over the lines; a big dot kept to its row.
    expect([boxed["text-halign"], boxed["text-valign"], boxed["z-compound-depth"]]).toEqual(["right", "center", "top"]);
    expect(call("width", ({ hasClass: () => false, data: () => 40 }) as unknown as GraphEls)).toBe(34);
    expect(node(st)["text-halign"]).toBe("center");
    expect(call("text-max-width", el(["hover"]))).toBe("340px");
    expect(call("text-max-width", el(["sel"]))).toBe("340px");
    expect((node(st)["text-opacity"] as (x: GraphEls) => unknown)(el([]))).toBe(0);
    zoom = 0.3;
    expect(call("text-opacity", el([]))).toBe(0);
    expect(call("text-opacity", el(["hover"]))).toBe(1);
    zoom = 2;
    expect(call("text-max-width", el([]))).toBe("85px");
    expect((by("node.group").padding as () => string)()).toBe("16px");
    zoom = 0.8;
    expect((by("node.group").padding as () => string)()).toBe("27px");
    expect(graphStyle(p, { map: true, boxed: true, zoom: () => zoom }).find((b) => b.selector === "node.hover:childless")?.style["z-index"]).toBe(20);
  });

  it("graphView boxed: packed, never turned; a box is background to a tap; the selection's boxes are never quiet", async () => {
    const column = (_id: string, i: number) => ({ x: 10, y: i * 100 });
    const fake = fakeEngine({ place: column });
    const picked: Array<string | null> = [];
    const opened: string[] = [];
    const v = graphView({ nodes: grouped, edges, groups, label: "g" }, { engine: fake.engine, onSelect: (id) => { picked.push(id); }, onOpen: (id) => { opened.push(id); } });
    document.body.append(v.el);
    await drawn();
    const core = fake.cores[0];
    expect(core.opts.layout.name).toBe("preset");
    // A wide pane and a column: a whole graph without boxes turns it; boxes keep theirs.
    expect(grouped.every((n) => core.at(n.id)?.x === 10)).toBe(true);
    expect(core.fits).toBe(0);
    core.fire("tap", "c");
    expect(picked).toEqual(["c"]);
    expect(core.cls("c")).toContain("sel");
    expect(core.cls("e")).toContain("faded");
    expect(core.cls("group:pay")).not.toContain("faded");
    expect(core.cls("group:pay/refunds")).not.toContain("faded");
    expect(core.cls("group:web")).not.toContain("faded");
    core.fire("tap", "group:pay");
    expect(picked).toEqual(["c", null]);
    expect(core.cls("c")).not.toContain("sel");
    expect(core.cls("group:pay")).not.toContain("sel");
    core.fire("dbltap", "group:pay");
    expect(opened).toEqual([]);
    core.fire("dbltap", "c");
    expect(opened).toEqual(["c"]);
    v.destroy();
  });

  it("graphView: a frame that left the page releases its engine at the next draw", async () => {
    const fake = fakeEngine();
    const one = graphView({ nodes, edges, label: "g" }, { engine: fake.engine });
    document.body.append(one.el);
    await drawn();
    one.el.remove();
    graphView({ nodes, edges, label: "g" }, { engine: fake.engine });
    expect(fake.cores[0].destroyed).toBe(true);
  });
});

describe("ui/group - groupNode", () => {
  it("page density: the group IS the card; the band holds toggle, + and ⋯", () => {
    const p = groupNode({ name: "learn", label: "Learn", count: 2, density: "page", addTitle: "Add to learn", moreTitle: "Group actions", data: { scope: "jobs" } },
      row({ name: "a" }), row({ name: "b" }));
    expect(p.root.className).toBe("grp grp--page group");
    expect(p.root.dataset.group).toBe("learn");
    expect(p.root.dataset.scope).toBe("jobs");
    expect(p.head.parentElement).toBe(p.root);
    expect(p.toggle.getAttribute("aria-expanded")).toBe("true");
    expect(p.toggle.querySelector(".grp-chev use")?.getAttribute("href")).toBe("#i-chevron-right");
    expect(p.toggle.querySelector(".grp-name")?.textContent).toBe("Learn");
    expect(p.toggle.querySelector(".grp-n")?.textContent).toBe("2");
    expect(p.add?.title).toBe("Add to learn");
    expect(p.add?.getAttribute("aria-label")).toBe("Add to learn");
    expect(p.more?.getAttribute("aria-label")).toBe("Group actions");
    expect(Array.from(p.head.children)).toEqual([p.toggle, p.add, p.more]);
    expect(p.body.querySelectorAll(":scope > .lrow").length).toBe(2);
    expect(p.empty).toBeNull();
  });

  it("side density, folded, empty, read-only: no card class, no buttons, the quiet line", () => {
    const p = groupNode({ name: "Views", count: 0, density: "side", collapsed: true, emptyText: "No views" });
    expect(p.root.className).toBe("grp grp--side collapsed");
    expect(p.toggle.getAttribute("aria-expanded")).toBe("false");
    expect(p.toggle.querySelector(".grp-name")?.textContent).toBe("Views");
    expect(p.add).toBeNull();
    expect(p.more).toBeNull();
    expect(p.empty?.className).toBe("grp-empty");
    expect(p.empty?.parentElement).toBe(p.body);
    expect(p.body.lastElementChild).toBe(p.empty);
  });
});

describe("ui/tab - objTab (SPEC §panel.pages)", () => {
  it("a role=tab with its glyph, name, count and mark, and a real close button beside them", () => {
    const t = objTab({
      name: "orders", selected: true, icon: "table", title: "demo_shop.orders", data: { dbtab: "0" },
      count: { n: 2, title: "2 filters" }, mark: heldDot("1 buffered change"),
      close: { label: "Close tab", data: { dbtabx: "0" } },
    });
    expect(t.tagName).toBe("DIV");
    expect(t.className).toBe("otab sel");
    expect(t.getAttribute("role")).toBe("tab");
    expect(t.getAttribute("aria-selected")).toBe("true");
    expect(t.tabIndex, "the open tab is the strip's one Tab stop").toBe(0);
    expect(t.title).toBe("demo_shop.orders");
    expect(t.dataset.dbtab).toBe("0");
    expect(Array.from(t.children).map((c) => c.getAttribute("class"))).toEqual(["ic", "otab-name", "otab-n tnum", "db-tab-dot", "otab-close"]);
    expect(t.querySelector(".otab-n")!.textContent).toBe("2");
    expect(t.querySelector(".otab-n")!.getAttribute("title")).toBe("2 filters");
    const x = t.querySelector<HTMLButtonElement>("button.otab-close")!;
    expect(x.type).toBe("button");
    expect(x.getAttribute("aria-label")).toBe("Close tab");
    expect(x.dataset.dbtabx).toBe("0");
    expect(useHref(x), "the close is the sprite, never a glyph").toBe("#i-x");
  });

  it("a tab that is not open leaves the Tab order; a name that is a node goes in as it is", () => {
    const input = h("input", { class: "x-rename" });
    const t = objTab({ name: input, selected: false, data: {}, close: { label: "Close", data: {} } });
    expect(t.className).toBe("otab");
    expect(t.tabIndex).toBe(-1);
    expect(t.getAttribute("aria-selected")).toBe("false");
    expect(t.querySelector(".otab-name")).toBeNull();
    expect(t.firstElementChild).toBe(input);
  });

  it("tnum is base.css's tabular-figures utility, and no sheet spells the OpenType tag as a value", () => {
    // Found in SPEC §panel.pages: rule 1's `tnum` was a class with no rule (Data's activity pid and
    // duration cells, the tab count), and two views.css rules wrote `font-variant-numeric: tnum`,
    // which is not a value - the browser drops the declaration.
    const rule = parseCss(sheet("base.css")).find((r) => !r.at && r.selectors.includes(".tnum"));
    expect(rule?.decls.find((d) => d.prop === "font-variant-numeric")?.value).toBe("tabular-nums");
    for (const name of ["base.css", "ui.css", "views.css"] as const) {
      expect(sheet(name), name).not.toMatch(/font-variant-numeric:\s*tnum/);
    }
  });

  it("the open tab merges into the surface below it, a 2px accent on its top edge (ui.css)", () => {
    const rules = parseCss(sheet("ui.css")).filter((r) => !r.at);
    const sel = rules.find((r) => r.selectors.includes(".otab.sel"));
    expect(sel?.decls.find((d) => d.prop === "background")?.value).toBe("var(--bg)");
    expect(sel?.decls.find((d) => d.prop === "box-shadow")?.value).toBe("inset 0 2px 0 var(--accent)");
  });
});

describe("ui/seg", () => {
  it("a tablist whose buttons carry the page's own data hook and the selection", () => {
    const s = seg([
      { id: "tools", label: "Tools", n: 12 },
      { id: "logs", label: "Logs" },
      { id: "run", label: "Run", hidden: true, title: "Needs a running server" },
    ], "logs", { key: "pane", label: "Server sections", id: "srvSeg" });
    expect(s.className).toBe("seg");
    expect(s.id).toBe("srvSeg");
    expect(s.getAttribute("role")).toBe("tablist");
    expect(s.getAttribute("aria-label")).toBe("Server sections");
    const bs = Array.from(s.querySelectorAll<HTMLButtonElement>("button"));
    expect(bs.map((b) => b.dataset.pane)).toEqual(["tools", "logs", "run"]);
    expect(bs.map((b) => b.getAttribute("aria-selected"))).toEqual(["false", "true", "false"]);
    expect(bs.every((b) => b.getAttribute("role") === "tab" && b.type === "button")).toBe(true);
    expect(bs[0].querySelector(".seg-n")?.textContent).toBe("12");
    expect(bs[1].querySelector(".seg-n")).toBeNull();
    expect(bs[2].hidden).toBe(true);
    expect(bs[2].title).toBe("Needs a running server");
  });

  it("the hook defaults to data-seg", () => {
    const s = seg([{ id: "all", label: "Everything" }], "all");
    expect((s.querySelector("button") as HTMLButtonElement).dataset.seg).toBe("all");
  });

  it("fill is a class the page asks for, not a width it restyles (SPEC §panel.ui)", () => {
    expect(seg([{ id: "daily", label: "Daily" }], "daily", { fill: true }).className).toBe("seg fill");
    expect(seg([{ id: "x", label: "X" }], "x").className).toBe("seg");
  });
});

/* A fixed local "now": 2026-09-23 10:00, so the day arithmetic is the reader's local days. */
const NOW = new Date(2026, 8, 23, 10, 0, 0).getTime();
const at = (d: number, hh: number, mm = 0, ss = 0): number => new Date(2026, 8, d, hh, mm, ss).getTime();

function item(id: string, when: number, extra: Partial<TimelineItem> = {}): TimelineItem {
  return Object.assign({ id, at: when, title: "mysql_query" }, extra);
}

describe("ui/timeline - the helpers", () => {
  it("dayLabel: today and yesterday are LOCAL days, not 24-hour windows", () => {
    expect(dayLabel(at(23, 0, 30), NOW)).toBe("Today");
    expect(dayLabel(at(22, 23, 50), NOW)).toBe("Yesterday");
    expect(dayLabel(at(22, 0, 0), NOW)).toBe("Yesterday");
    const older = dayLabel(at(21, 12), NOW);
    expect(older).toContain("Sep");
    expect(older).toContain("21");
    expect(older).not.toContain("2026");
    expect(dayLabel(new Date(2025, 11, 31, 9).getTime(), NOW)).toContain("2025");
  });

  it("local midnight and the year boundary retain natural-day labels; invalid legacy dates are unknown", () => {
    const midnight = new Date(2027, 0, 1, 0, 5).getTime();
    expect(dayLabel(new Date(2026, 11, 31, 23, 59).getTime(), midnight)).toBe("Yesterday");
    expect(dayLabel(new Date(2027, 0, 1, 0, 1).getTime(), midnight)).toBe("Today");
    expect(dayLabel(new Date(2026, 11, 30).getTime(), midnight)).toContain("2026");
    for (const invalid of [NaN, Infinity, -Infinity]) expect(dayLabel(invalid, midnight)).toBe(tr("ui.timeUnknown"));
  });

  it("dayLabel speaks the installed language", () => {
    install("zh-CN", zh);
    expect(dayLabel(at(23, 1), NOW)).toBe("今天");
    expect(dayLabel(at(22, 1), NOW)).toBe("昨天");
  });

  it("timeLabel is 24-hour HH:MM:SS", () => {
    expect(timeLabel(at(23, 14, 2, 11))).toBe("14:02:11");
    expect(timeLabel(at(23, 9, 5, 7))).toBe("09:05:07");
  });

  it("fmtMs: whole ms, then one decimal of seconds, then whole seconds - cut where rounding lands", () => {
    expect([0, 12.4, 999.4, 999.6, 1234, 9949, 9950, 61234].map(fmtMs))
      .toEqual(["0 ms", "12 ms", "999 ms", "1.0 s", "1.2 s", "9.9 s", "10 s", "61 s"]);
  });

  it("collapseRuns folds consecutive equal signatures within one day, keeping order", () => {
    const runs = collapseRuns([
      item("a", at(23, 9, 3), { same: "q" }),
      item("b", at(23, 9, 2), { same: "q" }),
      item("c", at(23, 9, 1), { same: "r" }),
      item("d", at(23, 9, 0)),
      item("e", at(23, 8, 59)),
      item("f", at(22, 23, 59), { same: "r" }),
    ]);
    expect(runs.map((r) => r.map((i) => i.id).join(""))).toEqual(["ab", "c", "d", "e", "f"]);
    // Same signature across midnight: two days, two rows.
    const split = collapseRuns([item("x", at(23, 0, 1), { same: "s" }), item("y", at(22, 23, 59), { same: "s" })]);
    expect(split.length).toBe(2);
  });
});

describe("ui/timeline - timeline()", () => {
  it("a long argument or output never widens the timeline, nor the sheet around it", () => {
    // Found on a walk of SPEC §panel.pages (a job's history, a multi-line claude reply): an implicit
    // grid column is `auto`, and an auto track never shrinks below its item's min-content - the
    // no-wrap summary line's full width. The row grew to 728 px in a 560 px sheet and was cut at
    // its edge, ellipsis and Output block with it. Both grids give their one column a 0 minimum.
    const css = sheet("ui.css");
    expect(css).toMatch(/\.tl \{ display: grid; grid-template-columns: minmax\(0, 1fr\); \}/);
    expect(css).toMatch(/\.sheet-body \{[^}]*display: grid; grid-template-columns: minmax\(0, 1fr\);/);
  });

  it("one day heading per local day, rows beneath it, the date never repeated per row", () => {
    const tl = timeline([item("a", at(23, 9)), item("b", at(23, 8)), item("c", at(22, 17))], { now: NOW });
    expect(tl.className).toBe("tl");
    const kids = Array.from(tl.children).map((k) => k.className === "tl-day" ? "#" + k.textContent : k.getAttribute("data-tl-id"));
    expect(kids).toEqual(["#Today", "a", "b", "#Yesterday", "c"]);
    expect(timeline([item("a", at(23, 9))], { now: NOW, dayHeads: false }).querySelector(".tl-day")).toBeNull();
  });

  it("a row: chevron, time, title, argument, the ms - and aria-expanded on the summary", () => {
    const tl = timeline([item("a", at(23, 14, 2, 11), { arg: '{"sql":"select 1"}', ms: 12, data: { call: "7" } })], { now: NOW });
    const it0 = tl.querySelector(".tl-item") as HTMLElement;
    expect(it0.dataset.tlId).toBe("a");
    expect(it0.dataset.call).toBe("7");
    const sum = it0.querySelector(":scope > button.tl-sum") as HTMLButtonElement;
    expect(sum.type).toBe("button");
    expect(sum.getAttribute("aria-expanded")).toBe("false");
    expect(Array.from(sum.children).map((c) => c.className)).toEqual(["tl-chev", "tl-time", "tl-title", "tl-arg", "tl-ms"]);
    expect(sum.querySelector("time.tl-time")?.textContent).toBe("14:02:11");
    expect(sum.querySelector("time")?.getAttribute("datetime")).toBe(new Date(at(23, 14, 2, 11)).toISOString());
    expect((sum.querySelector(".tl-arg") as HTMLElement).title).toBe('{"sql":"select 1"}');
    expect(sum.querySelector(".tl-ms")?.textContent).toBe("12 ms");
    expect(it0.querySelector(".tl-body")).toBeNull();
  });

  it("live: a run still in flight is a pulse and its word where its duration will go", () => {
    // SPEC §panel.pages: Remote Runs lists the runs the coordinator still holds above the record.
    // Such a run has no duration yet; its state (running, or queued behind a busy slot) sits in
    // that column - the amber pulse for running, the hollow dot for waiting - never a tag, which
    // is for an outcome.
    const tl = timeline([
      item("a", at(23, 9), { live: { text: "running" } }),
      item("b", at(23, 8), { live: { text: "queued", queued: true } }),
    ], { now: NOW });
    const [a, b] = Array.from(tl.querySelectorAll(".tl-sum"));
    expect(Array.from(a.children).map((c) => c.className)).toEqual(["tl-chev", "tl-time", "tl-title", "tl-arg", "tl-live"]);
    expect(a.querySelector(".tl-live")?.textContent).toBe("running");
    expect(a.querySelector(".tl-live > .dot")?.className).toBe("dot starting");
    expect(b.querySelector(".tl-live > .dot")?.className).toBe("dot idle");
    expect(a.querySelector(".tag")).toBeNull();
    expect(sheet("ui.css")).toMatch(/\.tl-live \{[^}]*display: inline-flex;/);
  });

  it("hit: a search result's matched line, inside the summary, the match marked", () => {
    // Remote Runs' search (SPEC §remote.panel): a row that only says `bash -s` cannot say why it is
    // in the results - the line that matched does. Inside the summary, so a click on it opens the row.
    const tl = timeline([
      item("a", at(23, 9), { arg: "bash -s", hit: { before: "Active: active (", match: "running", after: ") since Sat" } }),
      item("b", at(23, 8), { arg: "bash -s" }),
    ], { now: NOW });
    const [a, b] = Array.from(tl.querySelectorAll<HTMLElement>(".tl-sum"));
    expect(a.className).toBe("tl-sum has-hit");
    const hit = a.lastElementChild as HTMLElement;
    expect(hit.className).toBe("tl-hit");
    expect(hit.textContent).toBe("Active: active (running) since Sat");
    expect(hit.querySelector("mark")?.textContent).toBe("running");
    expect(b.className).toBe("tl-sum");
    expect(b.querySelector(".tl-hit")).toBeNull();
    // Only a row with a reason wraps onto a second line; every other summary stays one line.
    const css = sheet("ui.css");
    expect(css).toMatch(/\.tl-sum\.has-hit \{ flex-wrap: wrap;/);
    expect(css).not.toMatch(/\.tl-sum \{[^}]*flex-wrap/);
  });

  it("who appears only when the loaded items disagree about it (or when forced)", () => {
    const one = [item("a", at(23, 9), { who: "claude-code" }), item("b", at(23, 8), { who: "claude-code" })];
    expect(timeline(one, { now: NOW }).querySelector(".tl-who")).toBeNull();
    expect(timeline(one, { now: NOW, showWho: true }).querySelectorAll(".tl-who").length).toBe(2);
    const two = [item("a", at(23, 9), { who: "claude-code" }), item("b", at(23, 8), { who: "cursor" })];
    const who = Array.from(timeline(two, { now: NOW }).querySelectorAll<HTMLElement>(".tl-who"));
    expect(who.map((w) => w.textContent)).toEqual(["claude-code", "cursor"]);
    expect(who[1].title).toBe("cursor");
  });

  it("×N for a folded run; a failure is a red tag, not a red row; slow is amber", () => {
    const tl = timeline([
      item("a", at(23, 9, 3), { same: "q", ms: 1500, status: { text: "error", tone: "bad" } }),
      item("b", at(23, 9, 2), { same: "q" }),
      item("c", at(23, 9, 1), { same: "q" }),
      item("d", at(23, 9, 0), { ms: 200 }),
    ], { now: NOW });
    const rows = Array.from(tl.querySelectorAll<HTMLElement>(".tl-item"));
    expect(rows.map((r) => r.dataset.tlId)).toEqual(["a", "d"]);
    const n = rows[0].querySelector(".tl-n") as HTMLElement;
    expect(n.textContent).toBe("×3");
    expect(n.title).toBe("3 identical in a row");
    expect(rows[0].querySelector(".tag.bad")?.textContent).toBe("error");
    expect(rows[0].className).toBe("tl-item");
    expect(rows[0].querySelector(".tl-ms")?.className).toBe("tl-ms slow");
    expect(rows[1].querySelector(".tl-ms")?.className).toBe("tl-ms");
    expect(rows[1].querySelector(".tl-n")).toBeNull();
    const strict = timeline([item("a", at(23, 9), { ms: 200 })], { now: NOW, slowMs: 100 });
    expect(strict.querySelector(".tl-ms")?.className).toBe("tl-ms slow");
  });

  it("an open row paints the body the view owns, handed the whole run", () => {
    const seen: string[] = [];
    const tl = timeline([item("a", at(23, 9), { same: "q" }), item("b", at(23, 8), { same: "q" })], {
      now: NOW, open: new Set(["a"]),
      body: (it, run) => { seen.push(it.id + ":" + run.length); return "the full request"; },
    });
    const it0 = tl.querySelector(".tl-item") as HTMLElement;
    expect(it0.className).toBe("tl-item open");
    expect(it0.querySelector(".tl-sum")?.getAttribute("aria-expanded")).toBe("true");
    expect(it0.querySelector(":scope > .tl-body")?.textContent).toBe("the full request");
    expect(seen).toEqual(["a:2"]);
  });

  it("the Chinese pass: ×N's tooltip and the durations come from the table", () => {
    install("zh-CN", zh);
    const tl = timeline([item("a", at(23, 9), { same: "q", ms: 2500 }), item("b", at(23, 8), { same: "q" })], { now: NOW });
    expect((tl.querySelector(".tl-n") as HTMLElement).title).toBe("连续 2 次相同");
    expect(tl.querySelector(".tl-day")?.textContent).toBe("今天");
    expect(tl.querySelector(".tl-ms")?.textContent).toBe("2.5 s");
  });
});

describe("ui/timeline - timelineToggle", () => {
  it("opens with the body it is handed, closes and drops it, and answers the new state", () => {
    const tl = timeline([item("a", at(23, 9)), item("b", at(23, 8))], { now: NOW });
    expect(timelineToggle(tl, "b", "detail")).toBe(true);
    const b = tl.querySelector('[data-tl-id="b"]') as HTMLElement;
    expect(b.classList.contains("open")).toBe(true);
    expect(b.querySelector(".tl-sum")?.getAttribute("aria-expanded")).toBe("true");
    expect(b.querySelector(":scope > .tl-body")?.textContent).toBe("detail");
    expect(timelineToggle(tl, "b")).toBe(false);
    expect(b.classList.contains("open")).toBe(false);
    expect(b.querySelector(".tl-sum")?.getAttribute("aria-expanded")).toBe("false");
    expect(b.querySelector(".tl-body")).toBeNull();
    expect(tl.querySelector('[data-tl-id="a"]')?.classList.contains("open")).toBe(false);
  });

  it("an unknown id changes nothing", () => {
    const tl = timeline([item("a", at(23, 9))], { now: NOW });
    const before = tl.outerHTML;
    expect(timelineToggle(tl, "zzz", "x")).toBe(false);
    expect(tl.outerHTML).toBe(before);
  });

  it("an open body stacks its blocks a grid step apart (ui.css)", () => {
    // A call's body is its arguments AND its reply - two code blocks. As a plain block the body
    // drew them edge to edge, one grey slab (found in the gallery's resource scene, P1b-3).
    const rule = parseCss(sheet("ui.css")).find((r) => !r.at && r.selectors.includes(".tl-body"));
    const decl = (p: string): string | undefined => rule?.decls.find((d) => d.prop === p)?.value;
    expect(decl("display")).toBe("flex");
    expect(decl("flex-direction")).toBe("column");
    expect(decl("gap")).toBe("var(--s2)");
  });
});

/** The P1b-2 mechanisms draw into the document (a menu on <body>, a select's face beside it
 *  and its list on <body>, a sheet in #sheet): render each with every option and hand back the
 *  roots, the field sheet with its inline error showing. */
function mechanisms(): Element[] {
  document.body.innerHTML = '<div id="sheet" class="backdrop" hidden></div><div id="host"></div>';
  popupMenu({ left: 0, top: 0, bottom: 0 }, [
    { label: "h", fn: () => {}, heading: true }, { label: "a", fn: () => {}, icon: "table", dot: true, affordance: "chevron-right" },
    { label: "p", fn: () => {}, pick: true, on: true }, { label: "x", fn: () => {}, disabled: true }, { sep: true },
    { label: "d", fn: () => {}, danger: true },
  ]);
  const menu = document.getElementById("menu")!;
  const sel = document.getElementById("host")!.appendChild(h("select", null, h("option", { value: "a" }, "a")));
  styleSelect(sel);
  (sel.nextElementSibling as HTMLButtonElement).click();
  const list = document.querySelector(".dd-menu")!;
  openFieldSheet({ title: "t", submit: () => true });
  (document.getElementById("g-save") as HTMLButtonElement).click(); // empty: the red hint shows
  const out = [menu, document.getElementById("host")!, list, document.getElementById("sheet")!.firstElementChild!];
  const snap = out.map((n) => n.cloneNode(true) as Element);
  closeMenu();
  closeSheet();
  return snap;
}

describe("ui/menu - anchoredMenu", () => {
  it("keeps every menu's rows in a scroll region and restores focus on Escape", () => {
    document.body.innerHTML = '<button id="opener">Open</button>';
    const opener = document.getElementById("opener")!;
    opener.focus();
    popupMenu({ left: 950, top: 600, bottom: 620 }, Array.from({ length: 80 }, (_, n) => ({ label: String(n), fn: () => {} })));
    const menu = document.getElementById("menu")!;
    expect(menu.querySelectorAll(".menu-rows button")).toHaveLength(80);
    menu.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(document.activeElement).toBe(opener);
  });
  it("hangs the menu in the host, not on <body>: same rows, focus on the first, Escape closes", () => {
    document.body.innerHTML = '<div class="pane-actions" id="host"></div>';
    const host = document.getElementById("host")!;
    let ran = "";
    anchoredMenu(host, [
      { label: "Group", fn: () => {}, heading: true },
      { label: "default", fn: () => { ran = "default"; }, pick: true, on: true },
      { sep: true },
      { label: "Delete", fn: () => { ran = "delete"; }, danger: true },
    ]);
    const menu = document.getElementById("menu")!;
    expect(menu.parentElement).toBe(host);
    expect(menu.className).toBe("menu float");
    expect(menu.getAttribute("role")).toBe("menu");
    expect(menuOpen()).toBe(true);
    expect(document.activeElement?.textContent).toBe("default");
    menu.querySelector<HTMLButtonElement>("button.danger")!.click();
    expect(ran).toBe("delete");
    expect(document.getElementById("menu")).toBeNull();
    expect(menuOpen()).toBe(false);
    anchoredMenu(host, [{ label: "a", fn: () => {} }]);
    document.activeElement!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(menuOpen()).toBe(false);
  });
});

describe("ui/sheet - stackSheet", () => {
  it("moves focus into the new layer, keeps it through repaint, and restores the opener on Escape", () => {
    const parent = h("div", { id: "sheet" });
    const opener = h("button", null, "open a preview");
    parent.appendChild(opener);
    document.body.appendChild(parent);
    opener.focus();
    const layer = stackSheet();
    const input = h("input", { type: "text" });
    layer.paint({ title: "Preview", body: input, foot: [] });
    const back = document.querySelector<HTMLElement>(".backdrop.stacked")!;
    expect(back.contains(document.activeElement), "a real Escape starts at the focused node").toBe(true);
    input.focus();
    layer.paint({ title: "Preview loaded", body: "loaded", foot: [] });
    expect(back.contains(document.activeElement), "repaint cannot send Escape to the parent").toBe(true);
    document.activeElement!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(document.querySelector(".backdrop.stacked")).toBeNull();
    expect(parent.isConnected).toBe(true);
    expect(parent.hidden).toBe(false);
    expect(document.activeElement).toBe(opener);
    parent.remove();
  });

  it("a second layer over the open sheet: its own backdrop, Escape closes it alone and stops there", () => {
    document.body.innerHTML = "";
    let closedCb = 0;
    let reachedDocument = 0;
    const onDoc = (e: KeyboardEvent): void => { if (e.key === "Escape") reachedDocument++; };
    document.addEventListener("keydown", onDoc);
    const layer = stackSheet(() => { closedCb++; });
    const node = layer.paint({ title: "Pick", body: h("input", { id: "p" }), foot: btn("Cancel") });
    const back = node.parentElement!;
    expect(back.className).toBe("backdrop stacked");
    expect(node.getAttribute("role")).toBe("dialog");
    // A repaint replaces the dialog inside the same layer.
    const again = layer.paint({ title: "Pick 2", body: null, foot: null });
    expect(back.children.length).toBe(1);
    expect(again.querySelector("h2")!.textContent).toBe("Pick 2");
    again.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(document.querySelector(".backdrop.stacked")).toBeNull();
    expect(closedCb).toBe(1);
    expect(reachedDocument, "the shell's Escape chain must not also close the sheet underneath").toBe(0);
    document.removeEventListener("keydown", onDoc);
  });
});

describe("ui/to-top - toTop", () => {
  it("a labelled round button that shows only past one screen of its scroller", () => {
    const scroller = document.createElement("div");
    Object.defineProperty(scroller, "clientHeight", { value: 400 });
    const b = toTop(scroller);
    expect(b.classList.contains("to-top")).toBe(true);
    expect(b.getAttribute("aria-label")).toBe("Back to top");
    expect(b.querySelector("use")?.getAttribute("href")).toBe("#i-arrow-up");
    scroller.scrollTop = 400;
    scroller.dispatchEvent(new Event("scroll"));
    expect(b.classList.contains("on")).toBe(false);
    scroller.scrollTop = 401;
    scroller.dispatchEvent(new Event("scroll"));
    expect(b.classList.contains("on")).toBe(true);
  });

  it("jumps rather than glides when the reader asked for reduced motion", () => {
    const scroller = document.createElement("div");
    const calls: unknown[] = [];
    scroller.scrollTo = ((o: unknown) => { calls.push(o); }) as typeof scroller.scrollTo;
    const real = window.matchMedia;
    window.matchMedia = ((q: string) => ({ matches: q.includes("reduce") })) as unknown as typeof window.matchMedia;
    try { toTop(scroller).click(); } finally { window.matchMedia = real; }
    expect(calls).toEqual([{ top: 0, behavior: "auto" }]);
  });

  it("hidden means out of the Tab order too, and it sits under every overlay (ui.css)", () => {
    const rule = parseCss(sheet("ui.css")).find((r) => !r.at && r.selectors.includes(".btn.to-top"));
    const decl = (p: string): string | undefined => rule?.decls.find((d) => d.prop === p)?.value;
    expect(decl("position")).toBe("fixed");
    expect(decl("visibility")).toBe("hidden");
    // Menus 30, the sheet backdrop 40, the toast 50.
    expect(Number(decl("z-index"))).toBeLessThan(30);
    const on = parseCss(sheet("ui.css")).find((r) => !r.at && r.selectors.includes(".btn.to-top.on"));
    expect(on?.decls.find((d) => d.prop === "visibility")?.value).toBe("visible");
  });
});

describe("ui/grip - grip", () => {
  afterEach(() => { localStorage.clear(); delete document.body.dataset.grip; });
  const pane = (): HTMLElement => { const p = document.createElement("aside"); p.id = "gripPane"; return p; };
  const press = (g: HTMLElement, key: string): void => { g.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true })); };
  const pointer = (g: HTMLElement, type: string, at: { x?: number; y?: number }): void => {
    g.dispatchEvent(new MouseEvent(type, { button: 0, clientX: at.x ?? 0, clientY: at.y ?? 0, bubbles: true }));
  };
  const stored = (key: string): unknown => JSON.parse(localStorage.getItem("swiss_grip_" + key) ?? "null");

  it("a labelled separator that leaves the stylesheet's size alone until the reader moves it, then remembers", () => {
    const p = pane();
    const g = grip(p, { axis: "x", key: "t1", min: 100, max: 400, label: "Tables" });
    expect(g.classList.contains("grip") && g.classList.contains("x")).toBe(true);
    expect(g.getAttribute("role")).toBe("separator");
    expect(g.getAttribute("aria-orientation")).toBe("vertical");
    expect(g.getAttribute("aria-controls")).toBe("gripPane");
    expect(g.getAttribute("aria-label")).toBe("Resize Tables");
    expect(g.querySelector(".grip-btn"), "no fold, no chevron").toBeNull();
    expect(p.style.width, "the stylesheet's default stands").toBe("");
    // Nothing is laid out here, so the measured width is 0 and the arrows start at the minimum.
    press(g, "ArrowRight");
    expect(p.style.width).toBe("116px");
    expect(stored("t1")).toEqual({ size: 116, folded: false });
    press(g, "End");
    expect(p.style.width).toBe("400px");
    press(g, "Home");
    expect(p.style.width).toBe("100px");
    // The page paints again: the new grip reads the remembered width back.
    const again = pane();
    grip(again, { axis: "x", key: "t1", min: 100, max: 400, label: "Tables" });
    expect(again.style.width).toBe("100px");
    expect(gripState({ axis: "x", key: "t1", min: 100, max: 400, label: "Tables" })).toEqual({ size: 100, folded: false });
  });

  it("a y edge sizes its pane's flex basis; a drag keeps to the bounds, holds the cursor and announces the change once", () => {
    const p = pane();
    const g = grip(p, { axis: "y", key: "t2", size: 220, min: 96, max: () => 500, label: "Console" });
    expect(g.getAttribute("aria-orientation")).toBe("horizontal");
    expect(p.style.flex).toBe("0 1 220px");
    let resizes = 0;
    const onResize = (): void => { resizes++; };
    window.addEventListener("resize", onResize);
    try {
      pointer(g, "pointerdown", { y: 300 });
      expect(document.body.dataset.grip).toBe("y");
      pointer(g, "pointermove", { y: 400 });
      expect(p.style.flex).toBe("0 1 320px");
      pointer(g, "pointermove", { y: 2000 });
      expect(p.style.flex, "never past max").toBe("0 1 500px");
      pointer(g, "pointermove", { y: -900 });
      expect(p.style.flex, "never under min").toBe("0 1 96px");
      expect(resizes, "nothing announced mid-drag").toBe(0);
      pointer(g, "pointerup", { y: -900 });
    } finally { window.removeEventListener("resize", onResize); }
    expect(document.body.dataset.grip).toBeUndefined();
    expect(resizes).toBe(1);
    expect(stored("t2")).toEqual({ size: 96, folded: false });
    // Without fold a double-click puts the default back.
    g.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    expect(p.style.flex).toBe("0 1 220px");
  });

  it("a foldable pane folds by double-click, Enter, its chevron or a drag past half its minimum; the folded strip's click brings it back", () => {
    const p = pane();
    const g = grip(p, { axis: "x", key: "t3", size: 200, min: 160, max: 500, label: "Tables", fold: true });
    const chevron = g.querySelector<HTMLElement>("button.grip-btn");
    expect(chevron?.getAttribute("aria-label")).toBe("Hide Tables");
    expect(chevron?.tabIndex, "the edge itself is the tab stop").toBe(-1);
    g.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    expect(p.style.display).toBe("none");
    expect(g.classList.contains("folded")).toBe(true);
    expect(g.getAttribute("aria-label")).toBe("Show Tables");
    expect(chevron?.getAttribute("aria-label")).toBe("Show Tables");
    expect(useHref(g)).toBe("#i-chevron-right");
    expect(stored("t3")).toEqual({ size: 200, folded: true });
    g.click();
    expect(p.style.display).toBe("");
    expect(p.style.width, "the width it had").toBe("200px");
    press(g, "Enter");
    expect(g.classList.contains("folded")).toBe(true);
    press(g, "ArrowRight");
    expect(g.classList.contains("folded"), "a folded edge has no width to change").toBe(true);
    press(g, "Enter");
    expect(g.classList.contains("folded")).toBe(false);
    chevron?.click();
    expect(g.classList.contains("folded")).toBe(true);
    g.click();
    // The drag: under the minimum it stops at the minimum, past half of it the pane folds -
    // and the click the release makes does not unfold it again.
    pointer(g, "pointerdown", { x: 300 });
    pointer(g, "pointermove", { x: 280 });
    expect(p.style.width).toBe("180px");
    pointer(g, "pointermove", { x: 200 });
    expect(p.style.width).toBe("160px");
    pointer(g, "pointermove", { x: 150 });
    expect(p.style.display).toBe("none");
    pointer(g, "pointerup", { x: 150 });
    g.click();
    expect(g.classList.contains("folded")).toBe(true);
    // The fold keeps the width the drag started from, not the minimum it crossed on the way.
    expect(stored("t3")).toEqual({ size: 200, folded: true });
    g.click();
    expect(g.classList.contains("folded")).toBe(false);
    expect(p.style.width).toBe("200px");
  });

  it("a press on the mid-edge chevron that moves is a drag, not a fold; one that stays is the chevron's click", () => {
    // Found on the 19998 walk: the chevron sits where a hand grabs the edge, and a drag that
    // started on it did nothing - first because the press was left to the button, then because
    // a capture taken only at the first move missed a hand whose first move left the button.
    const p = pane();
    const g = grip(p, { axis: "x", key: "t5", size: 200, min: 160, max: 500, label: "Tables", fold: true });
    const chevron = g.querySelector<HTMLElement>("button.grip-btn");
    if (!chevron) throw new Error("no chevron");
    pointer(chevron, "pointerdown", { x: 300 });
    pointer(g, "pointermove", { x: 360 }); // captured: the edge hears the move wherever it lands
    pointer(g, "pointerup", { x: 360 });
    g.click(); // the release's click lands on the edge that held the pointer
    expect(p.style.width).toBe("260px");
    expect(g.classList.contains("folded")).toBe(false);
    // A press that stays put is the chevron's click, answered at the release; the click after
    // it is the release's own and does not fold it back.
    pointer(chevron, "pointerdown", { x: 300 });
    pointer(g, "pointerup", { x: 300 });
    expect(g.classList.contains("folded")).toBe(true);
    g.click();
    expect(g.classList.contains("folded")).toBe(true);
    g.click(); // the folded strip's click
    expect(g.classList.contains("folded")).toBe(false);
    // No press before it (a keyboard, a screen reader): the chevron's click is the toggle.
    chevron.click();
    expect(g.classList.contains("folded")).toBe(true);
  });

  it("an end edge sizes the pane after it: a drag towards the start widens it, the arrows swap, the chevron points out; folded until opened, a page control folds it", () => {
    const p = pane();
    const o = { axis: "x" as const, key: "t6", size: 400, min: 280, max: 720, label: "Agent", fold: true, folded: true, end: true };
    const g = grip(p, o);
    expect(g.className).toBe("grip x end folded");
    expect(p.style.display, "folded while nothing is remembered").toBe("none");
    expect(gripFolded(g)).toBe(true);
    expect(useHref(g), "folded: the chevron points back in").toBe("#i-chevron-left");
    // A page's own button brings it back, and folds it again; asking for the state it has is nothing.
    gripFold(g, false);
    expect(gripFolded(g)).toBe(false);
    expect(p.style.display).toBe("");
    expect(p.style.width).toBe("400px");
    expect(useHref(g)).toBe("#i-chevron-right");
    expect(stored("t6")).toEqual({ size: 400, folded: false });
    gripFold(g, false);
    expect(gripFolded(g)).toBe(false);
    pointer(g, "pointerdown", { x: 800 });
    pointer(g, "pointermove", { x: 700 });
    expect(p.style.width, "towards the start widens").toBe("500px");
    pointer(g, "pointermove", { x: 1000 });
    expect(p.style.width, "towards the end narrows, down to the minimum").toBe("280px");
    pointer(g, "pointermove", { x: 1100 });
    expect(p.style.display, "past half the minimum it folds").toBe("none");
    pointer(g, "pointermove", { x: 800 });
    pointer(g, "pointerup", { x: 800 });
    expect(p.style.width).toBe("400px");
    press(g, "ArrowLeft");
    expect(p.style.width).toBe("416px");
    press(g, "ArrowRight");
    expect(p.style.width).toBe("400px");
    gripFold(g, true);
    expect(p.style.display).toBe("none");
    // Remembered open, the next paint opens it whatever the default.
    gripFold(g, false);
    const again = pane();
    expect(gripFolded(grip(again, o))).toBe(false);
    expect(again.style.width).toBe("400px");
  });

  it("ui.css: a hidden pane takes its edge with it, and a folded edge is a strip that stays", () => {
    const rules = parseCss(sheet("ui.css")).filter((r) => !r.at);
    const decl = (sel: string, prop: string): string | undefined =>
      rules.find((r) => r.selectors.includes(sel))?.decls.find((d) => d.prop === prop)?.value;
    expect(decl("[hidden] + .grip", "display")).toBe("none");
    expect(decl(".grip.x", "width")).toBe("0");
    expect(decl(".grip.x.folded", "width")).toBe("var(--s5)");
    expect(decl(".grip.x::before", "cursor")).toBe("col-resize");
    expect(decl(".grip.y::before", "cursor")).toBe("row-resize");
  });
});

describe("ui/chat - chatPane", () => {
  it("places editable context between the queue and input without rebuilding the editor", () => {
    const input = h("textarea"), queued = h("div"), context = h("button", { type: "button" }, "orders");
    input.value = "keep my draft";
    const pane = chatPane({ cap: "Agent", body: null, input, queued, context, actions: [] });
    expect([...pane.querySelector(".chat-compose")!.children].slice(0, 2)).toEqual([queued, context]);
    expect(pane.querySelector(".chat-editor > textarea")).toBe(input);
    context.textContent = "orders · 2 selected rows";
    expect(input.value).toBe("keep my draft");
  });
  it("keeps the subject in its heading and permission/model/send inside one input card", () => {
    const input = h("textarea"), permission = h("button", null, "Review writes"), model = h("button", null, "Model"), send = h("button", null, "Send");
    const p = chatPane({ cap: "Agent", heading: h("select"), subject: "shop @ localhost", body: null, input, controls: [permission], actions: [model, send] });
    expect(p.querySelector(".chat-heading .chat-subject")?.textContent).toBe("shop @ localhost");
    expect(p.querySelector(".chat-heading > select")).not.toBeNull();
    const editor = p.querySelector(".chat-editor");
    for (const control of [input, permission, model, send]) expect(control.closest(".chat-editor")).toBe(editor);
    expect([...p.querySelector(".chat-acts")!.children]).toEqual([permission, p.querySelector(".chat-note"), model, send]);
  });
  it("a labelled aside: the head with its caption and tools, the body, the words box over its note and actions", () => {
    const input = h("textarea");
    const send = h("button", { type: "button" }, "Send");
    const p = chatPane({ id: "ag", cap: "Agent", tools: [h("select")], body: h("p", null, "Ask"), input, note: "orders", actions: [send] });
    expect(p.tagName).toBe("ASIDE");
    expect(p.id).toBe("ag");
    expect(p.getAttribute("aria-label")).toBe("Agent");
    expect(Array.from(p.children).map((c) => c.className)).toEqual(["chat-head", "chat-body", "chat-compose"]);
    expect(p.querySelector(".chat-heading > .sec-cap")?.textContent).toBe("Agent");
    expect(p.querySelector(".chat-tools > select")).not.toBeNull();
    expect(p.querySelector(".chat-editor > textarea")).toBe(input);
    expect(p.querySelector(".chat-acts > .chat-note")?.textContent).toBe("orders");
    expect(p.querySelector(".chat-acts > button")).toBe(send);
    const bare = chatPane({ cap: "Agent", body: null, input: h("textarea"), actions: [] });
    expect(bare.querySelector(".chat-tools"), "no tools, no tools row").toBeNull();
    // What waits to be sent stands above the words box.
    const queued = h("div", { class: "edit-list" });
    const q = chatPane({ cap: "Agent", body: null, input: h("textarea"), queued, actions: [] });
    expect(Array.from(q.querySelector(".chat-compose")!.children).slice(0, 2).map((c) => c.tagName)).toEqual(["DIV", "DIV"]);
    expect(q.querySelector(".chat-compose > .edit-list")).toBe(queued);
    // The column's end comes into view as an answer streams.
    const body = p.querySelector<HTMLElement>(".chat-body")!;
    Object.defineProperty(body, "scrollHeight", { value: 900 });
    chatFollow(p);
    expect(body.scrollTop).toBe(900);
    expect(p.querySelector(".chat-cmds"), "no commands, no list").toBeNull();
  });

  it("follows growing replies until the reader scrolls up, then offers a way back to the latest words", () => {
    const p = chatPane({ cap: "Agent", body: h("div"), input: h("textarea"), actions: [] });
    document.body.appendChild(p);
    const body = p.querySelector<HTMLElement>(".chat-body")!;
    let height = 900;
    let top = 0;
    Object.defineProperties(body, {
      clientHeight: { value: 200 },
      scrollHeight: { get: () => height },
      scrollTop: { get: () => top, set: (v: number) => { top = Math.max(0, Math.min(v, height - 200)); } },
    });
    chatFollow(p);
    expect(top).toBe(700);
    height = 1100;
    chatFollow(p);
    expect(top).toBe(900);
    body.scrollTop = 400;
    body.dispatchEvent(new Event("scroll"));
    height = 1500;
    chatFollow(p);
    expect(top, "new chunks do not pull the reader out of older history").toBe(400);
    const latest = p.querySelector<HTMLButtonElement>('[aria-label="Back to bottom"]')!;
    const glyph = latest.querySelector("use")?.getAttribute("href") || "";
    expect(glyph).toBe("#i-arrow-down");
    const sprite = readFileSync(new NodeURL("../../src/admin_assets/index.html", import.meta.url), "utf8");
    expect(sprite).toContain('id="' + glyph.slice(1) + '"');
    expect(latest.hidden).toBe(false);
    latest.click();
    expect(top).toBe(1300);
    expect(latest.hidden).toBe(true);
    height = 1700;
    chatFollow(p);
    expect(top, "returning to the latest words resumes following").toBe(1500);
    p.remove();
  });

  it("the words box grows with its words, a write by the page included; a box not laid out keeps its rows", () => {
    const input = h("textarea", { rows: 2 });
    const p = chatPane({ cap: "Agent", body: null, input, actions: [] });
    document.body.appendChild(p);
    let tall = 0;
    Object.defineProperty(input, "scrollHeight", { get: () => tall, configurable: true });
    Object.defineProperty(input, "offsetHeight", { get: () => (tall ? tall + 2 : 0), configurable: true });
    Object.defineProperty(input, "clientHeight", { get: () => tall, configurable: true });
    tall = 120;
    input.value = "one\ntwo\nthree\nfour";
    expect(input.style.height, "the words plus the border").toBe("122px");
    expect(input.value).toBe("one\ntwo\nthree\nfour");
    tall = 80;
    input.dispatchEvent(new Event("input"));
    expect(input.style.height).toBe("82px");
    tall = 0;
    input.value = "";
    expect(input.style.height, "a folded pane's box keeps its rows").toBe("");
    p.remove();
  });

  it("commands: a \"/\" lists them above the box - the arrows walk, Tab or Enter completes, Escape shuts, a click completes; a key it takes goes no further", () => {
    const input = h("textarea");
    const p = chatPane({ cap: "Agent", body: null, input, actions: [], commands: [
      { name: "compact", arg: "[focus]", hint: "Condense the conversation" },
      { name: "clear", hint: "Start over" },
    ] });
    document.body.appendChild(p);
    const list = p.querySelector<HTMLElement>(".chat-compose > .chat-cmds")!;
    expect(list.getAttribute("role")).toBe("listbox");
    expect(input.getAttribute("aria-controls")).toBe(list.id);
    expect(list.hidden).toBe(true);
    let seen = 0;
    input.addEventListener("keydown", () => { seen++; });
    const press = (key: string, shiftKey = false): boolean => input.dispatchEvent(new KeyboardEvent("keydown", { key, shiftKey, bubbles: true, cancelable: true }));
    const names = (): string[] => Array.from(list.querySelectorAll(".chat-cmd-name")).map((r) => r.textContent || "");
    const marked = (): string | null => list.querySelector("[aria-selected=true] .chat-cmd-name")?.textContent ?? null;
    // Words in a box without the focus open nothing; the focus brings the list.
    input.value = "/c";
    expect(list.hidden).toBe(true);
    input.focus();
    expect(list.hidden).toBe(false);
    expect(input.getAttribute("aria-expanded")).toBe("true");
    expect(names()).toEqual(["/compact [focus]", "/clear"]);
    expect(list.querySelector(".chat-cmd-hint")?.textContent).toBe("Condense the conversation");
    expect(marked()).toBe("/compact [focus]");
    press("ArrowDown");
    expect(marked()).toBe("/clear");
    expect(input.getAttribute("aria-activedescendant")).toBe(list.querySelector("[aria-selected=true]")!.id);
    press("ArrowDown");
    expect(marked(), "the arrows wrap").toBe("/compact [focus]");
    press("ArrowUp");
    expect(press("Tab"), "Tab completes").toBe(false);
    expect(input.value).toBe("/clear ");
    expect(list.hidden, "a space ends the name").toBe(true);
    expect(input.getAttribute("aria-expanded")).toBe("false");
    expect(seen, "the keys the list took never reached the page").toBe(0);
    // A name that begins with the words first, then one that holds them.
    input.value = "/ear";
    expect(names()).toEqual(["/clear"]);
    input.value = "/sel";
    expect(list.hidden, "no command, no list").toBe(true);
    // Enter completes a part; on a name typed whole it is the page's.
    input.value = "/comp";
    expect(press("Enter")).toBe(false);
    expect(input.value).toBe("/compact ");
    input.value = "/compact";
    expect(list.hidden).toBe(false);
    expect(press("Enter")).toBe(true);
    expect(press("Enter", true), "Shift+Enter breaks the line").toBe(true);
    expect(seen).toBe(2);
    // Escape shuts the list until the words change.
    input.value = "/c";
    expect(press("Escape")).toBe(false);
    expect(list.hidden).toBe(true);
    input.dispatchEvent(new Event("focus"));
    expect(list.hidden, "the same words keep it shut").toBe(true);
    input.value = "/cl";
    expect(names()).toEqual(["/clear"]);
    // A click completes and leaves the caret in the box.
    input.value = "/";
    const down = new MouseEvent("mousedown", { bubbles: true, cancelable: true });
    list.querySelectorAll(".chat-cmd")[1].dispatchEvent(down);
    expect(down.defaultPrevented, "the box keeps the focus").toBe(true);
    expect(input.value).toBe("/clear ");
    expect(input.selectionStart).toBe(7);
    // Leaving the box shuts it.
    input.value = "/";
    expect(list.hidden).toBe(false);
    input.blur();
    expect(list.hidden).toBe(true);
    p.remove();
  });
});

describe("ui/chat - chatQueue", () => {
  it("a card naming how many wait, one row each with Edit, Delete and steer-now; ↑ moves a row first, a steered row is marked", () => {
    const steered: string[] = [];
    const deleted: string[] = [];
    let edited = 0;
    const q = chatQueue({
      id: "qq",
      max: 50,
      onSteer: (m) => { steered.push(m.text); q.steer(m.id, true); },
      onDelete: (m) => { deleted.push(m.text); },
      onEdited: () => { edited++; },
    });
    document.body.appendChild(q.node);
    expect(q.node.id).toBe("qq");
    expect(q.node.hidden, "empty, it takes no room").toBe(true);
    const a = q.add("first");
    q.add("second\nmore");
    const c = q.add("third");
    expect(q.node.hidden).toBe(false);
    expect(q.node.querySelector(".chat-queue-n")?.textContent).toBe("3 queued messages");
    const texts = (): string[] => q.items().map((m) => m.text);
    const rowOf = (id: string): HTMLElement => q.node.querySelector<HTMLElement>("[data-queued='" + id + "']")!;
    expect(Array.from(q.node.querySelectorAll(".chat-queue-text")).map((t) => t.textContent)).toEqual(["first", "second …", "third"]);
    expect(rowOf(q.items()[1].id).querySelector(".chat-queue-text")?.getAttribute("title"), "the whole words in the title").toBe("second\nmore");
    const labels = (id: string): string[] => Array.from(rowOf(id).querySelectorAll("button")).filter((b) => !b.hidden).map((b) => b.getAttribute("aria-label") ?? "");
    expect(labels(c.id)).toEqual(["Edit", "Delete", "Steer now: the answer reads it before its next step"]);
    // The head folds the rows away and back.
    const head = q.node.querySelector<HTMLButtonElement>("button.chat-queue-head")!;
    const list = q.node.querySelector<HTMLElement>(".chat-queue-list")!;
    expect(head.getAttribute("aria-controls")).toBe(list.id);
    head.click();
    expect(list.hidden).toBe(true);
    expect(head.getAttribute("aria-expanded")).toBe("false");
    head.click();
    expect(list.hidden).toBe(false);
    // ↑ puts the third first and hands it to the host, which marks it steered.
    rowOf(c.id).querySelectorAll("button")[2].click();
    expect(steered).toEqual(["third"]);
    expect(texts()).toEqual(["third", "first", "second\nmore"]);
    expect(list.firstElementChild).toBe(rowOf(c.id));
    expect(rowOf(c.id).hasAttribute("data-steered")).toBe(true);
    expect(rowOf(c.id).querySelector(".chat-queue-tag")?.textContent).toBe("steering");
    expect(labels(c.id), "a steered row is withdrawn, never edited").toEqual(["Withdraw"]);
    q.steer(c.id, false);
    expect(rowOf(c.id).hasAttribute("data-steered")).toBe(false);
    // Delete takes the row and tells the host.
    rowOf(a.id).querySelectorAll("button")[1].click();
    expect(deleted).toEqual(["first"]);
    expect(texts()).toEqual(["third", "second\nmore"]);
    expect(q.node.querySelector(".chat-queue-n")?.textContent).toBe("2 queued messages");
    // Edit turns the row into a box in place: Escape keeps the words, Enter saves.
    const b = q.items()[1];
    rowOf(b.id).querySelectorAll("button")[0].click();
    const box = rowOf(b.id).querySelector<HTMLTextAreaElement>("textarea.chat-queue-edit")!;
    expect(box.value).toBe("second\nmore");
    expect(box.maxLength).toBe(50);
    expect(q.editing()).toBe(b);
    let page = 0;
    const onPage = (): void => { page++; };
    document.addEventListener("keydown", onPage);
    box.value = "changed";
    box.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    expect(texts()).toEqual(["third", "second\nmore"]);
    expect(q.editing()).toBeNull();
    expect(edited).toBe(1);
    rowOf(b.id).querySelectorAll("button")[0].click();
    const again = rowOf(b.id).querySelector<HTMLTextAreaElement>("textarea")!;
    again.value = "  rewritten  ";
    again.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    expect(texts()).toEqual(["third", "rewritten"]);
    expect(rowOf(b.id).querySelector(".chat-queue-text")?.textContent).toBe("rewritten");
    expect(page, "the keys the edit took never reached the page").toBe(0);
    document.removeEventListener("keydown", onPage);
    // Saved empty, it is deleted.
    rowOf(b.id).querySelectorAll("button")[0].click();
    const empty = rowOf(b.id).querySelector<HTMLTextAreaElement>("textarea")!;
    empty.value = " ";
    empty.blur();
    expect(texts()).toEqual(["third"]);
    expect(deleted).toEqual(["first", "rewritten"]);
    expect(edited).toBe(3);
    // remove takes a row without a word to the host; the last one gone, the card hides.
    expect(q.remove(c.id)?.text).toBe("third");
    expect(q.remove(c.id)).toBeNull();
    expect(deleted).toHaveLength(2);
    expect(q.node.hidden).toBe(true);
    q.node.remove();
  });
});

describe("ui/copy - copyBtn and copyText", () => {
  const clip = (impl: (s: string) => Promise<void>): string[] => {
    const got: string[] = [];
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: async (s: string): Promise<void> => { await impl(s); got.push(s); } },
    });
    return got;
  };
  afterEach(() => {
    Reflect.deleteProperty(navigator, "clipboard");
    vi.useRealTimers();
  });

  it("a click copies the words read at the click, the glyph a check saying Copied for a moment, then Copy again", async () => {
    vi.useFakeTimers();
    const got = clip(async () => undefined);
    let words = "first";
    const b = copyBtn(() => words, { label: "Copy code" });
    expect(b.matches("button.btn.icon.ghost.copy-btn")).toBe(true);
    expect(b.getAttribute("aria-label")).toBe("Copy code");
    expect(b.querySelector("use")!.getAttribute("href")).toContain("copy");
    words = "SELECT 1";
    b.click();
    await vi.advanceTimersByTimeAsync(0);
    expect(got).toEqual(["SELECT 1"]);
    expect(b.hasAttribute("data-copied")).toBe(true);
    expect(b.getAttribute("aria-label")).toBe("Copied");
    expect(b.title).toBe("Copied");
    expect(b.querySelector("use")!.getAttribute("href")).toContain("check");
    await vi.advanceTimersByTimeAsync(1500);
    expect(b.hasAttribute("data-copied")).toBe(false);
    expect(b.getAttribute("aria-label")).toBe("Copy code");
    expect(b.querySelector("use")!.getAttribute("href")).toContain("copy");
  });

  it("a refused clipboard with no fallback says Copy failed under a cross, never Copied", async () => {
    clip(async () => { throw new Error("denied"); });
    const b = copyBtn("words");
    expect(b.getAttribute("aria-label")).toBe("Copy");
    b.click();
    await new Promise((r) => setTimeout(r, 0));
    expect(b.hasAttribute("data-copied")).toBe(false);
    expect(b.getAttribute("aria-label")).toBe("Copy failed");
    expect(b.querySelector("use")!.getAttribute("href")).toContain("x");
    expect(await copyText("words")).toBe(false);
    // The fallback's textarea never stays on the page.
    expect(document.querySelector("textarea[readonly]")).toBeNull();
  });

  it("markdownNode({copy}) wraps each code block with its Copy - a diagram's fence none - and without copy none", async () => {
    const got = clip(async () => undefined);
    const src = "Run:\n\n```sql\nSELECT 1;\n```\n\n```\nplain\n```\n\n```mermaid\ngraph TD; a-->b\n```";
    const md = markdownNode(src, { copy: true });
    const boxes = md.querySelectorAll(".md-codebox");
    expect(boxes.length).toBe(2);
    for (const box of Array.from(boxes)) {
      expect(box.firstElementChild!.matches("pre.md-code")).toBe(true);
      expect(box.lastElementChild!.matches(".copy-btn")).toBe(true);
      expect(box.lastElementChild!.getAttribute("aria-label")).toBe("Copy code");
    }
    (boxes[0].querySelector(".copy-btn") as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    expect(got).toEqual(["SELECT 1;"]);
    expect(markdownNode(src).querySelector(".md-codebox, .copy-btn")).toBeNull();
  });

  it("transcriptTurn({copy}) closes the turn on its Copy, a markdown turn's code blocks carrying theirs; no copy, no row", async () => {
    const got = clip(async () => undefined);
    const turn = transcriptTurn({ role: "assistant", text: "```\nx = 1\n```\nDone.", markdown: true, copy: "the answer" });
    expect(turn.lastElementChild!.matches(".tc-acts")).toBe(true);
    expect(turn.querySelectorAll(".md-codebox > .copy-btn").length).toBe(1);
    (turn.querySelector(".tc-acts > .copy-btn") as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    expect(got).toEqual(["the answer"]);
    expect(transcriptTurn({ side: "user", role: "user", text: "hi" }).querySelector(".tc-acts")).toBeNull();
    expect(transcriptTurn({ side: "user", role: "user", text: "hi", copy: "hi" }).querySelector(".tc-acts > .copy-btn")).not.toBeNull();
    // What went with the words sits under them, ahead of the Copy that closes the turn.
    const foot = document.createElement("details");
    const sent = transcriptTurn({ side: "user", role: "user", text: "hi", foot, copy: "hi" });
    expect(Array.from(sent.children).slice(1).map((c) => c.className || c.tagName)).toEqual(["tc-text", "DETAILS", "tc-acts"]);
  });
});

describe("ui/chat-stats - chatStats", () => {
  const USAGE = {
    turns: 2, steps: 5, tokensIn: 3_100, cacheRead: 38_400, cacheWrite: 2_600, tokensOut: 1_250,
    llmMs: 16_000, toolMs: 4_300, ttftMs: 4_200, ttftSteps: 5, decodeMs: 11_800, decodeTokens: 1_250,
    context: { bytes: 352_000, cap: 1_048_576, system: 30_000, tools: 6_400, limit: 256_000, used: 86_200, perByte: 0.24 },
  };
  const UNTIMED = { llmMs: 0, toolMs: 0, ttftMs: 0, ttftSteps: 0, decodeMs: 0, decodeTokens: 0 };
  const text = (el: Element | null): string => (el?.textContent || "").replace(/\s+/g, " ").trim();
  const rows = (pop: Element): string[][] => Array.from(pop.querySelectorAll(".chat-pop-rows > div")).map((r) => [r.getAttribute("data-row") || "", text(r.querySelector("dt")), text(r.querySelector("dd"))]);

  it("hides while there is nothing to say; then three pills: turns, steps and speed, tokens and the cache's share, the ring's percent", () => {
    const s = chatStats({ id: "st" });
    expect(s.node.matches("div.chat-stats#st")).toBe(true);
    expect(s.node.hidden).toBe(true);
    s.set({ ...USAGE, steps: 0, turns: 0, context: null });
    expect(s.node.hidden, "no step and no context: still nothing").toBe(true);
    s.set(USAGE);
    expect(s.node.hidden).toBe(false);
    const activity = s.node.querySelector<HTMLButtonElement>("button.chat-stat[data-stat=activity][aria-haspopup=dialog]")!;
    // The speed over the time after each round's first token: 1,250 out in 11.8 s.
    expect(activity.getAttribute("aria-label")).toBe("2 turns 5 steps · 106 tok/s");
    expect(activity.title).toBe("2 turns 5 steps · 106 tok/s");
    expect(activity.querySelector("use")!.getAttribute("href")).toBe("#i-gauge");
    const sep = activity.querySelector(".chat-stat-label > .chat-stat-sep")!;
    expect([sep.textContent, sep.getAttribute("aria-hidden")]).toEqual(["·", "true"]);
    const tokens = s.node.querySelector<HTMLButtonElement>("button.chat-stat[data-stat=tokens]")!;
    expect(tokens.getAttribute("aria-label")).toBe("45.4K tok · Cache hit 87%");
    expect(tokens.querySelector("use")!.getAttribute("href")).toBe("#i-database");
    const ring = s.node.querySelector<HTMLButtonElement>("button.chat-ctx")!;
    // The provider's count against the budget's tokens: 86,200 of 256,000.
    expect(text(ring)).toBe("34%");
    expect(ring.getAttribute("aria-label")).toBe("34% of context used - what fills it");
    expect(ring.dataset.level).toBe("");
    const arc = ring.querySelector(".chat-ring-fill")!;
    expect(Number(arc.getAttribute("stroke-dasharray")!.split(" ")[0])).toBeCloseTo(2 * Math.PI * 6 * 86_200 / 256_000, 1);
    expect(Array.from(s.node.children).map((c) => c.className)).toEqual(["chat-stat", "chat-stat", "chat-ctx", "chat-pop"]);
    s.set({ ...USAGE, ttftMs: 0, ttftSteps: 0, decodeMs: 0, decodeTokens: 0 });
    expect(activity.getAttribute("aria-label"), "no first token said: the output over the rounds' whole time").toBe("2 turns 5 steps · 78 tok/s");
    s.set({ ...USAGE, ...UNTIMED, turns: 1, steps: 1 });
    expect(activity.getAttribute("aria-label"), "no time counted: no speed").toBe("1 turn 1 step");
    expect(activity.disabled, "and no card to open").toBe(true);
    s.set({ ...USAGE, tokensIn: 4, cacheRead: 9_996, cacheWrite: 0, tokensOut: 0 });
    expect(tokens.getAttribute("aria-label"), "a hit short of all reads short of 100").toBe("10K tok · Cache hit 99.96%");
    s.set({ ...USAGE, tokensIn: 0, cacheRead: 0, cacheWrite: 0, tokensOut: 0 });
    expect(tokens.hidden, "nothing billed: no tokens pill").toBe(true);
    s.set({ ...USAGE, steps: 0, turns: 0 });
    expect(s.node.hidden, "a restored conversation's weight alone shows the ring").toBe(false);
    expect(activity.hidden).toBe(true);
    expect(ring.hidden).toBe(false);
    s.set({ ...USAGE, context: { ...USAGE.context, used: 200_000 } });
    expect(ring.dataset.level).toBe("warn");
    s.set({ ...USAGE, context: { ...USAGE.context, used: 240_000 } });
    expect(ring.dataset.level).toBe("full");
    s.set(null);
    expect(s.node.hidden).toBe(true);
  });

  it("each pill opens its card above the line, one at a time: the session's times, the tokens exactly", () => {
    const s = chatStats({ id: "st3" });
    document.body.appendChild(s.node);
    s.set(USAGE);
    const [activity, tokens] = Array.from(s.node.querySelectorAll<HTMLButtonElement>("button.chat-stat"));
    const pop = s.node.querySelector<HTMLElement>(".chat-pop[role=dialog]")!;
    expect([activity.getAttribute("aria-controls"), tokens.getAttribute("aria-controls")]).toEqual([pop.id, pop.id]);
    expect(pop.hidden).toBe(true);
    activity.click();
    expect(activity.getAttribute("aria-expanded")).toBe("true");
    expect(pop.hidden).toBe(false);
    expect(pop.getAttribute("aria-label")).toBe("Session statistics");
    expect(text(pop.querySelector(".chat-pop-title > .chat-pop-label"))).toBe("Session statistics");
    expect(pop.querySelector(".chat-pop-label use")!.getAttribute("href")).toBe("#i-gauge");
    expect(text(pop.querySelector(".chat-pop-title > .chat-pop-value"))).toBe("2 turns 5 steps");
    expect(pop.querySelector(".chat-pop-title + .chat-pop-rule + dl.chat-pop-rows")).not.toBeNull();
    expect(rows(pop)).toEqual([
      ["llm", "LLM time", "16s"],
      ["tool", "Tool time", "4.3s"],
      ["ttft", "Avg time to first token (TTFT)", "0.8s"],
      ["speed", "Tokens per second (TPS)", "106 tok/s"],
    ]);
    // An open card follows the counts: past a minute a time reads in minutes.
    s.set({ ...USAGE, llmMs: 1_443_000, toolMs: 0 });
    expect(rows(pop).map((r) => r[2]).slice(0, 2)).toEqual(["24m3s", "0.8s"]);
    // Another pill takes the card over.
    tokens.click();
    expect(activity.getAttribute("aria-expanded")).toBe("false");
    expect(tokens.getAttribute("aria-expanded")).toBe("true");
    expect(pop.getAttribute("aria-label")).toBe("Token usage");
    expect(pop.querySelector(".chat-pop-label use")!.getAttribute("href")).toBe("#i-database");
    expect(text(pop.querySelector(".chat-pop-value"))).toBe("45,350 tok");
    expect(rows(pop)).toEqual([
      ["hit", "Cache hit", "87%"],
      ["in", "Uncached input", "3,100 tok"],
      ["read", "Cached input", "38,400 tok"],
      ["write", "Cache write", "2,600 tok"],
      ["out", "Output", "1,250 tok"],
    ]);
    s.set({ ...USAGE, cacheWrite: 0 });
    expect(rows(pop).map((r) => r[0]), "nothing written to the cache: no such row").toEqual(["hit", "in", "read", "out"]);
    // Escape closes it, the focus back on its pill.
    tokens.blur();
    document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(pop.hidden).toBe(true);
    expect(tokens.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(tokens);
    // A pill whose card has nothing left to show closes it.
    activity.click();
    s.set({ ...USAGE, ...UNTIMED });
    expect(pop.hidden).toBe(true);
    expect(activity.getAttribute("aria-expanded")).toBe("false");
    s.node.remove();
  });

  it("the ring opens what fills the context - the count against the budget, a bar and a row each, the note under them - and Escape or a press outside closes it", () => {
    const s = chatStats({ id: "st2", note: "A send past the budget is refused." });
    document.body.appendChild(s.node);
    s.set(USAGE);
    const ring = s.node.querySelector<HTMLButtonElement>("button.chat-ctx")!;
    const pop = s.node.querySelector<HTMLElement>(".chat-pop[role=dialog]")!;
    expect(ring.getAttribute("aria-controls")).toBe(pop.id);
    ring.click();
    expect(ring.getAttribute("aria-expanded")).toBe("true");
    expect(pop.hidden).toBe(false);
    expect(text(pop.querySelector(".chat-pop-label"))).toBe("34% of context used");
    // The provider's count is no estimate, the budget no guess: neither carries a "~".
    expect(text(pop.querySelector(".chat-pop-value"))).toBe("86.2K / 256K");
    // The parts split the count as they split the bytes.
    expect(rows(pop)).toEqual([["system", "System prompt", "7.3K"], ["tools", "Tool definitions", "1.6K"], ["messages", "Messages", "77.3K"]]);
    expect(pop.querySelectorAll(".chat-pop-rows dt > .chat-ctx-key")).toHaveLength(3);
    const widths = (): number[] => Array.from(pop.querySelectorAll<HTMLElement>(".chat-ctx-bar > .chat-ctx-seg")).map((seg) => parseFloat(seg.style.width));
    expect(widths()).toEqual([2.87, 0.61, 30.19]);
    expect(text(pop.querySelector(".chat-pop-note"))).toBe("A send past the budget is refused.");
    // No round has counted what the conversation carries (a compaction's checkpoint): the
    // bytes read as tokens, marked as the estimate they are.
    s.set({ ...USAGE, context: { ...USAGE.context, used: undefined } });
    expect(text(ring)).toBe("~33%");
    expect(text(pop.querySelector(".chat-pop-value"))).toBe("~84.5K / 256K");
    expect(rows(pop)[0][2]).toBe("~7.2K");
    // Without a limit the bytes read against the byte budget.
    s.set({ ...USAGE, context: { bytes: 352_000, cap: 1_048_576, system: 30_000, tools: 6_400 } });
    expect(text(pop.querySelector(".chat-pop-value"))).toBe("344 KiB / 1024 KiB");
    expect(widths()).toEqual([2.86, 0.61, 30.1]);
    // Escape closes it, the focus back on the ring, and never reaches the page.
    const heard: string[] = [];
    const page = (e: KeyboardEvent): void => { heard.push(e.key); };
    document.addEventListener("keydown", page);
    ring.blur();
    document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(pop.hidden).toBe(true);
    expect(ring.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(ring);
    expect(heard).toEqual([]);
    document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(heard, "closed, it takes no Escape").toEqual(["Escape"]);
    document.removeEventListener("keydown", page);
    // A press inside or on a pill keeps it, one outside closes it; the ring again toggles it.
    ring.click();
    pop.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    expect(pop.hidden).toBe(false);
    s.node.querySelector("[data-stat=tokens]")!.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    expect(pop.hidden).toBe(false);
    document.body.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    expect(pop.hidden).toBe(true);
    ring.click();
    ring.click();
    expect(pop.hidden).toBe(true);
    s.node.remove();
  });

  it("chatPane({stats}) sets the line under the actions; in Chinese the counts and the cards read as such", () => {
    install("zh-CN", zh);
    const s = chatStats();
    s.set(USAGE);
    const pane = chatPane({ cap: "Agent", body: h("div"), input: h("textarea"), actions: [], stats: s.node });
    expect(pane.querySelector(".chat-compose > .chat-editor + .chat-stats")).toBe(s.node);
    const activity = s.node.querySelector<HTMLButtonElement>("[data-stat=activity]")!;
    expect(activity.getAttribute("aria-label")).toBe("2 轮 5 步 · 106 tok/s");
    expect(s.node.querySelector("[data-stat=tokens]")!.getAttribute("aria-label")).toBe("45.4K tok · 缓存命中 87%");
    expect(s.node.querySelector("button.chat-ctx")!.getAttribute("aria-label")).toBe("上下文已用 34%，查看构成");
    activity.click();
    const pop = s.node.querySelector(".chat-pop")!;
    expect(text(pop.querySelector(".chat-pop-label"))).toBe("会话统计");
    expect(rows(pop).map((r) => r.slice(1))).toEqual([["模型用时", "16秒"], ["工具调用用时", "4.3秒"], ["首 token 平均（TTFT）", "0.8秒"], ["输出速度（TPS）", "106 tok/s"]]);
    s.set({ ...USAGE, llmMs: 1_443_000 });
    expect(rows(pop)[0][2]).toBe("24分3秒");
    s.node.querySelector<HTMLButtonElement>("[data-stat=tokens]")!.click();
    expect(text(pop.querySelector(".chat-pop-label"))).toBe("Token 用量");
    expect(rows(pop).map((r) => r[1])).toEqual(["缓存命中", "未缓存输入", "缓存读取", "缓存写入", "输出"]);
  });
});

describe("ui/dock - dock", () => {
  afterEach(() => { localStorage.clear(); delete document.body.dataset.dock; });
  const O = { key: "d1", size: 220, min: 96, restMin: 200, label: "Console", restLabel: "Results" };
  const stored = (key: string): unknown => JSON.parse(localStorage.getItem("swiss_dock_" + key) ?? "null");
  const pointer = (t: HTMLElement, type: string, y: number): void => {
    t.dispatchEvent(new MouseEvent(type, { button: 0, clientY: y, bubbles: true }));
  };
  // A console over its results, the results' bar between them; the column is 760 px tall, so
  // the console may take 760 - 200 = 560 px (the head measures nothing here).
  const build = (o = O): { root: HTMLElement; top: HTMLElement; bar: HTMLElement; bottom: HTMLElement; head: HTMLElement; edge: HTMLElement; fold: HTMLElement; max: HTMLElement } => {
    const top = h("div", { id: "dockTop" });
    const bar = h("div", null, h("button", { type: "button", role: "tab" }, "orders · 3"), h("span", null, "3 rows · 4 ms"));
    const bottom = h("div");
    const root = dock(top, bar, bottom, o);
    root.getBoundingClientRect = (): DOMRect => ({ height: 760 }) as DOMRect;
    const head = root.querySelector<HTMLElement>(".dock-head")!;
    const [fold, max] = Array.from(head.querySelectorAll<HTMLElement>(".dock-tools > button"));
    return { root, top, bar, bottom, head, edge: head.querySelector<HTMLElement>(".dock-edge")!, fold: fold!, max: max! };
  };

  it("a pane, the bar it hangs from, and the pane under it; the bar's two buttons fold the results down and give them the column", () => {
    const d = build();
    expect(d.root.className).toBe("dock");
    expect(Array.from(d.root.children)).toEqual([d.top, d.head, d.bottom]);
    expect([d.top.className, d.bottom.className]).toEqual(["dock-top", "dock-bottom"]);
    expect(Array.from(d.head.children).map((c) => c.className)).toEqual(["dock-edge", "", "dock-tools"]);
    expect(d.head.children[1]).toBe(d.bar);
    expect(d.edge.getAttribute("role")).toBe("separator");
    expect(d.edge.getAttribute("aria-label")).toBe("Resize Console");
    expect(d.edge.getAttribute("aria-controls")).toBe("dockTop");
    expect(d.edge.title).toBe("Drag to resize · double-click hides Results or brings it back");
    expect(d.top.style.flex).toBe("0 1 220px");
    expect([d.fold.getAttribute("aria-label"), useHref(d.fold)]).toEqual(["Hide Results", "#i-chevron-down"]);
    expect([d.max.getAttribute("aria-label"), useHref(d.max)]).toEqual(["Maximize Results", "#i-expand"]);
    expect(d.fold.className, "quiet icons, not a floating handle").toBe("btn icon ghost");
    // Fold the results down: the console takes the column, the bar stays to bring them back.
    d.fold.click();
    expect(d.root.dataset.fold).toBe("bottom");
    expect(d.top.style.flex).toBe("1 1 auto");
    expect([d.fold.getAttribute("aria-label"), useHref(d.fold)]).toEqual(["Show Results", "#i-chevron-up"]);
    expect(stored("d1")).toEqual({ size: 220, fold: "bottom" });
    expect(build().root.dataset.fold, "a page painting again keeps it").toBe("bottom");
    d.fold.click();
    expect(d.root.dataset.fold).toBeUndefined();
    expect(d.top.style.flex).toBe("0 1 220px");
    expect(stored("d1"), "the default again: nothing kept").toBeNull();
    // The results take the column: a look, not a layout - the next paint shows the console.
    d.max.click();
    expect(d.root.dataset.fold).toBe("top");
    expect([d.max.getAttribute("aria-label"), useHref(d.max)]).toEqual(["Show Console", "#i-collapse"]);
    expect(stored("d1")).toBeNull();
    expect(build().root.dataset.fold).toBeUndefined();
    // From there, folding the results brings the console back over them.
    d.fold.click();
    expect(d.root.dataset.fold).toBe("bottom");
    d.max.click();
    d.max.click();
    expect(d.root.dataset.fold).toBeUndefined();
  });

  it("a double-click on the bar folds the results or brings back what is folded; the edge's keys size the console", () => {
    const d = build();
    d.head.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    expect(d.root.dataset.fold).toBe("bottom");
    d.bar.querySelector("span")!.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    expect(d.root.dataset.fold, "the facts are the bar too").toBeUndefined();
    d.bar.querySelector("button")!.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    expect(d.root.dataset.fold, "a tab's double-click is the tab's").toBeUndefined();
    const key = (k: string): void => { d.edge.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true })); };
    key("ArrowDown");
    expect(d.top.style.flex).toBe("0 1 236px");
    expect(d.edge.getAttribute("aria-valuenow")).toBe("236");
    key("ArrowUp");
    key("ArrowUp");
    expect(d.top.style.flex).toBe("0 1 204px");
    key("End");
    expect(d.top.style.flex, "the column less the room the results keep").toBe("0 1 560px");
    key("Home");
    expect(d.top.style.flex).toBe("0 1 96px");
    key("Enter");
    expect(d.root.dataset.fold).toBe("bottom");
    key("ArrowDown");
    expect(d.top.style.flex, "a folded pane has no size to step").toBe("1 1 auto");
    key("Enter");
    expect(d.root.dataset.fold).toBeUndefined();
    expect(stored("d1")).toEqual({ size: 96, fold: null });
  });

  it("the bar is the handle: a drag sizes the console, past either end folds a pane keeping the size it started from, and a press on a tab stays the tab's", () => {
    const d = build();
    pointer(d.head, "pointerdown", 300);
    expect(document.body.dataset.dock).toBe("");
    pointer(d.head, "pointermove", 302);
    expect(d.top.style.flex, "a hand's tremble is not a drag").toBe("0 1 220px");
    pointer(d.head, "pointermove", 400);
    expect(d.top.style.flex).toBe("0 1 320px");
    pointer(d.head, "pointermove", 300 + 340);
    expect(d.top.style.flex, "at the room the results keep it stops").toBe("0 1 560px");
    pointer(d.head, "pointermove", 300 + 441);
    expect(d.root.dataset.fold, "past it by half, the results fold down").toBe("bottom");
    pointer(d.head, "pointerup", 741);
    expect(document.body.dataset.dock).toBeUndefined();
    expect(stored("d1")).toEqual({ size: 220, fold: "bottom" });
    // The drag back starts from what the console fills (unmeasured here: its size) and sizes it again.
    pointer(d.edge, "pointerdown", 700);
    pointer(d.edge, "pointermove", 600);
    pointer(d.edge, "pointerup", 600);
    expect(d.root.dataset.fold).toBeUndefined();
    expect(d.top.style.flex).toBe("0 1 120px");
    // Up below half the console's minimum, the console folds away; a drag from there starts at nothing.
    pointer(d.head, "pointerdown", 500);
    pointer(d.head, "pointermove", 500 - 73);
    expect(d.root.dataset.fold).toBe("top");
    pointer(d.head, "pointerup", 427);
    expect(stored("d1"), "the size it started from").toEqual({ size: 120, fold: null });
    pointer(d.head, "pointerdown", 400);
    pointer(d.head, "pointermove", 500);
    pointer(d.head, "pointerup", 500);
    expect(d.root.dataset.fold).toBeUndefined();
    expect(d.top.style.flex).toBe("0 1 100px");
    // A press on a tab is the tab's click.
    const tab = d.bar.querySelector("button")!;
    pointer(tab, "pointerdown", 300);
    pointer(d.head, "pointermove", 400);
    pointer(d.head, "pointerup", 400);
    expect(d.top.style.flex).toBe("0 1 100px");
    expect(document.body.dataset.dock).toBeUndefined();
  });

  it("dockShow brings back the pane asked for - a run answering into folded results", () => {
    const d = build();
    d.fold.click();
    dockShow("d1", "top");
    expect(d.root.dataset.fold, "the console was not folded").toBe("bottom");
    dockShow("d1", "bottom");
    expect(d.root.dataset.fold).toBeUndefined();
    dockShow("nowhere", "bottom"); // no dock under the key: nothing to do
  });

  it("ui.css: a hidden top pane takes the bar with it and the results fill; the line waits a beat under the pointer", () => {
    const rules = parseCss(sheet("ui.css")).filter((r) => !r.at);
    const decl = (sel: string, prop: string): string | undefined =>
      rules.find((r) => r.selectors.includes(sel))?.decls.find((d) => d.prop === prop)?.value;
    expect(decl(".dock-top[hidden] ~ .dock-head", "display")).toBe("none");
    expect(decl(".dock[data-fold=\"top\"] > .dock-top", "display")).toBe("none");
    expect(decl(".dock[data-fold=\"bottom\"] > .dock-top:not([hidden]) ~ .dock-bottom", "display")).toBe("none");
    expect(decl(".dock-head", "cursor")).toBe("row-resize");
    expect(decl(".dock-head", "border-top")).toBe("1px solid var(--sep)");
    expect(decl(".dock-edge::after", "opacity"), "no line across the page at rest").toBe("0");
    expect(decl(".dock-edge:hover::after", "transition-delay")).toBe(".15s");
  });
});

describe("SPEC §panel.ui - ui/chip: a port fact chip", () => {
  it("maps tone to class: up and down are named, held is the quiet default with no tone class", () => {
    const up = portChip("4317", { tone: "up" });
    const down = portChip("4318", { tone: "down" });
    const held = portChip("9229", { tone: "held" });
    expect(up.className).toBe("pchip up");
    expect(down.className).toBe("pchip down");
    expect(held.className).toBe("pchip");
    expect(held.textContent).toBe("9229");
    const titled = portChip("80", { tone: "up", title: "held by this app" });
    expect(titled.title).toBe("held by this app");
  });
});

describe("SPEC §panel.ui - ui/pick: the entry picker", () => {
  it("a row is a button that names its entry, mono when asked", () => {
    const r = pickerRow("exe", "cc.exe", true);
    expect(r.className).toBe("dirpick-row");
    expect(r.querySelector(".dirpick-kind")!.textContent).toBe("\u25aa"); // an executable is a filled square
    expect(r.querySelector(".dirpick-name")!.className).toBe("dirpick-name mono");
    expect(r.textContent).toContain("cc.exe");
  });
  it("the panel hides until the page shows it, and its root class never collides with the select's option rows", () => {
    const p = pickPanel("C:\\tmp", "Go", "Up");
    expect(p.root.className).toBe("dirpick");
    expect((p.root as HTMLElement).hidden).toBe(true);
    expect(p.root.querySelector(".dirpick-bar")).toBeTruthy();
    expect(p.root.querySelector(".dirpick-list")).toBeTruthy();
  });
});

describe("SPEC §panel.ui - every class the library draws is styled by base.css or ui.css", () => {
  it("renders every component with every option; no class is left unstyled", () => {
    const tl = timeline([
      item("a", at(23, 9), { same: "q", ms: 1500, arg: "{}", who: "x", status: { text: "error", tone: "bad" } }),
      item("b", at(23, 9), { same: "q" }),
      item("c", at(22, 8), { who: "y", status: { text: "slow", tone: "warn" } }),
    ], { now: NOW, open: new Set(["a"]), body: () => "body" });
    const all = [
      btn("a"), btn("b", { kind: "primary", icon: "play" }), btn("c", { kind: "ghost" }), btn("d", { kind: "danger" }),
      iconBtn("copy", "Copy", { ghost: true }), moreBtn("More"),
      ...(["up", "down", "error", "idle", "starting", "stopping", "off"] as const).map((s) => dot(s, s)),
      tag("t", { mono: true, tone: "bad" }), tag("t", { tone: "warn" }), sw(true, "on"),
      paneHead({ title: "t", desc: "d", sub: "s", actions: [btn("x")] }),
      paneBody({ wide: true }, ...resHead({ title: "t", desc: "d", sub: "s", actions: [btn("x")], nav: seg([{ id: "a", label: "A" }], "a") })),
      note("n"), note("n", { busy: true }), note("n", { err: true }), spinner(),
      pager({ label: "p", status: [spinner(), "1"], prev: btn("a"), next: btn("b") }),
      failNote({ text: "t", why: "w", action: btn("r") }), filterInput({ placeholder: "q", label: "q" }),
      valueBlock({ label: "l", notes: ["n"], tools: [iconBtn("copy", "c")] }, jsonCodeNode({ v: 1 }, false, { oneLine: true }).node),
      timelineMeta(["a", "b"]),
      formFn(pair(fieldFn({ label: "a", required: true, meta: "m", control: document.createElement("input"), hint: "h" }), checkFieldFn({ label: "c", control: document.createElement("input") })), hintFn("x", { bad: true }), formActions(btn("b"))),
      row({ name: "n", sub: "s", detail: valueBlock({ label: "l", text: "t" }), primary: btn("b") }),
      formFn(formCapFn("c"), formFoldFn({ summary: "s" }, fieldFn({ label: "k", control: document.createElement("input"), action: btn("b") }))),
      toTop(document.createElement("div")),
      section({ cap: "c", tools: [btn("x")] }, card(row({ name: "n" }))),
      pageFoot({ note: "n", rev: "r" }), inlineForm(btn("x")),
      emptyNode({ icon: "server", title: "t", hint: "h", action: "a" }),
      row({ lead: dot("up", "up"), name: "n", sub: "s", cols: ["c", { v: "v", mono: true }], toggle: sw(true, "x"), primary: btn("x"), more: moreBtn("m"), muted: true }),
      row({ name: "n", err: "e" }), kvRow("k", "v", { mono: true }),
      groupNode({ name: "g", count: 1, density: "page", addTitle: "a", moreTitle: "m", collapsed: true }).root,
      groupNode({ name: "g", count: 0, density: "side", emptyText: "e" }).root,
      seg([{ id: "a", label: "A", n: 1 }], "a"), tl,
      sheetFrame({ title: "t", sub: "s", body: "b", foot: [h("span", { class: "grow" }), btn("x", { kind: "primary" })] }),
      heldDot("held"),
      portChip("8080", { tone: "up" }), portChip("8080", { tone: "down" }), portChip("8080", { tone: "held" }),
      pickerRow("exe", "cc.exe", true), pickPanel("C:\\tmp", "Go", "Up").root,
      objTab({ name: "t", selected: true, icon: "table", data: {}, count: { n: 1, title: "1" }, close: { label: "x", data: {} } }),
      jsonCodeNode(decodeStrings({ k: "s", n: 1, l: null, d: "{\"a\":[true]}" }), true).node,
      workspace(
        wsSide({ cap: "c", tools: [btn("x")] }, sideRow({ name: "n", lead: dot("up", "up"), badge: tag("1", { tone: "warn" }), tail: "1", selected: true })),
        wsHead({ title: "t", sub: "s", actions: [btn("x")], nav: seg([{ id: "a", label: "A" }], "a") }),
        wsSplit({
          fixed: "start",
          start: [
            wsBar({ rows: [[filterInput({ placeholder: "q", label: "q" })], [btn("x")]] }),
            wsScroll({}, pickList({ label: "l" },
              pickRow({ lead: catChip(1, "k", { bare: true }), name: "n", meta: "m", sub: "s", selected: true }),
              pickRow({ name: "n", muted: true }))),
            wsBar({ foot: true }, "n"),
          ],
          end: wsScroll({ pad: true }, docHead({ tags: [catChip(2, "k")], title: "t", meta: ["a", "b"], actions: [btn("x")] }),
            markdownNode("# h\n\n- a `c`\n\n```sh\nx\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |", { class: "note-body" }), refLink("r")),
        }),
        wsSplit({ fixed: "end", start: "a", end: "b" }),
        catChip(3, "k"), catChip(4, "k"), catChip(5, "k")),
      graphView({
        nodes: [
          { id: "a", label: "a", tone: 1 }, { id: "b", label: "b", tone: 2, dim: true }, { id: "c", label: "c", tone: 3 },
          { id: "d", label: "d", tone: 4 }, { id: "e", label: "e", tone: 5 },
        ],
        edges: [
          { from: "a", to: "b", directed: true }, { from: "b", to: "c", style: "heavy" }, { from: "c", to: "d", style: "dashed" },
          { from: "d", to: "e", style: "dotted" }, { from: "e", to: "a", style: "faint" },
        ],
        center: "a", selected: "b", label: "g",
        keys: [
          { label: "k", tone: 1 }, { label: "k", tone: 2 }, { label: "k", tone: 3 }, { label: "k", tone: 4 }, { label: "k", tone: 5 },
          { label: "l", style: "dotted" }, { label: "m", style: "solid", directed: true }, { label: "n", style: "heavy" },
          { label: "o", style: "dashed", alert: true }, { label: "p", style: "faint" },
        ],
      }, { engine: fakeEngine().engine }).el,
      graphView({ nodes: [], edges: [], label: "g", compact: true }, { engine: fakeEngine().engine }).el,
      graphView({ nodes: [], edges: [], label: "g", fill: true }, { engine: fakeEngine().engine }).el,
      ...mechanisms(),
    ];
    const drawn = new Set<string>();
    for (const root of all) {
      for (const el of [root, ...Array.from(root.querySelectorAll("*"))]) {
        const cls = el.getAttribute("class");
        if (cls) cls.split(/\s+/).filter(Boolean).forEach((c) => drawn.add(c));
      }
    }
    const styled = new Set<string>();
    for (const name of ["base.css", "ui.css"] as const) {
      for (const rule of parseCss(sheet(name))) rule.selectors.forEach((s) => allClassesOf(s).forEach((c) => styled.add(c)));
    }
    const unstyled = Array.from(drawn).filter((c) => !styled.has(c)).sort();
    expect(drawn.size).toBeGreaterThan(40);
    // The mechanisms really drew (a silent no-op would pass the styled check with nothing in it).
    for (const c of ["menu-head", "db-tab-dot", "dd-native", "dd-label", "dd-menu", "sheet-foot", "bad", "jv-dec", "jv-k"]) {
      expect(drawn.has(c), c + " was drawn").toBe(true);
    }
    expect(unstyled).toEqual([]);
  });
});
