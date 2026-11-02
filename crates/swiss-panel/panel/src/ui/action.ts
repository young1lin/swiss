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

/* The action vocabulary (SPEC §panel.design rule 7): WHAT A CLICK DOES maps to the glyph that
 * says it, once, so the same verb wears the same picture on every page.
 *
 * Two rules decide this table, and both were paid for:
 *
 *   AN ICON SAYS WHAT HAPPENS NEXT, NOT WHAT IS TRUE NOW. The word in the accessible name is the
 *   click's consequence ("Disable this MCP"), never the current state - a state mark is the dot,
 *   the tag or the row's own words. That is why enable and disable are two glyphs (i-power and
 *   i-power-off) instead of one power glyph plus a hover: with one picture the pair is only
 *   distinguishable by reading a tooltip, and a reader who does not hover clicks blind.
 *
 *   VERBS THAT ARE NOT THE SAME VERB ARE NOT THE SAME GLYPH. Refresh, restart, stop and disable
 *   each have their own: refresh asks again, restart tears down and builds back, stop ends the
 *   run, disable leaves a machine turned off. Sharing one arrow for all four would be a lie
 *   about what the click costs.
 *
 * A page never names a glyph for an action directly: it calls actionBtn/actionIcon with the verb
 * and lets this table pick. A verb the table lacks is added HERE first (with its sprite symbol
 * and its gallery row), not invented at a call site.
 *
 * What is NOT an action button: a real setting stays sw() - a switch states a value, an action
 * button performs one - and a form's Save/Cancel, an empty state's invitation and every menu row
 * keep their words (SPEC §panel.design rule 4). No handlers here: a view answers through its
 * delegated listener on the data hook it passed in (SPEC §panel.toolchain). */
import type { IconBtnOpts } from "./button.js";
import { iconBtn, setIconBtn } from "./button.js";

export type ActionId =
  | "new"
  | "newGroup"
  | "enable"
  | "disable"
  | "start"
  | "bookmark"
  | "unbookmark"
  | "startAll"
  | "stop"
  | "pause"
  | "resume"
  | "refresh"
  | "restart"
  | "edit"
  | "copy"
  | "download"
  | "upload"
  | "delete"
  | "close"
  | "more"
  | "test";

/** Action → sprite symbol. The one table; a call site names the verb, never the glyph. */
export const ACTION_ICON: Record<ActionId, string> = {
  new: "plus",
  newGroup: "folder-plus",
  enable: "power",
  disable: "power-off",
  start: "play",
  startAll: "list-play",
  bookmark: "bookmark",
  unbookmark: "bookmark-minus",
  // The solid square. It is drawn in --red only while the button can be pressed (ACTION_TONE).
  stop: "stop",
  // Only a thing that can actually be paused offers pause (SPEC §panel.design): today nothing on
  // the panel does, so these two are the vocabulary's promise, not a button a page may borrow.
  pause: "pause",
  resume: "play",
  // Read again. NOT a restart: the gateway is asked for a fresh reading, nothing is rebuilt.
  refresh: "refresh-cw",
  // Tear down and build back - the MCP and process restart. NOT a refresh.
  restart: "rotate-cw",
  edit: "pencil",
  copy: "copy",
  download: "download",
  upload: "upload",
  delete: "trash",
  close: "x",
  more: "ellipsis",
  // Try a connection or an endpoint: dial it and report. Its own verb, so its own glyph.
  test: "plug",
};

/** The verbs that wear a tone (SPEC §panel.design rule 2). Stop is the only one: its square is
 *  --red while the button is pressable, and grey when it is not (ui.css, data-tone="stop"). */
export const ACTION_TONE: Partial<Record<ActionId, "stop">> = { stop: "stop" };

/** The glyph for an action, when a labelled form control needs its shared mark. */
export function actionIcon(action: ActionId): string { return ACTION_ICON[action]; }

/** The hooks a verb leaves on its button: data-action always, data-tone only where the verb has one.
 *  Building and restating both come through here, so a first render and a state flip end with the
 *  same attributes - a Stop restated as a Start cannot keep the red of the Stop it replaced. */
function paint(b: HTMLButtonElement, action: ActionId): void {
  b.dataset.action = action;
  const tone = ACTION_TONE[action];
  if (tone) b.dataset.tone = tone;
  else delete b.dataset.tone;
}

/** An icon button whose glyph is the action's: the label (the accessible name AND the tooltip)
 *  says what the click will do, so it is written as a consequence, not as a state. */
export function actionBtn(action: ActionId, label: string, o: IconBtnOpts = {}): HTMLButtonElement {
  const b = iconBtn(ACTION_ICON[action], label, o);
  paint(b, action);
  return b;
}

/** Restate a built action button in place - glyph, name, tooltip, the data-action hook and the tone
 *  move together, so a state flip (Disable → Enable, Start → Stop) never leaves a power glyph over
 *  a start verb, and never writes textContent into a sprite (which would delete the icon). */
export function setActionBtn(b: HTMLButtonElement, action: ActionId, label: string, title?: string): void {
  setIconBtn(b, ACTION_ICON[action], label, title);
  paint(b, action);
}

/** An action button while its request is in flight (SPEC §panel.design rule 7). Busy: the glyph
 *  stays (it is still the verb being carried out), the name and the tooltip say the wait in words
 *  ("Stopping..."), and the button is disabled so a second click cannot send a second request. Idle
 *  (busy = null): the verb's own words come back and the button is enabled again, so a Stop whose
 *  request failed is a Stop once more. The node and its listeners stay the caller's. */
export function setActionBusy(b: HTMLButtonElement, action: ActionId, idle: { label: string; title?: string }, busy: string | null): void {
  if (busy === null) {
    setActionBtn(b, action, idle.label, idle.title);
    b.disabled = false;
    return;
  }
  setActionBtn(b, action, busy, busy);
  b.disabled = true;
}
