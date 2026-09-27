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

/* redacted() (2026-09-28): a remote address is masked until its eye is pressed, so a
   screenshot of the panel carries none. What must hold: the address is nowhere in the
   node - not its text, not an attribute a hover or a copy would surface - until the eye is
   pressed; the eye reveals every drawing of that value and a rebuilt node (a poll) keeps
   it; the click stops at the eye, because the eye sits inside rows whose own click means
   something else; loopback draws plain. */

import { beforeEach, describe, expect, it } from "vitest";
import { isLocalAddress, redacted } from "../src/ui/index.js";

const HOST = "203.0.113.7";

function click(el: Element): void {
  el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
}
function eyeOf(node: Element): HTMLButtonElement {
  return node.querySelector("button.redact-eye") as HTMLButtonElement;
}

beforeEach(() => {
  document.body.innerHTML = "";
});

describe("redacted()", () => {
  it("masks a remote host: the address is in no text and no attribute", () => {
    const node = redacted(HOST, { suffix: ":22" });
    document.body.appendChild(node);
    expect(node.textContent).toBe("••••••:22");
    expect(node.outerHTML).not.toContain(HOST);
    const eye = eyeOf(node);
    expect(eye.getAttribute("aria-label")).toBe("Show address");
    expect(eye.getAttribute("aria-pressed")).toBe("false");
    expect(eye.querySelector("use")?.getAttribute("href")).toBe("#i-eye");
  });

  it("the eye reveals every drawing of that value, and a node built after keeps it open", () => {
    const a = redacted(HOST, { suffix: ":22" });
    const b = redacted(HOST, { prefix: "15432 → ", suffix: ":5432" });
    const other = redacted("198.51.100.9");
    document.body.append(a, b, other);
    click(eyeOf(a));
    expect(a.textContent).toBe(HOST + ":22");
    expect(b.textContent).toBe("15432 → " + HOST + ":5432");
    expect(other.textContent, "another address stays masked").toBe("••••••");
    expect(eyeOf(a).getAttribute("aria-pressed")).toBe("true");
    expect(eyeOf(a).getAttribute("aria-label")).toBe("Hide address");
    expect(eyeOf(a).querySelector("use")?.getAttribute("href")).toBe("#i-eye-off");
    // The 6s poll rebuilds the row: the new node draws open.
    const rebuilt = redacted(HOST, { suffix: ":22" });
    expect(rebuilt.textContent).toBe(HOST + ":22");
    // And the eye closes them all again.
    click(eyeOf(b));
    expect(a.textContent).toBe("••••••:22");
    expect(b.textContent).toBe("15432 → ••••••:5432");
  });

  it("the click stops at the eye: the row around it never hears it", () => {
    const rowEl = document.createElement("div");
    let rowClicks = 0;
    rowEl.addEventListener("click", () => { rowClicks++; });
    const node = redacted(HOST);
    rowEl.appendChild(node);
    document.body.appendChild(rowEl);
    click(eyeOf(node));
    expect(rowClicks).toBe(0);
    click(node.querySelector("code") as Element);
    expect(rowClicks).toBe(1);
    click(eyeOf(node)); // close it again for the next test
  });

  it("loopback and empty values draw plain, with no eye", () => {
    for (const host of ["127.0.0.1", "127.3.2.1", "localhost", "::1", "[::1]"]) {
      const node = redacted(host, { suffix: ":5432" });
      expect(node.tagName, host).toBe("CODE");
      expect(node.textContent, host).toBe(host + ":5432");
      expect(node.querySelector(".redact-eye"), host).toBeNull();
    }
    expect(redacted("").textContent).toBe("");
  });

  it("isLocalAddress matches loopback whole, nothing that merely starts like it", () => {
    expect(isLocalAddress("127.0.0.1")).toBe(true);
    expect(isLocalAddress("LOCALHOST")).toBe(true);
    expect(isLocalAddress("[::1]")).toBe(true);
    expect(isLocalAddress("127.0.0.1.evil.test")).toBe(false);
    expect(isLocalAddress("localhost.evil.test")).toBe(false);
    expect(isLocalAddress("10.0.0.1")).toBe(false);
    expect(isLocalAddress("2402:4e00::1")).toBe(false);
  });
});
