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

/* Forms (docs/46 §2.1): the few shapes every form in the panel is made of - the Run tab's
 * argument form, an MCP's edit form, the Add sheet, a job's action. The page builds its own
 * controls (an <input>, a <select>, a <textarea>: their ids and data hooks are the page's
 * contract with its listener); these functions put them in the house shape:
 *
 *   form        the padded grid inside a card: one grid step between fields.
 *   field       a label over its control, a hint under it. `required` adds the red star;
 *               `meta` is a quiet word after the name (an argument's type).
 *   checkField  a checkbox with its label to the right, the hint under it.
 *   pair        two fields side by side (host + port).
 *   formActions the one primary and its companions, at the form's foot.
 *   hint        one line of help or a live result under a control (`bad`: a refusal).
 *
 * No handlers and no state: a form is read by id when it is submitted. */
                                      
import { h } from "../h.js";

export function form(...kids          )              {
  return h("div", { class: "form" }, ...kids);
}

/** The name, then the star, then the quiet meta word - one span, so the label's spacing is the
 *  text's own and the star never floats a flex gap away from the name it marks. */
function labelText(label        , required          , meta         )              {
  const kids           = [label];
  if (required) kids.push(" ", h("span", { class: "req-star" }, "*"));
  if (meta) kids.push(h("span", { class: "field-meta" }, meta));
  return h("span", null, kids);
}

export function field(o                                                                                           )              {
  return h("div", { class: "fld" },
    h("label", { class: "field" }, labelText(o.label, o.required, o.meta), o.control),
    o.hint != null && o.hint !== false && o.hint !== "" ? hint(o.hint) : null);
}

export function checkField(o                                                                                 )              {
  return h("div", { class: "fld" },
    h("label", { class: "check" }, o.control, labelText(o.label, o.required)),
    o.hint != null && o.hint !== false && o.hint !== "" ? hint(o.hint) : null);
}

export function pair(a        , b        )              {
  return h("div", { class: "two" }, a, b);
}

export function formActions(...kids          )              {
  return h("div", { class: "form-actions" }, ...kids);
}

export function hint(body        , o                                                                   = {})              {
  return h("div", {
    class: o.bad ? "hint bad" : "hint", id: o.id, hidden: o.hidden,
    aria: o.live ? { live: "polite" } : undefined,
  }, body);
}
