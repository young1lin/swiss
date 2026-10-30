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

import { describe, expect, it } from "vitest";
import { closeSelect, initSelects, selectOpen, styleSelect } from "../src/ui/select.js";
import { popupMenu } from "../src/ui/menu.js";

/* SPEC §panel.ui: an open select list is fixed to the window while its trigger lives in a
   scrolling box (a sheet body). Found live on 19998: in the Add agent sheet the body scrolled,
   the trigger went with it, and the list stayed where the trigger had been. The list follows
   the trigger now, and closes once the trigger has left the box's view. happy-dom lays nothing
   out, so the rects are stubbed. */
type Rect = { top: number; bottom: number; left: number; right: number; width: number; height: number };
const rect = (top: number, height: number): Rect => ({ top, bottom: top + height, left: 40, right: 240, width: 200, height });

function setup(): { box: HTMLElement; sel: HTMLSelectElement; trig: HTMLButtonElement; at: (top: number) => void } {
  document.body.innerHTML = "";
  const box = document.body.appendChild(document.createElement("div"));
  box.getBoundingClientRect = () => rect(100, 400) as DOMRect;
  const sel = document.createElement("select");
  for (const v of ["a", "b", "c"]) {
    const o = document.createElement("option");
    o.value = v; o.textContent = v.toUpperCase();
    sel.appendChild(o);
  }
  box.appendChild(sel);
  styleSelect(sel);
  const trig = sel.nextElementSibling as HTMLButtonElement;
  let top = 200;
  trig.getBoundingClientRect = () => rect(top, 30) as DOMRect;
  return { box, sel, trig, at: (t) => { top = t; } };
}

const menu = (): HTMLElement | null => document.querySelector(".dd-menu");

describe("an open select list rides its trigger through a scroll (SPEC §panel.ui)", () => {
  initSelects();

  it("the list moves with the trigger when the box around it scrolls", () => {
    const { box, trig, at } = setup();
    trig.click();
    expect(selectOpen()).toBe(true);
    expect(menu()!.style.top).toBe("234px");
    at(150);
    box.dispatchEvent(new Event("scroll"));
    expect(selectOpen(), "still open: the trigger is in view").toBe(true);
    expect(menu()!.style.top, "placed under the trigger's new spot").toBe("184px");
    closeSelect();
  });

  it("the list closes once its trigger scrolls out of the box's view", () => {
    const { box, trig, at } = setup();
    trig.click();
    at(60); // above the box's top edge (100): clipped away
    box.dispatchEvent(new Event("scroll"));
    expect(selectOpen()).toBe(false);
    expect(menu()).toBeNull();
  });

  it("scrolling the list itself is not a move", () => {
    const { trig, at } = setup();
    trig.click();
    at(9999);
    menu()!.dispatchEvent(new Event("scroll"));
    expect(selectOpen()).toBe(true);
    closeSelect();
  });
});

describe("a floating popup menu closes when something else scrolls (SPEC §panel.ui)", () => {
  it("a scroll outside closes it; a scroll inside does not", () => {
    document.body.innerHTML = "";
    const box = document.body.appendChild(document.createElement("div"));
    popupMenu({ left: 10, top: 10, bottom: 30 }, [{ label: "One", fn: () => {} }]);
    const node = document.getElementById("menu")!;
    node.dispatchEvent(new Event("scroll"));
    expect(document.getElementById("menu")).not.toBeNull();
    box.dispatchEvent(new Event("scroll"));
    expect(document.getElementById("menu")).toBeNull();
  });
});
