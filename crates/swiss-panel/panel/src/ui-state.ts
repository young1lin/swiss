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

/* The shell owns its state (docs/37 R4, slice 3 of 7): which page is on screen, whether a menu
   has the pointer latched, the MCP list's filter and fold map, the two drag slots a poll freezes
   on, the group a header "+" targets, and the build stamp the reload check compares against.

   A leaf module on purpose — it imports nothing. Twenty-three modules read this slice, and the
   shell files that would otherwise host it (pane, menu, page-registry) sit downstream of most of
   them, so a record exported from any of those closes an import cycle. The record never leaves
   this file: swapping it for a store or signals later is one file's work. */
const ui: {
  view: string;
  menuOpen: boolean;
  filter: string;
  collapsed: Record<string, boolean>;
  dragging: string | null;
  draggingGroup: string | null;
  addGroup: string | null;
  panelVersion: string | null;
} = {
  view: "mcps",        // the toolbar switcher: "mcps" | "tunnels" | "traffic" | "data" | "jobs" | …
  menuOpen: false,     // a row/cell menu holds the pointer; outside clicks and Escape close it first
  filter: "",          // the sidebar's MCP filter box — a non-empty filter unfolds every group
  collapsed: {},       // mcps fold map; group name -> true. Panel-only, so localStorage keeps it (groups.ts)
  dragging: null,      // name of the ROW being dragged — polls must not rebuild under it
  draggingGroup: null, // name of the GROUP HEADER being dragged — polls must not rebuild under it either
  addGroup: null,      // sidebar group a header "+" targets for the next created MCP (add-sheet.ts)
  panelVersion: null,  // admin.html mtime stamp from /api/info; a change means a new build landed
};

/** Which page the shell is showing. page-registry owns the switch; the polls read it to stay quiet
 *  about views that are not on screen. */
export function currentView(): string { return ui.view; }
export function setCurrentView(id: string): void { ui.view = id; }

/** A menu has the pointer latched. Every outside-click closer asks this before it acts, which is
 *  why a row menu and the detail pane do not both react to the same click. */
export function menuIsOpen(): boolean { return ui.menuOpen; }
export function setMenuOpen(open: boolean): void { ui.menuOpen = open; }

/** The sidebar's filter text, raw — callers that care about whitespace trim it themselves. */
export function listFilter(): string { return ui.filter; }
export function setListFilter(text: string): void { ui.filter = text; }

/** The mcps fold map. The groups component takes the whole record and writes group keys into it,
 *  so this hands out the reference on purpose: the MAP is the component's write surface, the slot
 *  is not — replacing it goes through setFoldMap. */
export function foldMap(): Record<string, boolean> { return ui.collapsed; }
export function setFoldMap(map: Record<string, boolean>): void { ui.collapsed = map; }

/** The two drag slots. While either is set a poll defers its sidebar rebuild (see menu.ts), because
 *  a rebuild mid-drag drops the row under the pointer. */
export function draggingRow(): string | null { return ui.dragging; }
export function setDraggingRow(name: string | null): void { ui.dragging = name; }
export function draggingGroupName(): string | null { return ui.draggingGroup; }
export function setDraggingGroupName(name: string | null): void { ui.draggingGroup = name; }

/** The group a sidebar header "+" targets for the next created MCP. */
export function addGroupTarget(): string | null { return ui.addGroup; }
export function setAddGroupTarget(group: string | null): void { ui.addGroup = group; }

/** The build stamp last seen from /api/info; maybeReloadPanel compares the new answer against it. */
export function knownPanelVersion(): string | null { return ui.panelVersion; }
export function setKnownPanelVersion(stamp: string | null): void { ui.panelVersion = stamp; }
