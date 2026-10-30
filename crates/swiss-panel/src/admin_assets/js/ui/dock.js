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

/* The dock (SPEC §panel.ui): a pane over the one that takes the rest - an editor over its
 * results - with the lower pane's own bar between them as the handle, the way a desktop SQL
 * client's result pane hangs under its editor. Three parts, in a column:
 *
 *   top     the caller's pane, sized here (its flex basis);
 *   head    the caller's bar (the result tabs and facts) in a row the dock owns: a hairline
 *           on its top edge, and two quiet buttons at its right end - fold the lower pane down
 *           to the head, and give it the whole column;
 *   bottom  the caller's pane that takes what is left.
 *
 * The head is the handle: a press on any of it that is not a control (a tab, a button) drags
 * the edge, the line turning accent under the pointer and while held. A drag below half the
 * top pane's minimum folds the top pane away, past the room the lower pane keeps by half of it
 * folds the lower pane down; either fold keeps the size the drag started from, so bringing the
 * pane back gives it the room it had. A double-click on the head (Enter on its focused edge)
 * folds the lower pane, or brings back whichever pane is folded. dockShow lets the page bring
 * a pane back - a run answering into folded results.
 *
 * At rest it draws one hairline and two ghost icons: no floating handle, no line across the
 * page until the pointer finds it. A hidden top pane (the page shows something else there)
 * hides the head too, and the lower pane fills the column whatever is folded.
 *
 * The size and a folded lower pane are remembered per browser (swiss_dock_<key>); the whole
 * column given to the lower pane is a look, not a layout, and ends with the paint. Each paint
 * builds the dock again from the stored value. A change ends with a window resize event, so
 * the pages that measure their space remeasure.
 *
 * A DOM-only mechanism: no API, no app state. */
import { h } from "../h.js";
import { tr } from "../i18n.js";
import { iconBtn } from "./button.js";

                           
                                                                                      
              
                                                              
               
                                                                                      
              
                                                                                                
                  
                                                                
                
                    
 

                                    
                                                

const STEP = 16; // one arrow key press
const SLOP = 3; // a press that moved this little is still a click

/* A press on one of these is the control's, not the edge's. */
const CONTROL = "button, a, input, select, textarea, label, [role=tab]";

function storeKey(key        )         { return "swiss_dock_" + key; }

function load(o          )            {
  const st            = { size: o.size, fold: null };
  try {
    const raw = localStorage.getItem(storeKey(o.key));
    const j          = raw ? JSON.parse(raw) : null;
    if (j && typeof j === "object") {
      if ("size" in j && typeof j.size === "number" && isFinite(j.size)) st.size = Math.max(o.min, Math.round(j.size));
      if ("fold" in j && j.fold === "bottom") st.fold = "bottom";
    }
  } catch (e) { /* blocked or garbled - the default stands */ }
  return st;
}

function save(o          , s           )       {
  try {
    const fold = s.fold === "bottom" ? "bottom" : null;
    if (s.size === o.size && !fold) localStorage.removeItem(storeKey(o.key));
    else localStorage.setItem(storeKey(o.key), JSON.stringify({ size: s.size, fold }));
  } catch (e) { /* blocked - only forgotten */ }
}

function announce()       {
  if (typeof window !== "undefined" && typeof window.dispatchEvent === "function") window.dispatchEvent(new Event("resize"));
}

function setIcon(b             , name        , label        )       {
  b.querySelector("use")?.setAttribute("href", "#i-" + name);
  b.setAttribute("aria-label", label);
  b.title = label;
}

/* The newest dock built under each key, for dockShow: a page paints its dock again on every
 * entry, so the newest one is the one on screen. */
const live = new Map                                           ();

/** Bring back a pane a dock folded away: a run that answers into folded results unfolds them.
 *  Nothing folded there, nothing changes. */
export function dockShow(key        , which                  )       {
  live.get(key)?.(which);
}

export function dock(top             , bar             , bottom             , o          )              {
  let st = load(o);
  const edge = h("div", {
    class: "dock-edge", role: "separator", tabIndex: 0,
    title: tr("ui.dock.tip", { name: o.restLabel }),
    aria: { orientation: "horizontal", label: tr("ui.dock.resize", { name: o.label }), valuemin: String(o.min), controls: top.id || null },
  });
  const foldBtn = iconBtn("chevron-down", "", { ghost: true });
  const maxBtn = iconBtn("expand", "", { ghost: true });
  const head = h("div", { class: "dock-head" }, edge, bar, h("span", { class: "dock-tools" }, foldBtn, maxBtn));
  top.classList.add("dock-top");
  bottom.classList.add("dock-bottom");
  const root = h("div", { class: "dock" }, top, head, bottom);

  // The room the top pane may take: the column less the head and what the lower pane keeps.
  const maxOf = ()         => {
    const room = root.getBoundingClientRect().height - head.getBoundingClientRect().height - o.restMin;
    return Math.max(o.min, Math.round(room) || o.min);
  };
  const paint = ()       => {
    if (st.fold) root.dataset.fold = st.fold;
    else delete root.dataset.fold;
    // Folded down under the head, the lower pane leaves the top pane the whole column.
    top.style.flex = st.fold === "bottom" ? "1 1 auto" : "0 1 " + st.size + "px";
    edge.setAttribute("aria-valuenow", String(st.fold === "top" ? 0 : st.size));
    setIcon(foldBtn, st.fold === "bottom" ? "chevron-up" : "chevron-down",
      tr(st.fold === "bottom" ? "ui.dock.show" : "ui.dock.hide", { name: o.restLabel }));
    setIcon(maxBtn, st.fold === "top" ? "collapse" : "expand",
      st.fold === "top" ? tr("ui.dock.show", { name: o.label }) : tr("ui.dock.maximize", { name: o.restLabel }));
  };
  const commit = (next           )       => {
    st = next;
    paint();
    save(o, st);
    announce();
  };
  // Enter and the double-click: bring back whatever is folded, else fold the lower pane down.
  const toggle = ()       => { commit({ size: st.size, fold: st.fold ? null : "bottom" }); };
  live.set(o.key, (which) => { if (st.fold === which) commit({ size: st.size, fold: null }); });

  foldBtn.addEventListener("click", () => { commit({ size: st.size, fold: st.fold === "bottom" ? null : "bottom" }); });
  maxBtn.addEventListener("click", () => { commit({ size: st.size, fold: st.fold === "top" ? null : "top" }); });

  // The drag: the head captures the pointer, so a fast hand that leaves it is still followed;
  // the body carries the mark meanwhile so the cursor holds and no text selects. A folded top
  // pane's drag starts from nothing, and one filling the column (the lower pane folded) from
  // what it fills, so the hand brings either back.
  let from = 0, base = 0, hi = 0, keep = 0, moved = false, held = false;
  const control = (t                    )          => {
    const el = t instanceof Element ? t.closest(CONTROL) : null;
    return !!el && head.contains(el);
  };
  head.addEventListener("pointerdown", (e              ) => {
    if (e.button !== 0 || control(e.target)) return;
    e.preventDefault();
    from = e.clientY;
    keep = st.size;
    base = st.fold === "top" ? 0 : st.fold === "bottom" ? Math.round(top.getBoundingClientRect().height) || st.size : st.size;
    hi = maxOf();
    moved = false;
    held = true;
    if (typeof head.setPointerCapture === "function") { try { head.setPointerCapture(e.pointerId); } catch (err) { /* a synthetic pointer */ } }
    root.classList.add("dragging");
    document.body.dataset.dock = "";
  });
  head.addEventListener("pointermove", (e              ) => {
    if (!held) return;
    const dy = e.clientY - from;
    if (!moved && Math.abs(dy) <= SLOP) return;
    moved = true;
    const raw = base + dy;
    const fold       = raw < o.min / 2 ? "top" : raw > hi + o.restMin / 2 ? "bottom" : null;
    st = { size: fold ? keep : Math.min(hi, Math.max(o.min, Math.round(raw))), fold };
    paint();
  });
  const end = ()       => {
    if (!held) return;
    held = false;
    root.classList.remove("dragging");
    delete document.body.dataset.dock;
    if (moved) { save(o, st); announce(); }
  };
  head.addEventListener("pointerup", end);
  head.addEventListener("pointercancel", end);
  head.addEventListener("lostpointercapture", end);
  head.addEventListener("dblclick", (e            ) => {
    if (!control(e.target)) toggle();
  });
  edge.addEventListener("keydown", (e               ) => {
    let size                = null;
    if (e.key === "Enter" || e.key === " ") size = -1;
    else if (st.fold) return;
    else if (e.key === "ArrowUp") size = st.size - STEP;
    else if (e.key === "ArrowDown") size = st.size + STEP;
    else if (e.key === "Home") size = o.min;
    else if (e.key === "End") size = maxOf();
    if (size === null) return;
    e.preventDefault();
    if (size < 0) { toggle(); return; }
    commit({ size: Math.min(maxOf(), Math.max(o.min, size)), fold: null });
  });
  paint();
  return root;
}
