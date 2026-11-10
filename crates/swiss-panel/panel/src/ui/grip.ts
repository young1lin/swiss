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

/* The grip (SPEC §panel.ui): the edge between two panes that the reader drags to give one of
 * them more room - a sidebar's right edge, a workspace's side column, the list beside the
 * record picked from it (ui/workspace.ts wsSplit). It sits between the
 * pane it sizes (the one BEFORE it) and the pane that takes the rest, and draws nothing of its
 * own until the pointer finds it: a hairline turns accent under the pointer and while held.
 *
 *   axis "x"  sizes a column's width - a sidebar beside its page;
 *   axis "y"  sizes a row's height.
 *
 * A sidebar may fold away (`fold`): a drag past half its minimum, a double-click on the edge,
 * Enter on the focused edge, or the chevron that shows on the edge while the sidebar is under
 * the pointer. A folded sidebar leaves a thin strip whose click brings it back. Without `fold`
 * a double-click puts the size back to its default. A page's own control (a toolbar button
 * that opens an assistant) folds or brings the pane back through gripFold.
 *
 * `end` turns the edge around: the pane it sizes is the one AFTER it, a column at the page's
 * end edge (an assistant beside the work), so a drag towards the start widens it and its
 * chevron points the other way.
 *
 * An editor over its results, where the pane after the edge folds too, is a dock
 * (ui/dock.ts), not a grip.
 *
 * The size and the fold are remembered per browser under the caller's key (swiss_grip_<key>),
 * so a sidebar keeps its width across pages and reloads; a blocked storage only forgets. The
 * grip writes the size onto the pane's own style; with no `size` given and nothing remembered
 * it writes none, and the pane's stylesheet size (a token, a narrow-window rule) stands. Each
 * paint builds the grip again from the stored value, so nothing a page repaints can lose it.
 * A change ends with a window resize event: the pages that measure their space (the terminal,
 * the tab strips) remeasure, as they do for Focus mode.
 *
 * A DOM-only mechanism: no API, no app state. */
import { h } from "../h.js";
import { tr } from "../i18n.js";
import { iconNode } from "./icon.js";

export interface GripOpts {
  axis: "x" | "y";
  /** Where the size is remembered: one key per place ("side", "db-side", "db-console"). */
  key: string;
  /** The size with nothing remembered, in px; left out, the pane's stylesheet size. */
  size?: number;
  /** The bounds a drag keeps to, in px. `max` may be read when the drag starts: the room the
   *  page has then. */
  min: number;
  max: number | (() => number);
  /** The pane's name, for the edge's accessible label ("Tables", "MCPs"). */
  label: string;
  fold?: boolean;
  /** Folded while nothing is remembered: a pane the reader opens when they want it. */
  folded?: boolean;
  /** The pane sized is the one after the edge, at the page's end. */
  end?: boolean;
}

/** `size` null: the default stands (the caller's `size`, else the stylesheet's). `folded`: the
 *  pane is put away. */
interface GripState { size: number | null; folded: boolean }

function state(size: number | null, folded: boolean): GripState {
  return { size, folded };
}

const STEP = 16; // one arrow key press

function storeKey(key: string): string { return "swiss_grip_" + key; }

/** The remembered state, or the default. Pure but for the read. */
export function gripState(o: GripOpts): GripState {
  let st: GripState = state(o.size ?? null, !!o.fold && !!o.folded);
  try {
    const raw = localStorage.getItem(storeKey(o.key));
    const j: unknown = raw ? JSON.parse(raw) : null;
    if (j && typeof j === "object") {
      const size = "size" in j && typeof j.size === "number" && isFinite(j.size) ? Math.max(o.min, Math.round(j.size)) : st.size;
      st = state(size, "folded" in j && j.folded === true && !!o.fold);
    }
  } catch (e) { /* blocked or garbled - the default stands */ }
  return st;
}

function save(o: GripOpts, s: GripState): void {
  try {
    if (s.size === null && !s.folded) localStorage.removeItem(storeKey(o.key));
    else localStorage.setItem(storeKey(o.key), JSON.stringify(s));
  } catch (e) { /* blocked - only forgotten */ }
}

function maxOf(o: GripOpts): number {
  const m = typeof o.max === "function" ? o.max() : o.max;
  return Math.max(o.min, isFinite(m) ? m : o.min);
}

/** The event gripFold sends: the edge answers it as its own chevron would. */
const FOLD_EVENT = "grip-fold";

/** Fold the pane an edge sizes (true) or bring it back (false), remembered as the reader's
 *  own fold is - for a page control that opens or puts away the pane. */
export function gripFold(edge: HTMLElement, folded: boolean): void {
  edge.dispatchEvent(new CustomEvent<boolean>(FOLD_EVENT, { detail: folded }));
}

/** Whether the pane an edge sizes is folded away. */
export function gripFolded(edge: HTMLElement): boolean {
  return edge.classList.contains("folded");
}

function announce(): void {
  if (typeof window !== "undefined" && typeof window.dispatchEvent === "function") window.dispatchEvent(new Event("resize"));
}

export function grip(target: HTMLElement, o: GripOpts): HTMLElement {
  let st = gripState(o);
  // A foldable sidebar's chevron.
  // Its chevron points the way the pane goes: towards its own edge of the page.
  const away = o.end ? "chevron-right" : "chevron-left";
  const back = o.end ? "chevron-left" : "chevron-right";
  const foldBtn = o.fold ? h("button", { type: "button", class: "grip-btn", tabIndex: -1 }, iconNode(away)) : null;
  const g = h("div", {
    class: "grip " + o.axis + (o.end ? " end" : ""), role: "separator", tabIndex: 0,
    aria: { orientation: o.axis === "x" ? "vertical" : "horizontal", valuemin: String(o.min), controls: target.id || null },
  }, foldBtn);
  const buttonAt = (t: EventTarget | null): HTMLElement | null =>
    foldBtn && foldBtn.contains(t as Node) ? foldBtn : null;

  // The size the pane has on screen, and the one it keeps: the state's, else the measured one.
  const measured = (): number => {
    const r = target.getBoundingClientRect();
    return Math.round((o.axis === "x" ? r.width : r.height) || o.min);
  };
  const current = (): number => st.size ?? measured();
  const paint = (): void => {
    const px = st.size === null ? "" : st.size + "px";
    if (o.axis === "x") target.style.width = px;
    else target.style.flex = px ? "0 1 " + px : "";
    if (o.fold) target.style.display = st.folded ? "none" : "";
    g.classList.toggle("folded", st.folded);
    // A stylesheet size is unknown until laid out: the value waits for the first change.
    const now = st.folded ? 0 : st.size;
    if (now !== null) g.setAttribute("aria-valuenow", String(now));
    g.setAttribute("aria-label", tr(st.folded ? "ui.grip.show" : "ui.grip.resize", { name: o.label }));
    g.title = st.folded ? tr("ui.grip.show", { name: o.label }) : tr(o.fold ? "ui.grip.tipFold" : "ui.grip.tip");
    if (foldBtn) {
      const label = tr(st.folded ? "ui.grip.show" : "ui.grip.hide", { name: o.label });
      foldBtn.setAttribute("aria-label", label);
      foldBtn.title = label;
      const use = foldBtn.querySelector("use");
      if (use) use.setAttribute("href", "#i-" + (st.folded ? back : away));
    }
  };
  const commit = (next: GripState): void => {
    st = next;
    paint();
    save(o, st);
    announce();
  };
  // Enter, the double-click and the chevron: fold the pane or bring it back; without a fold,
  // put the default size back.
  const toggle = (): void => {
    if (st.folded) commit(state(st.size, false));
    else if (o.fold) commit(state(st.size, true));
    else commit(state(o.size ?? null, false));
  };

  // The drag: the pointer is captured by the edge, so a fast hand that leaves it is still
  // followed; the body carries the axis meanwhile so the cursor holds and no text selects.
  // The chevron sits mid-edge, where a hand most often grabs, so a press on it is a drag too:
  // the edge captures it at once (a hand's first move can already leave a 20 px button), and a
  // press that never moved is the button's click, answered at the release - the click that
  // follows lands on the edge that held the pointer, so it is the release's own (`handled`).
  // A folded pane's drag starts from nothing, so the hand brings it back.
  // A drag that ends in a fold keeps the size the pane had when the hand took the edge, not the
  // bound the drag crossed on its way, so bringing the pane back gives it the room it had.
  let from = 0, base = 0, moved = false, hi = 0, handled = false;
  let keep: number | null = null;
  let pressed: HTMLElement | null = null;
  g.addEventListener("pointerdown", (e: PointerEvent) => {
    handled = false;
    if (e.button !== 0) return;
    e.preventDefault();
    from = o.axis === "x" ? e.clientX : e.clientY;
    keep = st.size;
    base = st.folded ? 0 : current();
    hi = maxOf(o);
    moved = false;
    pressed = buttonAt(e.target);
    if (typeof g.setPointerCapture === "function") { try { g.setPointerCapture(e.pointerId); } catch (err) { /* a synthetic pointer */ } }
    g.classList.add("dragging");
    document.body.dataset.grip = o.axis;
  });
  g.addEventListener("pointermove", (e: PointerEvent) => {
    if (!g.classList.contains("dragging")) return;
    const travel = (o.axis === "x" ? e.clientX : e.clientY) - from;
    const raw = base + (o.end ? -travel : travel);
    if (!moved && Math.abs(raw - base) <= 2) return;
    moved = true;
    // Past half the minimum a foldable pane folds - the drag that would crush it hides it.
    const folded = !!o.fold && raw < o.min / 2;
    st = state(folded ? keep : Math.min(hi, Math.max(o.min, Math.round(raw))), folded);
    paint();
  });
  const end = (): void => {
    if (!g.classList.contains("dragging")) return;
    g.classList.remove("dragging");
    delete document.body.dataset.grip;
    // The click this release makes is the drag's own, not a request to unfold what it folded.
    handled = moved || pressed !== null;
    if (moved) { save(o, st); announce(); } else if (pressed) toggle();
  };
  g.addEventListener("pointerup", end);
  g.addEventListener("pointercancel", end);
  g.addEventListener("lostpointercapture", end);
  g.addEventListener("dblclick", (e: MouseEvent) => {
    if (st.folded || buttonAt(e.target)) return;
    toggle();
  });
  g.addEventListener("click", (e: MouseEvent) => {
    if (handled) { handled = false; return; }
    // A button (a click with no press before it: a keyboard or a screen reader), or anywhere
    // on a folded strip.
    const b = buttonAt(e.target);
    if (b || st.folded) { e.stopPropagation(); toggle(); }
  });
  g.addEventListener("keydown", (e: KeyboardEvent) => {
    const towardsStart = o.axis === "x" ? "ArrowLeft" : "ArrowUp";
    const towardsEnd = o.axis === "x" ? "ArrowRight" : "ArrowDown";
    const narrower = o.end ? towardsEnd : towardsStart;
    const wider = o.end ? towardsStart : towardsEnd;
    let size: number | null = null;
    if (e.key === "Enter" || e.key === " ") size = -1;
    else if (st.folded) return;
    else if (e.key === narrower) size = current() - STEP;
    else if (e.key === wider) size = current() + STEP;
    else if (e.key === "Home") size = o.min;
    else if (e.key === "End") size = maxOf(o);
    if (size === null) return;
    e.preventDefault();
    if (size < 0) { toggle(); return; }
    commit(state(Math.min(maxOf(o), Math.max(o.min, size)), false));
  });
  g.addEventListener(FOLD_EVENT, (e: Event) => {
    const folded = e instanceof CustomEvent && e.detail === true;
    if (folded !== st.folded && (!folded || o.fold)) toggle();
  });
  paint();
  return g;
}
