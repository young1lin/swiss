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

/* Small cross-module DOM shapes, ambient globals on purpose (docs/36 D6). These are
   panel-side conventions, not API shapes - menu items and empty states are built by one
   module and read by many, so the shape lives where both sides can see it. The built-in
   augmentations this file once carried (Function, EventTarget, RegExp) are retired by
   docs/37 M3: handlers read e.currentTarget, guards narrow with instanceof, and the
   module-scoped singletons live in their modules now. */

/** One popupMenu row - menu.ts:25. The union is load-bearing: a separator is { sep: true }
 *  with NO label/fn (tunnels.ts:256 was the strict-mode error that proved it), an action
 *  is label+fn with optional styling flags. pick/on drive the checked-mark row styles
 *  (danger reds the item); menu.ts never reads a field the arm does not carry. */
import type { ApiMcpRow } from "./api.js";
export interface MenuItemAction {
  label: string;
  fn: (ev?: MouseEvent) => void;
  danger?: boolean;
  pick?: boolean;
  on?: boolean;
  title?: string;
  /* docs/43 M1: a row can carry the type glyph and the dirty dot its card does — the tab
   *  strip's overflow lists open objects, and the menu is the whole set's one read. */
  icon?: string;
  dot?: boolean;
  /* The drawer follow-up: a row can carry a MARK (the dialect word, painted by
   * typeTagNode - glyph for whitelisted dialects, mono word otherwise) and a META (a dim
   * trailing value like a table count). popupMenu ignores both; the sidebar drawers read
   * them, because a drawer row has room a one-line menu label does not. */
  mark?: string;
  meta?: string;
  /* docs/43 M3: a row the menu shows but refuses to run — the database selector lists
   *  every database the instance names, browsable or not, so the reason (title) is one
   *  hover away instead of the row simply being missing. */
  disabled?: boolean;
  /* fix-plan #14: a trailing glyph for rows that open ANOTHER menu (the "Table" row that
   *  used to spell its caret in the label) - the leading icon field is the row's TYPE
   *  glyph; this one is a direction, painted at the end of the row like the caret it
   *  replaces. */
  affordance?: string;
  /* docs/43 M3: a non-interactive heading row (a connection GROUP name, "system" bands) —
   *  styled like the menu's own chrome, never focused, never clicked. */
  heading?: boolean;
  sep?: never;
}

export interface MenuItemSep {
  sep: true;
}

export type MenuItem = MenuItemSep | MenuItemAction;

/** A switcher-menu item before its click handler is attached (pageMenuItems' output): the
 *  caller binds fn per item (page-registry) before popupMenu takes over. */
export interface PageMenuItemSpec {
  label: string;
  title?: string;
  pick?: boolean;
  on?: boolean;
  fn?: (ev?: MouseEvent) => void;
}

/** The module contract every page entry exports (docs/13): mount owns the pane while
 *  mounted; the optional hooks are called by the shell exactly when present. */
export interface PageModule {
  mount?: (ctx: { signal: AbortSignal }) => void | Promise<void>;
  unmount?: () => void | Promise<void>;
  poll?: () => void | Promise<void>;
  refresh?: () => void | Promise<void>;
  canLeave?: () => boolean;
  countText?: () => string;
  hasPendingChanges?: () => boolean;
}

/** One empty state - util.ts emptyHtml opts (docs/18 V7): every view's nothing-here is
 *  this shape. action, when present, renders the ghost button and names the data-empty-action
 *  the owning view wires. */
/** One page descriptor as the registry stores it after valid(): the server rows plus the
 *  client-side pages, order normalised to a finite number. pluginId/path/sidebar/layout ride
 *  when the contributing side sent them (ApiPluginPage's shape, host/descriptor.rs
 *  PageDescriptor::to_json); a client-side page may leave them out. */
export interface PageDescriptor {
  id: string;
  label: string;
  entry: string;
  order: number;
  pluginId?: string;
  path?: string;
  sidebar?: boolean;
  layout?: string;
}

/** What replace() accepts - the unvalidated wire/input form valid() checks and throws on.
 *  Fixed optional fields only: an index signature would reject the ApiPluginPage interface
 *  (interfaces carry no implicit index signatures), and the wire rows all land here. */
export interface PageInput {
  id?: string;
  label?: string;
  entry?: string;
  order?: number;
  pluginId?: string;
  path?: string;
  sidebar?: boolean;
  layout?: string;
}

/** One sidebar page group - page-registry's groupPages output (grouped by plugin). */
export interface PageGroup {
  id: string;
  label: string;
  order: number;
  pages: PageDescriptor[];
}


/** A grouped list's slice (group-logic slice): every group in stored order, each holding its
 *  members in the caller's flat order. Empty groups keep their slot - a group you just made
 *  has to stay visible. */
export interface GroupSlice<Row> {
  name: string;
  rows: Row[];
}

/** A row carrying the one field grouping reads: the stored group name (absent when unset). */
export interface GroupedRow {
  group?: string | null;
}

/** The phantom row a pane renders when the selected MCP has no live registry row yet: the
 *  fields every renderer reads, absent the ones only a real row carries. */
export type PhantomMcpRow = Partial<ApiMcpRow> & { name: string; state: string; type: string; source: string; lifecycle: string };

/** The one grouped-list component's configuration (groups.ts mountGroup, docs/20 section 4):
 *  everything a scope owns - row markup, ids, its own moves - while the component owns the
 *  band, the folds, the drags and the /api/groups/{scope} family. Row-generic: mcps, conns,
 *  rules, jobs, secrets, tokens and targets all pass their own row type through it. */
/* drag/dragGroup/rowId/onMoveRow/onAssign are optional: scopes without the row-drag
 *  contract (tokens: creation time is the order) omit them and gate everything behind
 *  draggable: false, so the wiring that would read them never runs. */
export interface GroupCfg<Row> {
  scope: string;
  density: "side" | "page";
  names: string[];
  collapsed: Record<string, boolean>;
  noun: string;
  addTitle?: (group: string) => string;
  onAdd?: (group: string) => void;
  /* docs/43 M2 (the Data tree): optional gates and overrides for scopes whose bands are
   *  DERIVED from data rather than named by the operator. label renames a band for display
   *  without touching the collapse key; canAdd gates the header + per band; moreItems
   *  replaces the stock Move/Rename/Delete list (null = this band gets no ellipsis at all,
   *  moreTitle its button's title); emptyText replaces the drop-target line a read-only
   *  tree cannot honor; countOf answers what the band's count badge numbers (a nested band
   *  counts its ROWS, not its inner bands). */
  label?: (group: string) => string;
  canAdd?: (group: string) => boolean;
  moreItems?: (group: string) => MenuItem[] | null;
  moreTitle?: (group: string) => string;
  emptyText?: (group: string) => string;
  countOf?: (slice: { name: string; rows: unknown[] }) => number;
  /* docs/43 addendum (the 200-row wall): optional render cap for bands whose rows run into
   *  the thousands. Given the band's FULL row list, answer how many to paint and the note
   *  row to append under them (null = paint everything, no note). The band's count badge
   *  still numbers the full list - the cap is a DOM budget, not a redefinition of what the
   *  band holds. */
  capRows?: (rows: Row[]) => { keep: number; note: HTMLElement | null };
  reload: () => void | Promise<void>;
  render?: () => void;
  afterDrag?: () => void;
  drag?: { get(): string | null | undefined; set(value: string | null): void };
  dragGroup?: { get(): string | null | undefined; set(value: string | null): void };
  /* The row builder every scope supplies (docs/37 R5): the component wires click + drag on
   * the node it hands back. The pre-R5 rowsHtml + rowSel string path retired with the last
   * unconverted scope. */
  rowNode?: (row: Row) => HTMLElement;
  wireRow?: (node: HTMLElement, row: Row) => void;
  rowId?: (row: Row) => string;
  rowsById: () => Row[];
  groupOfRow: (row: Row) => string;
  onMoveRow?: (id: string, targetId: string, before: boolean) => unknown;
  onAssign?: (id: string, group: string | null) => unknown;
  draggable?: boolean;
  /* optional: only the sidebar's filtered scope sets it; absent means the full list */
  filtered?: boolean;
}

/* A JSON-tree node (logs.ts buildJsonTree/jtNode): an arbitrary JSON object whose every
 *  value is more tree material - arrays arrive the same way, narrowed by Array.isArray at
 *  the branch. Record<string, unknown> is the honest type of "any JSON object": the tree
 *  indexes val[k] over Object.keys, it never names a key. */
export type JtBox = Record<string, unknown>;

/** One form field's schema (fields.ts TYPE_FIELDS rows): k is the def key, bool/num/area/
 *  kv/json pick the input kind, half pairs it into two columns, def is the checkbox default. */
export interface FieldSpec {
  k: string;
  label: string;
  ph?: string;
  hint?: string;
  bool?: boolean;
  num?: boolean;
  area?: boolean;
  kv?: boolean;
  json?: boolean;
  half?: boolean;
  def?: boolean;
}

/** A pg url decomposed into the form's parts (fields.ts parsePgUrl; docs/30). */
export interface PgUrlParts {
  host: string;
  port: string;
  user: string;
  password: string;
  database: string;
  params: string;
}

/** A registry group decorated with plugin availability (page-registry's decoratedGroups
 *  output, the palette's rows): off = every page's plugin is currently unavailable. */
export interface PaletteGroup {
  id: string;
  label: string;
  pages: PageDescriptor[];
  off?: boolean;
  offDetail?: string;
}

/** One palette section (plugin-palette paletteRows): Pinned / All plugins, empties dropped. */
export interface PaletteSection {
  section: string;
  groups: PaletteGroup[];
}

export interface EmptyStateSpec {
  icon: string;
  title: string;
  hint?: string;
  action?: string;
}
