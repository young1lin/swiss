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
import { styleSelect } from "../src/dropdown.js";

/* The styled dropdown's pick writes the native select programmatically, which fires no native
   change event, so dropdown.ts dispatches one itself. Before docs/37 R5 every view listened ON
   the select (sel.onchange), and a non-bubbling Event("change") reached it. R5 moved the Data,
   detail and Runs views to one delegated listener on the pane root - which a non-bubbling event
   never reaches. On 19998 that was the Data connection picker repainting its label and doing
   nothing else (2026-09-20). This drives the real menu on a real DOM and listens where the
   views listen now. */
describe("dropdown pick -> change event (docs/37 R5 delegation)", () => {
  function mount(): { pane: HTMLDivElement; sel: HTMLSelectElement; trig: HTMLButtonElement } {
    document.body.innerHTML = "";
    const pane = document.createElement("div");
    pane.id = "pane";
    const sel = document.createElement("select");
    sel.id = "dbConn";
    for (const v of ["mysql", "redis"]) {
      const o = document.createElement("option");
      o.value = v; o.textContent = v + " · " + v;
      sel.appendChild(o);
    }
    pane.appendChild(sel);
    document.body.appendChild(pane);
    styleSelect(sel);
    const trig = pane.querySelector<HTMLButtonElement>("button.dd")!;
    return { pane, sel, trig };
  }

  function pick(trig: HTMLButtonElement, value: string): void {
    trig.click(); // opens the menu on <body>
    const item = Array.from(document.body.querySelectorAll<HTMLButtonElement>(".dd-menu button.pick"))
      .find((b) => b.textContent!.startsWith(value));
    expect(item, "menu item for " + value).toBeTruthy();
    item!.click();
  }

  it("reaches a delegated listener on the pane root, with the select as target", () => {
    const { pane, sel, trig } = mount();
    const seen: Event[] = [];
    pane.onchange = (e) => { seen.push(e); };
    pick(trig, "redis");
    expect(sel.value).toBe("redis");
    expect(seen).toHaveLength(1);
    expect(seen[0].target).toBe(sel);
    expect(seen[0].bubbles).toBe(true);
    // The face followed the pick too.
    expect(trig.querySelector(".dd-label")!.textContent).toBe("redis · redis");
  });

  it("still reaches a listener on the select itself (the pre-R5 idiom the sheets keep)", () => {
    const { sel, trig } = mount();
    let direct = 0;
    sel.onchange = () => { direct++; };
    pick(trig, "redis");
    expect(direct).toBe(1);
  });

  it("does not fire when the pick is the current value", () => {
    const { pane, trig } = mount();
    let n = 0;
    pane.onchange = () => { n++; };
    pick(trig, "mysql"); // already selected
    expect(n).toBe(0);
  });
});
