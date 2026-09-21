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

/* h(), frag(), fill() and iconNode — the builders R5 converts this panel's rendering to
 * (docs/37 §7).
 *
 * THE FIRST REAL DOM IN THIS SUITE, on purpose and per-file. Every other test here hand-rolls
 * a stub object with appendChild/setAttribute/textContent, and against a stub these assertions
 * would be circular: a fake textContent setter cannot demonstrate that a `<` stops being a tag.
 * h()'s entire claim is about what the BROWSER does with what it is handed, so the test needs
 * something that behaves like one. happy-dom is a dev-only devDependency (cargo build still
 * needs no node), and the environment is opted into by the comment above rather than in
 * vitest.config.ts, so the 64 suites that stub their own globals keep running under `node`
 * exactly as before. */

import { describe, expect, it } from "vitest";
import { frag, h, fill } from "../src/h.js";
import { iconNode } from "../src/util.js";

/* The label a hostile (or merely unlucky) server can send. The old string builders were correct
 * only while every author remembered esc(); these tests are what says the builder no longer
 * depends on remembering. */
const HOSTILE = '<img src=x onerror="alert(1)">';

describe("h: remote text is text, not markup (docs/37 R5)", () => {
  it("a string child lands as a text node, tags and all", () => {
    const node = h("div", { class: "name" }, HOSTILE);
    expect(node.textContent).toBe(HOSTILE);
    // The one assertion that matters: nothing was PARSED. No element child exists, so no
    // onerror can ever fire.
    expect(node.children.length).toBe(0);
    expect(node.querySelector("img")).toBeNull();
  });

  it("an attribute value cannot break out of its quotes", () => {
    const node = h("button", { data: { token: '" onclick="alert(1)' } });
    expect(node.getAttribute("data-token")).toBe('" onclick="alert(1)');
    expect(node.getAttribute("onclick")).toBeNull();
  });

  it("the same value through innerHTML DOES parse — this is what the builder replaces", () => {
    // Not a test of h(); a test of the idiom h() exists to retire, so the difference is
    // pinned in the suite rather than asserted in a comment.
    const bad = document.createElement("div");
    bad.innerHTML = '<div class="name">' + HOSTILE + "</div>";
    expect(bad.querySelector("img")).not.toBeNull();
  });
});

describe("h: props go through the element's own types", () => {
  it("assigns DOM properties, not same-named attributes", () => {
    const input = h("input", { value: "typed", readOnly: true, placeholder: "Label" });
    expect(input.value).toBe("typed");
    expect(input.readOnly).toBe(true);
    expect(input.placeholder).toBe("Label");
    // `value` as a PROPERTY is the live value; the attribute is only the default, and the old
    // string builder could set nothing else.
    expect(input.getAttribute("value")).toBeNull();
  });

  it("class and style are the two attribute spellings kept", () => {
    const node = h("div", { class: "group wide", style: "margin-top:var(--s4)" });
    expect(node.className).toBe("group wide");
    expect(node.getAttribute("style")).toBe("margin-top:var(--s4)");
  });

  it("data and aria maps expand to prefixed attributes", () => {
    const node = h("button", { data: { tkuse: "abc", n: 3 }, aria: { label: "Use abc", expanded: false } });
    expect(node.getAttribute("data-tkuse")).toBe("abc");
    expect(node.getAttribute("data-n")).toBe("3");
    expect(node.getAttribute("aria-label")).toBe("Use abc");
    // false skips the attribute — aria-expanded absent is not the same as "false", and a
    // conditional data-* reads better as an expression than as an if.
    expect(node.getAttribute("aria-expanded")).toBeNull();
  });

  it("true writes the empty attribute, null and undefined write nothing", () => {
    const node = h("div", { data: { open: true, gone: null, missing: undefined } });
    expect(node.getAttribute("data-open")).toBe("");
    expect(node.hasAttribute("data-gone")).toBe(false);
    expect(node.hasAttribute("data-missing")).toBe(false);
  });
});

describe("h: children", () => {
  it("nests elements, flattens arrays and drops the falsy ones", () => {
    const used = false;
    const node = h("div", { class: "row" },
      h("div", { class: "name" }, "redis"),
      used && h("span", { class: "tag" }, "copies use this"),
      [h("em", null, "a"), h("em", null, "b")],
      null, undefined, 0);
    expect(node.children.length).toBe(3);
    expect(node.querySelector(".tag")).toBeNull();
    // 0 is a number, not a falsy child to drop — a count of zero must still print.
    expect(node.textContent).toBe("redisab0");
  });

  it("reads a Node, string, number or array in the props slot as the first child", () => {
    expect(h("div", "plain").textContent).toBe("plain");
    expect(h("div", 7).textContent).toBe("7");
    expect(h("div", h("b", null, "x")).children.length).toBe(1);
    expect(h("div", [h("b", null, "x"), h("b", null, "y")]).children.length).toBe(2);
  });
});

describe("frag and fill", () => {
  it("frag collects siblings with no wrapper element", () => {
    const f = frag(h("div", null, "a"), h("div", null, "b"), null);
    expect(f.childNodes.length).toBe(2);
    const host = document.createElement("div");
    host.appendChild(f);
    expect(host.children.length).toBe(2);
    expect(f.childNodes.length).toBe(0); // appending MOVED them — one reflow, not n
  });

  it("fill replaces a host's contents and keeps the host itself", () => {
    const host = document.createElement("div");
    host.id = "tkGroups";
    host.appendChild(document.createElement("span"));
    fill(host, h("div", { class: "row" }, "one"), h("div", { class: "row" }, "two"));
    expect(host.id).toBe("tkGroups"); // the node the listeners are on survives
    expect(host.querySelectorAll(".row").length).toBe(2);
    expect(host.querySelector("span")).toBeNull();
  });
});

describe("iconNode: the sprite as a node (docs/37 R5)", () => {
  it("builds a real SVG element in the SVG namespace", () => {
    const svg = iconNode("ellipsis");
    expect(svg.namespaceURI).toBe("http://www.w3.org/2000/svg");
    expect(svg.getAttribute("class")).toBe("ic");
    expect(svg.getAttribute("aria-hidden")).toBe("true");
    const use = svg.firstElementChild;
    expect(use?.namespaceURI).toBe("http://www.w3.org/2000/svg");
    expect(use?.getAttribute("href")).toBe("#i-ellipsis");
  });

  it("a label makes it an image with a name, exactly as icon() does", () => {
    const svg = iconNode("ellipsis", "Actions for default");
    expect(svg.getAttribute("role")).toBe("img");
    expect(svg.getAttribute("aria-label")).toBe("Actions for default");
    expect(svg.hasAttribute("aria-hidden")).toBe(false);
  });
});
