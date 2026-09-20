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
   The plugin palette - the rail's "..." seat (docs/13 D5, the adaptive shell).

   The rail holds the pinned few; this holds EVERY plugin the host serves, searchable, with
   pinning per browser. Pin state is a preference about this screen, not gateway state, so it
   lives in localStorage like the theme - no server persistence for a UI choice.

   The pure half (pin slice, palette rows, the glyph map) is exported for the vitest suite;
   openPluginPalette builds the one overlay. The caller passes the "go" callback (the shell
   navigates to the group's remembered page, docs/39 S4 - the callee picks the page, so the
   policy stays out of the palette) and an "onchange" repaint callback, so this module never
   imports the shell back.
   ================================================================================================ */
                                                                              
import { el, iconNode } from "./util.js";
import { fill } from "./h.js";

const PIN_KEY = "swiss.rail.pinned";
/* How many seats the rail shows before the "..." seat. The palette is the real list; the
 * rail is the shortlist, so the limit is about the rail's height, not the host's capacity. */
const RAIL_LIMIT = 7;

/* Built-in plugins get the sprite glyph they already own. The remote plugin reuses the mark
 * its MCP type chip already wears (i-remote), the way terminal and plug serve both their seat
 * and their type chip. A plugin the map has never heard of falls back to the PUZZLE piece -
 * the universal plugin mark - so every seat reads as an icon; an initial letter reads as a
 * broken glyph, one seat in a row of drawings that is suddenly type. This is panel-side
 * chrome data, deliberately NOT a descriptor field: adding icon metadata to the wire would
 * tax every plugin author for a panel nicety. */
const GLYPHS                         = { mcp: "mcp", tunnels: "plug", data: "database", jobs: "clock", terminal: "terminal", remote: "remote", host: "gear" };
const DEFAULT_GLYPH = "puzzle";

function pluginGlyph(group                          )                { return GLYPHS[group.id] || null; }

function glyphNode(group                          )                {
  return iconNode(pluginGlyph(group) || DEFAULT_GLYPH);
}

/* --- pin state (localStorage) ------------------------------------------------------------------- */

function loadPins() {
  try {
    const v = JSON.parse(localStorage.getItem(PIN_KEY)          );
    return Array.isArray(v) ? v.filter((x) => { return typeof x === "string"; }) : null;
  } catch (e) { return null; }
}
function savePins(ids          )       {
  try { localStorage.setItem(PIN_KEY, JSON.stringify(ids)); } catch (e) { /* full or blocked */ }
}

/* --- the pure half ------------------------------------------------------------------------------- */

/** The default shortlist: the first RAIL_LIMIT groups, in group order - the order the host
 *  already decided. Pure so the default is pinnable by a test. */
function defaultPinIds(groups                  )           {
  return groups.slice(0, RAIL_LIMIT).map((g) => { return g.id; });
}

/** The rail's slice of the groups: the pinned ones, in GROUP order (pinning picks a seat,
 *  it does not reorder the rail). `pins` is optional so tests can pass an explicit list;
 *  without it the store is read - null (never set / unreadable) means the default. */
function pinnedGroups(groups             , pins                  )              {
  const p = pins === undefined ? loadPins() : pins;
  if (!p) return groups.slice(0, RAIL_LIMIT);
  return groups.filter((g) => { return p?.includes(g.id); });
}

/** The palette's sections given a query: Pinned first, then All plugins, each filtered by
 *  the query against label and id. Empty sections drop out. Pure: groups + pins + query in,
 *  sections out. */
function paletteRows(groups                , pins          , query        )                   {
  const q = String(query || "").trim().toLowerCase();
  const match = (g              )          => {
    return !q || String(g.label).toLowerCase().includes(q) || String(g.id).toLowerCase().includes(q);
  };
  const pinned                 = [];
  const rest                 = [];
  groups.forEach((g) => { (pins.includes(g.id) ? pinned : rest).push(g); });
  return [
    { section: "Pinned", groups: pinned.filter(match) },
    { section: "All plugins", groups: rest.filter(match) },
  ].filter((s) => { return s.groups.length; });
}

/* --- the overlay -------------------------------------------------------------------------------- */

/** Open the palette over everything (z-index above menus: it is the navigation itself).
 *  groups: registry groups (each may carry .off - every page unavailable). go(group)
 *  navigates - which PAGE that is (the remembered one, docs/39 S4) is the callee's call;
 *  onchange() repaints the rail after a pin toggle. Escape or a click on the backdrop
 *  closes, and focus returns to the "..." seat so the keyboard path does not dead-end. */
function openPluginPalette(groups                , go                                               , onchange             )       {
  closePluginPalette();
  let pins = loadPins() || defaultPinIds(groups);

  const back = el("div", "palette-back");
  const card = el("div", "palette");
  card.setAttribute("role", "dialog");
  card.setAttribute("aria-label", "Plugins");
  back.appendChild(card);

  const input = el("input");
  input.type = "search";
  input.placeholder = "Search plugins…";
  input.setAttribute("aria-label", "Search plugins");
  card.appendChild(input);

  const list = el("div", "palette-list");
  card.appendChild(list);

  function rowButton(g              )                    {
    const b = el("button", "pal-row" + (g.off ? " off" : ""));
    b.type = "button";
    b.appendChild(glyphNode(g));
    const name = el("span", "pal-name");
    name.textContent = g.label + (g.off ? " " : "");
    if (g.off) name.appendChild(el("span", "pal-off", "· off"));
    b.appendChild(name);
    b.title = g.off ? (g.offDetail || "Plugin disabled") : "Open " + g.label;
    b.onclick = (ev) => { ev.stopPropagation(); closePluginPalette(); void go(g); };
    return b;
  }

  function pinButton(g              )                    {
    const pinned = pins.includes(g.id);
    const p = el("button", "pal-pin");
    p.type = "button";
    p.appendChild(iconNode("star", (pinned ? "Unpin " : "Pin ") + g.label));
    p.className = "pal-pin" + (pinned ? " on" : "");
    p.setAttribute("aria-pressed", String(pinned));
    p.title = pinned ? "Remove from the rail" : "Pin to the rail";
    p.onclick = (ev) => {
      ev.stopPropagation();
      pins = pins.includes(g.id) ? pins.filter((x) => { return x !== g.id; }) : pins.concat([g.id]);
      savePins(pins);
      render();
      if (onchange) onchange();
    };
    return p;
  }

  function render() {
    fill(list);
    paletteRows(groups, pins, input.value).forEach((section) => {
      list.appendChild(el("div", "pal-section", section.section));
      section.groups.forEach((g) => {
        const holder = el("div", "pal-item");
        holder.appendChild(rowButton(g));
        holder.appendChild(pinButton(g));
        list.appendChild(holder);
      });
    });
    if (!list.children.length) list.appendChild(el("div", "pal-none", "No plugins match."));
  }

  input.oninput = render;
  input.onkeydown = (ev) => {
    const rows                      = Array.prototype.slice.call(list.querySelectorAll                   (".pal-row"));
    const i = rows.indexOf(document.activeElement                     );
    if (ev.key === "Escape") { ev.preventDefault(); closePluginPalette(); return; }
    if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
      ev.preventDefault();
      let n = ev.key === "ArrowDown" ? i + 1 : i - 1;
      if (i < 0) n = ev.key === "ArrowDown" ? 0 : rows.length - 1; /* from the input */
      if (n < 0) n = 0;
      if (n >= rows.length) n = rows.length - 1;
      if (rows[n]) rows[n].focus();
    } else if (ev.key === "Enter" && i < 0) {
      ev.preventDefault();
      const first = list.querySelector                   (".pal-row");
      if (first) first.click();
    }
  };
  list.addEventListener("keydown", (ev) => {
    if (ev.key !== "Escape") return;
    ev.preventDefault();
    closePluginPalette();
  });

  back.onclick = (ev) => { if (ev.target === back) closePluginPalette(); };
  document.body.appendChild(back);
  render();
  input.focus();
}

function closePluginPalette()       {
  const back = document.querySelector(".palette-back");
  if (back) back.remove();
  const more = document.getElementById("railMore");
  if (more && typeof more.focus === "function") more.focus();
}

export { closePluginPalette, defaultPinIds, glyphNode, openPluginPalette, paletteRows, pinnedGroups, pluginGlyph, RAIL_LIMIT };
