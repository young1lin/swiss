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
import { initSelects, styleSelect } from "../src/ui/select.js";

/* SPEC §data: a styled select that leaves the DOM must take its trigger button with
   it. The trigger sits BESIDE the select ("afterend"), so removing the select alone orphans a
   live-looking dropdown - on a redis page one kept showing the previous pg connection's schema
   pick. Until SPEC §panel.ui this suite ran under node on hand-rolled stubs with a fake
   MutationObserver it fired by hand; ui/select.ts now builds with h(), which the stubs cannot
   host, so it runs on happy-dom and the REAL observer delivers the removal. */
const settle = (): Promise<void> => new Promise((r) => { setTimeout(r, 0); });

function styled(host: HTMLElement, label: string): { sel: HTMLSelectElement; trig: HTMLButtonElement } {
  const sel = document.createElement("select");
  sel.className = "db-schemaish";
  sel.setAttribute("aria-label", label);
  for (const [v, t] of [["", "All schemas"], ["public", "public (103)"]]) {
    const o = document.createElement("option");
    o.value = v; o.textContent = t;
    sel.appendChild(o);
  }
  host.appendChild(sel);
  styleSelect(sel);
  const trig = sel.nextElementSibling as HTMLButtonElement;
  expect(trig && trig.classList.contains("dd"), "styleSelect inserted a trigger beside the select").toBe(true);
  return { sel, trig };
}

describe("a styled select leaving the DOM takes its trigger with it (SPEC §data)", () => {
  document.body.innerHTML = "";
  const parent = document.body.appendChild(document.createElement("div"));
  initSelects();

  it("removing the select alone must not orphan a live-looking dropdown", async () => {
    const { sel, trig } = styled(parent, "Schema");
    expect(trig.getAttribute("aria-label")).toBe("Schema");
    expect(trig.className, "the select's own sizing class rides on the face").toBe("dd db-schemaish");
    // The view removes the SELECT (connection switch to a kind with no picker):
    sel.remove();
    await settle();
    expect(parent.contains(trig), "the trigger went with its select").toBe(false);
  });

  it("a whole removed subtree containing styled selects drops their triggers too", async () => {
    const holder = parent.appendChild(document.createElement("div"));
    const { trig } = styled(holder, "Kind");
    holder.remove();
    await settle();
    expect(document.body.contains(trig)).toBe(false);
  });

  it("a select added later is styled by the observer, no opt-in", async () => {
    const sel = document.createElement("select");
    sel.appendChild(document.createElement("option")).textContent = "one";
    parent.appendChild(sel);
    await settle();
    expect(sel.classList.contains("dd-native")).toBe(true);
    expect((sel.nextElementSibling as HTMLElement).querySelector(".dd-label")!.textContent).toBe("one");
  });

  it("an open list whose trigger is re-rendered away closes instead of floating", async () => {
    const holder = parent.appendChild(document.createElement("div"));
    const { trig } = styled(holder, "Schema");
    trig.click();
    expect(document.querySelector(".dd-menu"), "the list opened").toBeTruthy();
    holder.remove();
    await settle();
    expect(document.querySelector(".dd-menu"), "no list left floating").toBeNull();
  });
});
