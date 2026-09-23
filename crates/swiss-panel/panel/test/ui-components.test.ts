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

/* docs/46 §2 - the ui/ component library, one describe per module. Each pins the markup a
   page and the stylesheet agree on (class, role, aria, data hooks) and the option that
   changes it, so a component cannot drift from ui.css or from the delegated listeners that
   address it. The last block renders every component with every option and checks each class
   it drew is styled by base.css or ui.css - the two sheets the gallery links - so a typo in a
   class name fails here instead of rendering as an unstyled box. */
import { afterEach, describe, expect, it } from "vitest";
import { h } from "../src/h.js";
import { install } from "../src/i18n.js";
import zh from "../src/locales/zh.js";
import {
  checkField as checkFieldFn, field as fieldFn, form as formFn, formActions, formCap as formCapFn, formFold as formFoldFn, hint as hintFn,
  pair, stackSheet,
} from "../src/ui/index.js";
import {
  anchoredMenu, btn, card, closeMenu, closeSheet, collapseRuns, dayLabel, decodeStrings, dot, emptyNode, failNote, filterInput,
  fmtMs, groupNode, iconBtn, iconNode, inlineForm, jsonCodeNode, kvRow, menuOpen, moreBtn, note, openFieldSheet, pageFoot,
  pager, paneBody, paneHead, popupMenu, resHead, row, section, seg, sheet as sheetFrame, spinner, styleSelect, sw, tag,
  timeLabel, timeline, timelineMeta, timelineToggle, toTop, valueBlock,
} from "../src/ui/index.js";
import type { TimelineItem } from "../src/ui/index.js";
import { allClassesOf, parseCss } from "./css-rules.js";
import { sheet } from "./styles.js";

afterEach(() => { install("en", null); });

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
  });

  it("moreBtn is the ghost ellipsis with the caller's hook", () => {
    const b = moreBtn("More actions", { data: { more: "fs" } });
    expect(b.className).toBe("btn icon ghost");
    expect(useHref(b)).toBe("#i-ellipsis");
    expect(b.getAttribute("aria-label")).toBe("More actions");
    expect(b.dataset.more).toBe("fs");
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
    expect(menu.className).toBe("menu");
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

describe("docs/46 - every class the library draws is styled by base.css or ui.css", () => {
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
      sheetFrame({ title: "t", body: "b", foot: [h("span", { class: "grow" }), btn("x", { kind: "primary" })] }),
      jsonCodeNode(decodeStrings({ k: "s", n: 1, l: null, d: "{\"a\":[true]}" }), true).node,
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
