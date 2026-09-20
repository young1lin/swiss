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

/* ================================================================================================
   h() — the element builder R5 replaces string-concatenated innerHTML with (docs/37 §7).

   util.ts' el(tag, cls, text) covers "a div with a class and a label" and nothing more, so every
   row, chip and form in this panel is built by gluing strings together and assigning the result to
   .innerHTML. That idiom costs three things, and h() is what buys them back:

     ESCAPING STOPS BEING A CHOICE. A string builder is correct only while every interpolation
     remembers esc(); the parser cannot tell an author's markup from a server's value. h() puts
     remote text in a TEXT NODE, where `<` is a character and nothing else. The converted views
     lose their esc() calls — that loss IS the fix, not a tidy-up.

     THE TYPES REACH THE ATTRIBUTES. `'<input value="' + v + '">'` is a string to the compiler.
     h("input", { value: v, readOnly: true }) is checked against HTMLInputElement, so a typo
     (`readonly`, `calss`) is an error rather than an attribute nobody ever notices is missing.

     NODES SURVIVE A REPAINT. Assigning .innerHTML destroys and rebuilds every descendant, which
     is why this panel re-attaches its handlers after each render. Built nodes can be kept,
     replaced one at a time, or handed to a delegated listener that never needs re-attaching.

   What h() deliberately does NOT do:

     NO HANDLERS IN PROPS. Settable strips every function-valued property, so h("button", {
     onclick: fn }) is a type error. R5's second half is moving this panel off per-element
     handlers and onto one delegated listener per view; a builder that made `onclick:` the most
     convenient thing to type would work against that. A view that genuinely needs a direct
     handler assigns it to the returned node, in the open, on its own line.

     NO RAW HTML CHILD. There is no `html:` prop and no dangerouslySetInnerHTML. A caller with
     real markup to insert (the icon sprite) uses a builder that returns a NODE — see iconNode in
     util.ts. Leaving a raw-HTML door in this module would hand back exactly the hole the text
     nodes just closed.
   ================================================================================================ */

/* The writable, non-method properties of an element — what h() will accept in its props bag.
 *
 * The first mapped type is the standard readonly probe: two otherwise identical conditional
 * types, one with `-readonly`, are assignable to each other only when the key was already
 * mutable. That is the only way TypeScript exposes the readonly modifier to a mapped type, and
 * it is what keeps h("div", { offsetTop: 3 }) — a silent no-op at runtime — from compiling.
 *
 * The second strips function-valued properties (see the header: no handlers in props). The test
 * is on NonNullable because every on* property is typed `handler | null`, which does not extend
 * Function on its own. */
type WritableKeys<T> = {
  [K in keyof T]-?: (<G>() => G extends { [P in K]: T[K] } ? 1 : 2) extends
    (<G>() => G extends { -readonly [P in K]: T[K] } ? 1 : 2) ? K : never;
}[keyof T];

type Settable<T> = {
  [K in WritableKeys<T> as NonNullable<T[K]> extends (...args: never[]) => unknown ? never : K]?: T[K];
};

/** An attribute map: data / aria. A null or undefined value SKIPS the attribute rather than
 *  writing the string "null" — the conditional attributes in this panel ("selected" only when
 *  it is, aria-label only when the button has no text) read better as an expression than as an
 *  if around an setAttribute. */
type AttrMap = Record<string, string | number | boolean | null | undefined>;

/* className is omitted in favour of `class`: two spellings of one thing is how the old code
 * ended up with rows that set both. `style` comes back as a string because that is how this
 * panel writes the handful of inline styles it has (a width, a grid template) — CSSStyleDeclaration
 * is readonly and would have been stripped anyway. */
export type HProps<K extends keyof HTMLElementTagNameMap> =
  Omit<Settable<HTMLElementTagNameMap[K]>, "className" | "style"> & {
    class?: string;
    style?: string;
    data?: AttrMap;
    aria?: AttrMap;
  };

/** Anything h() accepts as a child. `false`, `null` and `undefined` render nothing, so a
 *  conditional child is `cond && h(…)` with no ternary-to-empty-string dance. Arrays flatten,
 *  so `rows.map(rowNode)` drops straight in. */
export type HChild = Node | string | number | false | null | undefined | HChild[];

function appendChild(node: HTMLElement, child: HChild): void {
  if (child === false || child == null) return;
  if (Array.isArray(child)) { child.forEach((c) => { appendChild(node, c); }); return; }
  node.appendChild(child instanceof Node ? child : document.createTextNode(String(child)));
}

function setAttrs(node: HTMLElement, prefix: string, map: AttrMap): void {
  Object.keys(map).forEach((key) => {
    const value = map[key];
    if (value == null || value === false) return;
    node.setAttribute(prefix + key, value === true ? "" : String(value));
  });
}

/**
 * Build one element: h(tag, props?, ...children).
 *
 *   h("div", { class: "row", data: { token: t.id } },
 *     h("div", { class: "name" }, t.label),
 *     used && h("span", { class: "tag" }, "copies use this"))
 *
 * `props` may be omitted entirely (`h("div", child)` is fine — a Node, string, number or array
 * in that slot is read as the first child). Everything in `props` that is not class/style/data/
 * aria is assigned as a DOM PROPERTY, so it goes through the element's own type: `value`,
 * `readOnly`, `disabled`, `selected` behave like the properties they are, not like the
 * attributes with the same names that only set a default.
 */
export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props?: HProps<K> | HChild,
  ...children: HChild[]
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  // A Node, a string, a number or an array in the props slot is a child, not a props bag. Only
  // a plain object can be props, and every child shape above is either not an object or one of
  // the two the check names.
  const isProps = props != null && typeof props === "object" && !(props instanceof Node) && !Array.isArray(props);
  if (isProps) {
    const p = props as HProps<K> & Record<string, unknown>;
    Object.keys(p).forEach((key) => {
      const value = p[key];
      if (value === undefined) return;
      if (key === "class") { node.className = String(value); return; }
      if (key === "style") { node.setAttribute("style", String(value)); return; }
      if (key === "data") { setAttrs(node, "data-", value as AttrMap); return; }
      if (key === "aria") { setAttrs(node, "aria-", value as AttrMap); return; }
      // A dynamic prop write onto a checked key: Object.assign does it without
      // pretending the element carries an index signature.
      Object.assign(node, { [key]: value });
    });
  } else {
    appendChild(node, props as HChild);
  }
  children.forEach((child) => { appendChild(node, child); });
  return node;
}

/** A DocumentFragment of the given children — for the callers that return "several siblings,
 *  no wrapper". Appending a fragment moves its children and leaves the fragment empty, which is
 *  what makes `body.appendChild(frag(rows.map(rowNode)))` one reflow instead of n. */
export function frag(...children: HChild[]): DocumentFragment {
  const f = document.createDocumentFragment();
  children.forEach((child) => {
    if (child === false || child == null) return;
    if (Array.isArray(child)) { f.appendChild(frag(...child)); return; }
    f.appendChild(child instanceof Node ? child : document.createTextNode(String(child)));
  });
  return f;
}

/** Replace a host's children with the given ones, in one shot. The one legitimate use of
 *  "wipe and redraw" that survives R5: the host itself is kept (its listeners, its scroll), only
 *  the contents change — and no string is parsed on the way in. */
export function fill(host: HTMLElement, ...children: HChild[]): void {
  host.textContent = "";
  host.appendChild(frag(...children));
}
