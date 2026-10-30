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

/* ================================================================================================
   The plugin palette - the rail's "..." seat (SPEC §panel.nav, the adaptive shell).

   The rail holds the pinned few; this holds EVERY plugin the host serves, searchable, with
   pinning per browser. Pin state is a preference about this screen, not gateway state, so it
   lives in localStorage like the theme - no server persistence for a UI choice.

   The pure half (pin slice, palette rows, the glyph map) is exported for the vitest suite;
   openPluginPalette builds the one overlay. The caller passes the "go" callback (the shell
   navigates to the group's remembered page, SPEC §panel.nav - the callee picks the page, so the
   policy stays out of the palette) and an "onchange" repaint callback, so this module never
   imports the shell back.
   ================================================================================================ */
                                                                              
import { tr, wireLabel } from "./i18n.js";
import { el, iconNode } from "./util.js";
import { fill } from "./h.js";

const PIN_KEY = "swiss.rail.pinned";
/* The pin store's shape, versioned. v1 was a bare array of ids; v2 wraps them so a
 * list's ERA is knowable on read - the difference the fifteen-seat default needed:
 * a v1 list was saved when the default rail seated seven, so it is a snapshot of
 * THAT default (or a hand-edit of it), and honoring it forever hides every module
 * added since from exactly the browsers that lived through the raise. The owner
 * called that outcome wrong: the raise must reach existing screens, not only fresh
 * ones. So a v1 list is discarded once - the rail falls back to today's default,
 * every served module up to the cap - and the deliberate unpins of the seven-era
 * are the accepted price; from v2 on a saved list is durable (an emptied list
 * stays empty), because only this UI writes it. */
const PIN_STORE_VERSION = 2;

/* How many seats the rail shows before the "..." seat. The default seats EVERY served module
 * up to this cap - modules arrive faster than anyone curates them - and the cap binds saved
 * pins too, so the rail stays a shortlist, never a second sidebar. A full rail scrolls
 * (.rail-nav, base.css), so the cap is curation, not a layout necessity. */
const RAIL_LIMIT = 15;

/* Built-in plugins get the sprite glyph they already own. The remote plugin reuses the mark
 * its MCP type chip already wears (i-remote), the way terminal and plug serve both their seat
 * and their type chip. A plugin the map has never heard of falls back to the PUZZLE piece -
 * the universal plugin mark - so every seat reads as an icon; an initial letter reads as a
 * broken glyph, one seat in a row of drawings that is suddenly type. This is panel-side
 * chrome data, deliberately NOT a descriptor field: adding icon metadata to the wire would
 * tax every plugin author for a panel nicety. */
const GLYPHS                         = { mcp: "mcp", tunnels: "plug", data: "database", jobs: "clock", terminal: "terminal", remote: "remote", host: "gear", memory: "brain", llm: "sparkles", process: "activity" };
const DEFAULT_GLYPH = "puzzle";

function pluginGlyph(group                          )                { return GLYPHS[group.id] || null; }

function glyphNode(group                          )                {
  return iconNode(pluginGlyph(group) || DEFAULT_GLYPH);
}

/* --- pin state (localStorage) ------------------------------------------------------------------- */

function loadPins()                  {
  try {
    const v = JSON.parse(localStorage.getItem(PIN_KEY)          );
    // Only v2 counts: an object with the version and a string array. A bare array
    // is a v1 legacy list - the pre-fifteen era's frozen default - and reads as
    // never-set, which seats today's default. Anything else is the same fall.
    if (v && typeof v === "object" && !Array.isArray(v)
      && v.v === PIN_STORE_VERSION && Array.isArray(v.ids)) {
      return (v.ids             ).filter((x)              => { return typeof x === "string"; });
    }
    return null;
  } catch (e) { return null; }
}
function savePins(ids          )       {
  try { localStorage.setItem(PIN_KEY, JSON.stringify({ v: PIN_STORE_VERSION, ids })); } catch (e) { /* full or blocked */ }
}

/* --- the pure half ------------------------------------------------------------------------------- */

/** The default shortlist: every group the host serves, up to the rail's seats, in group
 *  order - the order the host already decided. Pure so the default is pinnable by a test. */
function defaultPinIds(groups                  )           {
  return groups.slice(0, RAIL_LIMIT).map((g) => { return g.id; });
}

/** The star offers one more seat only while the rail has one free: a list at the cap cannot
 *  grow, so a plugin can never be starred without a seat to show for it. Pure so the
 *  palette's refused state is pinnable by a test. */
function pinFull(pins          )          {
  return pins.length >= RAIL_LIMIT;
}

/** The rail's slice of the groups: the pinned ones, in GROUP order (pinning picks a seat,
 *  it does not reorder the rail). `pins` is optional so tests can pass an explicit list;
 *  without it the store is read - null (never set / unreadable / a discarded v1 list)
 *  means the default. The cap binds the saved branch too, in group order - a backstop
 *  for hand-edited or stale storage; the star's own guard keeps every honest list
 *  under it. */
function pinnedGroups(groups             , pins                  )              {
  const p = pins === undefined ? loadPins() : pins;
  if (p === null) return groups.slice(0, RAIL_LIMIT);
  return groups.filter((g) => { return p.includes(g.id); }).slice(0, RAIL_LIMIT);
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
    { section: tr("pluginPalette.pinned"), groups: pinned.filter(match) },
    { section: tr("pluginPalette.allPlugins"), groups: rest.filter(match) },
  ].filter((s) => { return s.groups.length; });
}

/* --- the overlay -------------------------------------------------------------------------------- */

/** Open the palette over everything (z-index above menus: it is the navigation itself).
 *  groups: registry groups (each may carry .off - every page unavailable). go(group)
 *  navigates - which PAGE that is (the remembered one, SPEC §panel.nav) is the callee's call;
 *  onchange() repaints the rail after a pin toggle. Escape or a click on the backdrop
 *  closes, and focus returns to the "..." seat so the keyboard path does not dead-end. */
function openPluginPalette(groups                , go                                               , onchange             )       {
  closePluginPalette();
  // A stored list - even a deliberately emptied one - is the user's choice verbatim; only a
  // missing or unreadable key falls back to the default.
  const saved = loadPins();
  let pins = saved === null ? defaultPinIds(groups) : saved;

  const back = el("div", "palette-back");
  const card = el("div", "palette");
  card.setAttribute("role", "dialog");
  card.setAttribute("aria-label", tr("pluginPalette.plugins"));
  back.appendChild(card);

  const input = el("input");
  input.type = "search";
  input.placeholder = tr("pluginPalette.searchPlugins");
  input.setAttribute("aria-label", tr("pluginPalette.searchPlugins2"));
  card.appendChild(input);

  const list = el("div", "palette-list");
  card.appendChild(list);

  function rowButton(g              )                    {
    const b = el("button", "pal-row" + (g.off ? " off" : ""));
    b.type = "button";
    b.appendChild(glyphNode(g));
    const name = el("span", "pal-name");
    name.textContent = tr(wireLabel(g.label)) + (g.off ? " " : "");
    if (g.off) name.appendChild(el("span", "pal-off", "· " + tr("pluginPalette.off")));
    b.appendChild(name);
    b.title = g.off ? (g.offDetail || tr("pageRegistry.pluginDisabled")) : tr("pluginPalette.openLabel", { label: tr(wireLabel(g.label)) });
    b.onclick = (ev) => { ev.stopPropagation(); closePluginPalette(); void go(g); };
    return b;
  }

  function pinButton(g              )                    {
    const pinned = pins.includes(g.id);
    // A star beyond the cap would promise a seat that does not exist, so it is refused -
    // disabled with the reason in the title - rather than silently seating nobody.
    const full = !pinned && pinFull(pins);
    const p = el("button", "pal-pin");
    p.type = "button";
    p.appendChild(iconNode("star", tr(full ? "pluginPalette.railFull" : pinned ? "pluginPalette.unpinLabel" : "pluginPalette.pinLabel", { label: tr(wireLabel(g.label)) })));
    p.className = "pal-pin" + (pinned ? " on" : "");
    p.setAttribute("aria-pressed", String(pinned));
    p.disabled = full;
    p.title = full ? tr("pluginPalette.railFull") : tr(pinned ? "pluginPalette.removeRail" : "pluginPalette.pinRail");
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
    if (!list.children.length) list.appendChild(el("div", "pal-none", tr("pluginPalette.pluginsMatch")));
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

export { closePluginPalette, defaultPinIds, glyphNode, openPluginPalette, paletteRows, pinFull, pinnedGroups, pluginGlyph, RAIL_LIMIT };
