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

/* SPEC §panel.ui - the three DOM-only mechanisms the library took over: the floating
   menu (ui/menu.ts), the select (ui/select.ts) and the sheet (ui/sheet.ts). Each is driven on
   a real DOM with dispatched keys and clicks, and each key test also listens where main.ts
   listens - a bubbling keydown on document - because the bugs these moves fixed were keys
   the component handled that ALSO reached the panel's own chain: an Escape in a dropdown
   closed the sheet around it, an arrow in a menu moved the MCP selection behind it. */
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { h } from "../src/h.js";
import { install } from "../src/i18n.js";
import zh from "../src/locales/zh.js";
import {
  closeMenu, closeSelect, closeSheet, initSelects, initSheet, menuOpen, openFieldSheet, popupMenu, selectOpen,
  sheet, sheetOpen, showSheet, styleSelect,
} from "../src/ui/index.js";
import { parseCss } from "./css-rules.js";
import { sheet as styleSheet } from "./styles.js";

/** What reached main.ts's layer: every keydown that bubbled to document. */
const reached: string[] = [];
document.addEventListener("keydown", (e) => { reached.push(e.key); });

function key(target: Element, k: string): KeyboardEvent {
  const e = new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true });
  target.dispatchEvent(e);
  return e;
}
const settle = (): Promise<void> => new Promise((r) => { setTimeout(r, 0); });

beforeEach(() => {
  reached.length = 0;
  closeMenu();
  closeSelect();
  document.body.innerHTML = "<div id=\"sheet\" class=\"backdrop\" hidden></div><div id=\"host\"></div>";
});
afterEach(() => { install("en", null); });

describe("ui/menu - popupMenu", () => {
  const anchor = { left: 40, top: 40, bottom: 60 };

  it("one #menu.menu.float: roles, a rule for a separator, a heading that is no button", () => {
    popupMenu(anchor, [
      { label: "Heading", fn: () => {}, heading: true },
      { label: "Edit", fn: () => {} },
      { label: "Pick me", fn: () => {}, pick: true, on: true },
      { label: "Refused", fn: () => {}, disabled: true, title: "why not" },
      { sep: true },
      { label: "Delete", fn: () => {}, danger: true },
    ]);
    const m = document.getElementById("menu")!;
    expect(m.className).toBe("menu float");
    expect(m.getAttribute("role")).toBe("menu");
    expect(m.querySelector(".menu-head")!.tagName).toBe("DIV");
    expect(m.querySelectorAll("hr")).toHaveLength(1);
    const buttons = Array.from(m.querySelectorAll("button"));
    expect(buttons.map((b) => b.className)).toEqual(["", "pick on", "", "danger"]);
    expect(buttons[2].disabled).toBe(true);
    expect(buttons[2].title).toBe("why not");
    // The refused row is shown but out of the walk; the rest are menu items.
    expect(buttons.filter((b) => b.getAttribute("role") === "menuitem").map((b) => b.textContent)).toEqual(["Edit", "Pick me", "Delete"]);
    expect(document.activeElement, "the first real item takes focus").toBe(buttons[0]);
    expect(menuOpen()).toBe(true);
  });

  it("a glyph leads the label, the held-edits dot and an affordance trail it", () => {
    popupMenu(anchor, [{ label: "orders", fn: () => {}, icon: "table", dot: true, affordance: "chevron-right" }]);
    const b = document.querySelector("#menu button")!;
    const kids = Array.from(b.childNodes);
    expect((kids[0] as Element).getAttribute("class")).toBe("ic");
    expect(kids[1].textContent).toBe("orders");
    expect((kids[2] as Element).getAttribute("class")).toBe("ic");
    expect((kids[3] as Element).className).toBe("db-tab-dot");
  });

  it("such a row is a flex row in ui.css - inline, the glyph touched its label and the dot drew nothing", () => {
    // Found by the gallery walk (SPEC §panel.ui), shipped that way since SPEC §data.tabs: the dot is
    // a sized span, and a sized inline span has no box. Layout is the browser's to prove (the
    // walk measures the dot); this pins the rule that gives it one.
    const rule = parseCss(styleSheet("ui.css")).find((r) => !r.at && r.selectors.some((sel) => /^\.menu button:has\(/.test(sel) && sel.includes(".ic") && sel.includes(".db-tab-dot")));
    expect(rule, "a .menu button:has(> .ic, > .db-tab-dot) rule").toBeDefined();
    const decl = (p: string): string | undefined => rule?.decls.find((d) => d.prop === p)?.value;
    expect(decl("display")).toBe("flex");
    expect(decl("align-items")).toBe("center");
    expect(decl("gap")).toMatch(/^var\(--s\d\)$/);
  });

  it("an item click runs its fn once and closes; a disabled one runs nothing", () => {
    let ran = 0;
    popupMenu(anchor, [{ label: "Go", fn: () => { ran++; } }, { label: "No", fn: () => { ran += 10; }, disabled: true }]);
    const [go, no] = Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button"));
    no.click();
    expect(ran).toBe(0);
    go.click();
    expect(ran).toBe(1);
    expect(document.getElementById("menu")).toBeNull();
    expect(menuOpen()).toBe(false);
  });

  it("the arrows walk the items with wrap and stop at the menu - main.ts's sidebar walk never sees them", () => {
    popupMenu(anchor, [{ label: "a", fn: () => {} }, { label: "b", fn: () => {} }]);
    const [a, b] = Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button"));
    key(a, "ArrowDown");
    expect(document.activeElement).toBe(b);
    key(b, "ArrowDown");
    expect(document.activeElement).toBe(a);
    key(a, "ArrowUp");
    expect(document.activeElement).toBe(b);
    expect(reached).toEqual([]);
  });

  it("Escape closes the menu and goes no further", () => {
    popupMenu(anchor, [{ label: "a", fn: () => {} }]);
    key(document.querySelector("#menu button")!, "Escape");
    expect(document.getElementById("menu")).toBeNull();
    expect(menuOpen()).toBe(false);
    expect(reached).toEqual([]);
  });

  it("a second menu replaces the first; closeMenu takes it and the flag", () => {
    popupMenu(anchor, [{ label: "one", fn: () => {} }]);
    popupMenu(anchor, [{ label: "two", fn: () => {} }]);
    expect(document.querySelectorAll("#menu")).toHaveLength(1);
    expect(document.querySelector("#menu button")!.textContent).toBe("two");
    closeMenu();
    expect(document.querySelector(".menu")).toBeNull();
    expect(menuOpen()).toBe(false);
  });

  it("a menu dropped from a row is never narrower than the row", () => {
    popupMenu({ ...anchor, width: 233 }, [{ label: "mysql", fn: () => {} }]);
    expect(document.getElementById("menu")!.style.minWidth).toBe("max(160px, 233px)");
  });
});

describe("ui/select - the dropdown", () => {
  beforeAll(() => { initSelects(); });

  function mount(): { sel: HTMLSelectElement; trig: HTMLButtonElement } {
    const sel = document.createElement("select");
    sel.className = "db-pagesize";
    sel.setAttribute("aria-label", "Page size");
    for (const v of ["50", "100", "500"]) {
      const o = document.createElement("option");
      o.value = v; o.textContent = v + " rows";
      sel.appendChild(o);
    }
    sel.value = "100";
    document.getElementById("host")!.appendChild(sel);
    styleSelect(sel);
    return { sel, trig: sel.nextElementSibling as HTMLButtonElement };
  }
  const items = (): HTMLButtonElement[] => Array.from(document.querySelectorAll<HTMLButtonElement>(".dd-menu button"));

  it("the face: a .dd trigger beside the hidden select, its label the selected option's text", () => {
    const { sel, trig } = mount();
    expect(sel.classList.contains("dd-native")).toBe(true);
    expect(trig.className).toBe("dd db-pagesize");
    expect(trig.getAttribute("aria-label")).toBe("Page size");
    expect(trig.getAttribute("aria-haspopup")).toBe("listbox");
    expect(trig.getAttribute("aria-expanded")).toBe("false");
    expect(trig.querySelector(".dd-label")!.textContent).toBe("100 rows");
    // Programmatic writes reach the face.
    sel.value = "500";
    expect(trig.querySelector(".dd-label")!.textContent).toBe("500 rows");
    sel.disabled = true;
    expect(trig.disabled).toBe(true);
  });

  it("opening lists every option, marks and focuses the selected one", () => {
    const { trig } = mount();
    trig.click();
    const menu = document.querySelector(".dd-menu")!;
    expect(menu.className).toBe("menu float dd-menu");
    expect(menu.getAttribute("role")).toBe("listbox");
    expect(items().map((b) => [b.textContent, b.className, b.getAttribute("role"), b.getAttribute("aria-selected")])).toEqual([
      ["50 rows", "pick", "option", "false"],
      ["100 rows", "pick on", "option", "true"],
      ["500 rows", "pick", "option", "false"],
    ]);
    expect(document.activeElement).toBe(items()[1]);
    expect(trig.getAttribute("aria-expanded")).toBe("true");
    expect(selectOpen()).toBe(true);
  });

  it("Escape closes the list, gives focus back to the trigger, and stops there (the sheet around it stays)", () => {
    const { trig } = mount();
    trig.click();
    const e = key(items()[1], "Escape");
    expect(document.querySelector(".dd-menu"), "the list closed").toBeNull();
    expect(selectOpen()).toBe(false);
    expect(document.activeElement, "focus is back on the trigger").toBe(trig);
    expect(trig.getAttribute("aria-expanded")).toBe("false");
    expect(e.defaultPrevented).toBe(true);
    expect(reached, "main.ts's Escape chain (close the sheet) never saw it").toEqual([]);
  });

  it("Tab closes and hands focus to the trigger, keeping its default so focus moves on", () => {
    const { trig } = mount();
    trig.click();
    const e = key(items()[1], "Tab");
    expect(document.querySelector(".dd-menu")).toBeNull();
    expect(document.activeElement).toBe(trig);
    expect(e.defaultPrevented).toBe(false);
  });

  it("the arrows walk the options with wrap and go no further", () => {
    const { trig } = mount();
    trig.click();
    key(items()[1], "ArrowDown");
    expect(document.activeElement).toBe(items()[2]);
    key(items()[2], "ArrowDown");
    expect(document.activeElement).toBe(items()[0]);
    key(items()[0], "ArrowUp");
    expect(document.activeElement).toBe(items()[2]);
    expect(reached).toEqual([]);
  });

  it("a pick writes the select, repaints the face, closes and refocuses the trigger", () => {
    const { sel, trig } = mount();
    let changes = 0;
    sel.addEventListener("change", () => { changes++; });
    trig.click();
    items()[0].click();
    expect(sel.value).toBe("50");
    expect(changes).toBe(1);
    expect(trig.querySelector(".dd-label")!.textContent).toBe("50 rows");
    expect(document.querySelector(".dd-menu")).toBeNull();
    expect(document.activeElement).toBe(trig);
  });

  it("a mousedown outside closes; one inside the list does not", () => {
    const { trig } = mount();
    trig.click();
    document.querySelector(".dd-menu")!.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(selectOpen()).toBe(true);
    document.getElementById("host")!.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(selectOpen()).toBe(false);
  });

  it("the trigger opens from the keyboard like a native select, and the key stops there", () => {
    const { trig } = mount();
    const e = key(trig, "ArrowDown");
    expect(e.defaultPrevented).toBe(true);
    expect(selectOpen()).toBe(true);
    // Found live on 19997: the ArrowDown that opened the Add sheet's Group list also walked
    // the MCP sidebar behind the sheet.
    expect(reached).toEqual([]);
  });
});

describe("ui/sheet - the frame, show and close", () => {
  it("sheet(): a modal dialog of head, body and foot", () => {
    const s = sheet({ title: "New job in learn", titleId: "j-title", body: "b", foot: "f" });
    expect(s.className).toBe("sheet");
    expect(s.getAttribute("role")).toBe("dialog");
    expect(s.getAttribute("aria-modal")).toBe("true");
    expect(s.getAttribute("aria-label")).toBe("New job in learn");
    expect(Array.from(s.children).map((c) => c.className)).toEqual(["sheet-head", "sheet-body", "sheet-foot"]);
    expect(s.querySelector("h2")!.id).toBe("j-title");
    // A node title names the dialog through label.
    const t = document.createElement("span");
    t.textContent = "x";
    expect(sheet({ title: t, label: "Add MCP", body: null, foot: null }).getAttribute("aria-label")).toBe("Add MCP");
  });

  it("showSheet unhides the host BEFORE the sheet lands in it (panel-proof-of-life rule 1)", () => {
    const host = document.getElementById("sheet")!;
    const node = sheet({ title: "t", body: null, foot: null });
    let inHostWhenShown: boolean | null = null;
    const proto = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "hidden")!;
    Object.defineProperty(host, "hidden", {
      configurable: true,
      get() { return proto.get!.call(host); },
      set(v: boolean) { if (!v) inHostWhenShown = host.contains(node); proto.set!.call(host, v); },
    });
    showSheet(node);
    expect(inHostWhenShown, "the host was visible first, then filled").toBe(false);
    expect(host.hidden).toBe(false);
    expect(host.contains(node)).toBe(true);
    expect(sheetOpen()).toBe(true);
  });

  it("a click on the backdrop closes; a click inside the card does not", () => {
    const host = document.getElementById("sheet")!;
    const node = sheet({ title: "t", body: "inside", foot: null });
    showSheet(node);
    (node.querySelector(".sheet-body") as HTMLElement).click();
    expect(sheetOpen()).toBe(true);
    host.click();
    expect(sheetOpen()).toBe(false);
    expect(host.hidden).toBe(true);
    expect(host.childNodes).toHaveLength(0);
  });

  it("a sheet is modal: keys pressed in it stay in it, Escape aside (initSheet)", () => {
    initSheet(); // beforeEach rebuilds #sheet, so wire this test's host
    const node = sheet({ title: "t", body: null, foot: h("button", { type: "button", id: "in-sheet" }, "Cancel") });
    showSheet(node);
    const b = document.getElementById("in-sheet")!;
    for (const k of ["ArrowDown", "ArrowUp", "/", "r"]) key(b, k);
    expect(reached, "the sidebar walk, search focus and refresh behind the sheet never saw them").toEqual([]);
    key(b, "Escape");
    expect(reached, "Escape reaches main.ts's chain, which closes the sheet").toEqual(["Escape"]);
  });

  it("closeSheet hides and empties the host", () => {
    showSheet(sheet({ title: "t", body: null, foot: null }));
    closeSheet();
    expect(sheetOpen()).toBe(false);
    expect(document.getElementById("sheet")!.childNodes).toHaveLength(0);
  });
});

describe("ui/sheet - openFieldSheet", () => {
  const field = (): HTMLInputElement => document.getElementById("g-name") as HTMLInputElement;
  const err = (): HTMLElement => document.getElementById("g-err")!;
  const save = (): void => { (document.getElementById("g-save") as HTMLButtonElement).click(); };

  it("opens visibly with the field focused; the words are Create / Cancel when creating", () => {
    openFieldSheet({ title: "New group", submit: () => true });
    expect(sheetOpen()).toBe(true);
    expect(document.activeElement).toBe(field());
    expect(document.getElementById("g-save")!.textContent).toBe("Create");
    expect(document.getElementById("g-cancel")!.textContent).toBe("Cancel");
    expect(field().placeholder).toBe("prod");
  });

  it("an empty value is an inline error in the red hint, and nothing is submitted", async () => {
    const got: string[] = [];
    openFieldSheet({ title: "New group", submit: (v) => { got.push(v); return true; } });
    field().value = "   ";
    save();
    await settle();
    expect(got).toEqual([]);
    expect(err().hidden).toBe(false);
    expect(err().className).toBe("hint bad");
    expect(err().textContent).toBe("Name is required");
    expect(sheetOpen()).toBe(true);
  });

  it("a rename that changed nothing is a cancel: closed, nothing submitted", async () => {
    const got: string[] = [];
    openFieldSheet({ title: "Rename group", def: "prod", submit: (v) => { got.push(v); return true; } });
    expect(document.getElementById("g-save")!.textContent).toBe("Rename");
    save();
    await settle();
    expect(got).toEqual([]);
    expect(sheetOpen()).toBe(false);
  });

  it("Enter submits the trimmed value; true closes", async () => {
    const got: string[] = [];
    openFieldSheet({ title: "New group", submit: (v) => { got.push(v); return true; } });
    field().value = "  learn ";
    const e = key(field(), "Enter");
    await settle();
    expect(e.defaultPrevented).toBe(true);
    expect(got).toEqual(["learn"]);
    expect(sheetOpen()).toBe(false);
  });

  it("a string back is an inline error and the sheet stays; false stays with no error", async () => {
    openFieldSheet({ title: "Rename table", submit: async () => "letters, digits and _ only" });
    field().value = "a b";
    save();
    await settle();
    expect(err().textContent).toBe("letters, digits and _ only");
    expect(sheetOpen()).toBe(true);
    openFieldSheet({ title: "Rename table", submit: () => false });
    field().value = "ok";
    save();
    await settle();
    expect(sheetOpen()).toBe(true);
    expect(err().hidden).toBe(true);
  });

  it("the Chinese pass: the sheet's own words come from the table", () => {
    install("zh-CN", zh);
    openFieldSheet({ title: "新建分组", submit: () => true });
    expect(document.getElementById("g-save")!.textContent).toBe("创建");
    expect(document.getElementById("g-cancel")!.textContent).toBe("取消");
    expect(document.querySelector("label.field span")!.textContent).toBe("名称");
  });
});
