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

/* The sheet (docs/46 §2.5): the one modal the panel has - a card over a backdrop, head / body /
 * foot. Moved here from add-sheet.ts at P1b-2 (closeSheet, openFieldSheet) with the frame every
 * sheet builder repeated by hand; the MCP add sheet (openSheet) is app-bound - it knows the
 * field schemas, the detail pane and the poll - and stays in add-sheet.ts, built on these.
 *
 * The host is the page's one #sheet element (index.html, class .backdrop). Two rules every
 * sheet shares live here so no caller can get them wrong again:
 *   - VISIBLE BEFORE PAINTED. showSheet unhides the host and only then fills it
 *     (panel-proof-of-life rule 1): markup filled into a still-hidden host is the sheet that
 *     "rendered nothing", and focus() on a hidden input is a no-op.
 *   - THE BACKDROP CLOSES. A click that lands on the host itself - outside the card - is a
 *     cancel, on every sheet. */
import type { HChild } from "../h.js";
import { fill, h } from "../h.js";
import { tr } from "../i18n.js";

export interface SheetOpts {
  /** The heading. A string, or a node when a caller repaints it later (the add sheet's title
   *  follows its group pick, addressed by titleId). */
  title: HChild;
  /** The dialog's accessible name; the title when the title is a string. */
  label?: string;
  /** An id for the heading, for a caller that repaints it. */
  titleId?: string;
  body: HChild;
  /** The footer, left to right: a leading secondary, a .grow spacer, Cancel, the one primary. */
  foot: HChild;
}

/** The dialog frame. Pure markup: a builder that wants it on screen hands it to showSheet. */
export function sheet(o: SheetOpts): HTMLDivElement {
  const label = o.label ?? (typeof o.title === "string" ? o.title : undefined);
  return h("div", { class: "sheet", role: "dialog", aria: { modal: "true", label } },
    h("div", { class: "sheet-head" }, h("h2", { id: o.titleId }, o.title)),
    h("div", { class: "sheet-body" }, o.body),
    h("div", { class: "sheet-foot" }, o.foot));
}

function host(): HTMLElement {
  const node = document.getElementById("sheet");
  if (!node) throw new Error("no #sheet host on this page");
  return node;
}

/** Put a sheet on screen: the host unhidden FIRST, then filled, and a backdrop click closes. */
export function showSheet(node: HTMLElement): void {
  const el = host();
  el.hidden = false;
  fill(el, node);
  el.onclick = (e) => { if (e.target === el) closeSheet(); };
}

/** A sheet over the open sheet: a picker a sheet opens for one of its own fields (the tunnel
 *  key browser, docs/46 P4), on its own backdrop one layer up. It keeps its keys - Escape
 *  closes THIS layer and stops there; before, it reached the shell's Escape chain, which closed
 *  the sheet underneath and left the picker floating over nothing. `paint` (re)draws its dialog
 *  (the picker repaints on every folder it opens); `close` removes the layer. */
export function stackSheet(onClose?: () => void): { paint: (o: SheetOpts) => HTMLElement; close: () => void } {
  const back = h("div", { class: "backdrop stacked" });
  document.body.appendChild(back);
  const close = (): void => {
    back.remove();
    if (onClose) onClose();
  };
  back.onclick = (e) => { if (e.target === back) close(); };
  back.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Escape") close();
  });
  return {
    paint: (o: SheetOpts): HTMLElement => {
      const node = sheet(o);
      fill(back, node);
      return node;
    },
    close,
  };
}

/** Close the open sheet, dropping its markup (and with it any styled select's open list). */
export function closeSheet(): void {
  const el = host();
  el.hidden = true;
  fill(el);
}

/** Wire the #sheet host once, at boot (main.ts, beside initSelects): a sheet is MODAL, so the
 *  keys pressed inside it stay inside it. Only Escape goes on - main.ts's Escape chain is what
 *  closes a sheet. Without this an arrow on any button in a sheet (the Add sheet's dropdown
 *  triggers, Cancel) walked the MCP sidebar behind it, and "/" pulled focus out of the dialog
 *  into the search box (found live on 19997 in the P1b-2 walk). The listener sits on the host,
 *  not on each sheet, so every sheet has it whichever builder painted it. Capture-phase
 *  listeners on document (the open dropdown's keys, the terminal's, Data's Ctrl+Tab) run
 *  before it and are unaffected. */
export function initSheet(): void {
  const node = document.getElementById("sheet");
  if (!node) return; // a page without the shell (the gallery) has no sheet to wire
  node.addEventListener("keydown", (e) => { if (e.key !== "Escape") e.stopPropagation(); });
}

/** Whether a sheet is on screen: Escape closes it first, and a new panel build waits for it. */
export function sheetOpen(): boolean {
  const node = document.getElementById("sheet");
  return !!node && !node.hidden;
}

export interface FieldSheetSpec {
  title: string;
  /** The current value when renaming; null or absent when creating. */
  def?: string | null;
  label?: string;
  placeholder?: string;
  /** The primary's word; Rename or Create by default. */
  save?: string;
  /** Gets the TRIMMED value. true: done, the sheet closes. false: a failure the caller has
   *  already reported (the sheet stays, so what was typed sits next to the reason). A string:
   *  an inline validation error, painted under the field instead of dismissed into a toast. */
  submit: (value: string) => Promise<boolean | string> | boolean | string;
}

/** The general single-field sheet (fix-plan #16): one text ask - creating or renaming a group,
 *  renaming an MCP or a table, typing a destructive confirm - so none of them is a browser
 *  prompt(). The sheet owns only the two rules every caller shares: the value is required, and
 *  a rename that changed nothing is a cancel; everything else (a charset, a name the server
 *  refuses) is the caller's submit. */
export function openFieldSheet(spec: FieldSheetSpec): void {
  const def = spec.def ?? null;
  const editing = !!def;
  const input = h("input", { id: "g-name", value: def || "", placeholder: spec.placeholder || tr("ui.sheetPlaceholder"), autocomplete: "off" });
  const err = h("div", { class: "hint bad", id: "g-err", aria: { live: "polite" }, hidden: true });
  const cancel = h("button", { type: "button", class: "btn", id: "g-cancel" }, tr("ui.cancel"));
  const ok = h("button", { type: "button", class: "btn primary", id: "g-save" }, spec.save || (editing ? tr("ui.rename") : tr("ui.create")));
  showSheet(sheet({
    title: spec.title,
    body: [
      h("label", { class: "field" }, h("span", null, spec.label || tr("ui.name")), input),
      err,
    ],
    foot: [h("span", { class: "grow" }), cancel, ok],
  }));
  const fail = (msg: string): void => {
    err.textContent = msg;
    err.hidden = false;
  };
  const save = async (): Promise<void> => {
    const value = input.value.trim();
    if (!value) { fail(tr("ui.nameRequired")); return; }
    if (value === def) { closeSheet(); return; } // a rename that changed nothing is a cancel
    const out = await spec.submit(value);
    if (out === true) closeSheet();
    else if (typeof out === "string") fail(out);
  };
  cancel.onclick = closeSheet;
  ok.onclick = () => { void save(); };
  input.onkeydown = (ev) => { if (ev.key === "Enter") { ev.preventDefault(); void save(); } };
  input.focus();
  if (editing) input.select();
}
