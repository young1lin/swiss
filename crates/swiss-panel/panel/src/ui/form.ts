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

/* Forms (SPEC §panel.ui): the few shapes every form in the panel is made of - the Run tab's
 * argument form, an MCP's edit form, the Add sheet, a job's action. The page builds its own
 * controls (an <input>, a <select>, a <textarea>: their ids and data hooks are the page's
 * contract with its listener); these functions put them in the house shape:
 *
 *   form        the padded grid inside a card: one grid step between fields.
 *   field       a label over its control, a hint under it. `required` adds the red star;
 *               `meta` is a quiet word after the name (an argument's type); `action` is the
 *               control's one button beside it (Browse).
 *   formCap     a caption over one part of a form.
 *   formFold    a part of a form that starts folded (Advanced).
 *   checkField  a checkbox with its label to the right, the hint under it.
 *   pair        two fields side by side (host + port).
 *   formActions the one primary and its companions, at the form's foot.
 *   hint        one line of help or a live result under a control (`bad`: a refusal).
 *
 * No handlers and no state: a form is read by id when it is submitted. */
import type { HChild } from "../h.js";
import { h } from "../h.js";

export function form(...kids: HChild[]): HTMLElement {
  return h("div", { class: "form" }, ...kids);
}

/** The name, then the star, then the quiet meta word - one span, so the label's spacing is the
 *  text's own and the star never floats a flex gap away from the name it marks. */
function labelText(label: HChild, required?: boolean, meta?: string): HTMLElement {
  const kids: HChild[] = [label];
  if (required) kids.push(" ", h("span", { class: "req-star" }, "*"));
  if (meta) kids.push(h("span", { class: "field-meta" }, meta));
  return h("span", null, kids);
}

let groupSeq = 0; // ids for a group field's caption, unique per page load

export function field(o: { label: HChild; control: HTMLElement; required?: boolean; meta?: string; hint?: HChild; action?: HTMLElement; group?: boolean }): HTMLElement {
  const text = labelText(o.label, o.required, o.meta);
  // `group`: a control that is several controls (weekday toggles, a set of checks). A <label>
  // hands a click on its caption to the FIRST labelable element inside it - the job sheet's
  // "Days" caption toggled Sunday. A group names its members (role=group, aria-labelledby)
  // and activates none of them; it draws the same as a label field (ui.css .field).
  if (o.group) text.id = "fld-g" + (++groupSeq);
  const label = o.group
    ? h("div", { class: "field", role: "group", aria: { labelledby: text.id } }, text, o.control)
    : h("label", { class: "field" }, text, o.control);
  return h("div", { class: "fld" },
    // `action`: the control's one button beside it (a key path and Browse), on the control's row.
    o.action ? h("div", { class: "field-row" }, label, o.action) : label,
    o.hint != null && o.hint !== false && o.hint !== "" ? hint(o.hint) : null);
}

/** A fold inside a form: the part most people never open (a connection's proxy and jump),
 *  collapsed under a summary line that still names what is set in it (chips), so collapsed
 *  never means hidden. Native <details>: keyboard and state for free. */
export function formFold(o: { summary: HChild; id?: string }, ...body: HChild[]): HTMLElement {
  return h("details", { class: "fold", id: o.id },
    h("summary", null, o.summary),
    h("div", { class: "fold-body" }, ...body));
}

/** A caption inside a form, over the part it names (a sheet's "Proxy", "Serves MCPs"): the
 *  section caption's voice, a grid step above its fields. */
export function formCap(text: HChild): HTMLElement {
  return h("div", { class: "form-cap" }, text);
}

export function checkField(o: { label: HChild; control: HTMLInputElement; required?: boolean; hint?: HChild }): HTMLElement {
  return h("div", { class: "fld" },
    h("label", { class: "check" }, o.control, labelText(o.label, o.required)),
    o.hint != null && o.hint !== false && o.hint !== "" ? hint(o.hint) : null);
}

export function pair(a: HChild, b: HChild): HTMLElement {
  return h("div", { class: "two" }, a, b);
}

export function formActions(...kids: HChild[]): HTMLElement {
  return h("div", { class: "form-actions" }, ...kids);
}

export function hint(body: HChild, o: { id?: string; bad?: boolean; hidden?: boolean; live?: boolean } = {}): HTMLElement {
  return h("div", {
    class: o.bad ? "hint bad" : "hint", id: o.id, hidden: o.hidden,
    aria: o.live ? { live: "polite" } : undefined,
  }, body);
}
