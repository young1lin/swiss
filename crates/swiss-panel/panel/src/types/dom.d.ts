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

/* Small cross-module DOM shapes, ambient globals on purpose (docs/36 D6/D9). These are
   panel-side conventions, not API shapes - menu items and empty states are built by one
   module and read by many, so the shape lives where both sides can see it. */

/** One popupMenu row - menu.ts:25. The union is load-bearing: a separator is { sep: true }
 *  with NO label/fn (tunnels.ts:256 was the strict-mode error that proved it), an action
 *  is label+fn with optional styling flags. pick/on drive the checked-mark row styles
 *  (danger reds the item); menu.ts never reads a field the arm does not carry. */
/* The grid's column headers: oncontextmenu redeclared without lib.dom's this-param so the
   handler's this is the th itself (popupMenu anchors on this.getBoundingClientRect). */
interface MenuTh extends HTMLElement {
  oncontextmenu: ((ev: MouseEvent) => unknown) | null;
}

interface MenuItemAction {
  label: string;
  fn: (ev?: MouseEvent) => void;
  danger?: boolean;
  pick?: boolean;
  on?: boolean;
  title?: string;
  sep?: never;
}

interface MenuItemSep {
  sep: true;
}

type MenuItem = MenuItemSep | MenuItemAction;

/** A switcher-menu item before its click handler is attached (pageMenuItems' output): the
 *  caller binds fn per item (page-registry) before popupMenu takes over. */
interface PageMenuItemSpec {
  label: string;
  title?: string;
  pick?: boolean;
  on?: boolean;
  fn?: (ev?: MouseEvent) => void;
}

/** The module contract every page entry exports (docs/13): mount owns the pane while
 *  mounted; the optional hooks are called by the shell exactly when present. */
interface PageModule {
  mount?: (ctx: { signal: AbortSignal }) => void | Promise<void>;
  unmount?: () => void | Promise<void>;
  poll?: () => void | Promise<void>;
  refresh?: () => void | Promise<void>;
  canLeave?: () => boolean;
  countText?: () => string;
  hasPendingChanges?: () => boolean;
  [key: string]: unknown;
}

/** One empty state - util.ts emptyHtml opts (docs/18 V7): every view's nothing-here is
 *  this shape. action, when present, renders the ghost button and names the data-empty-action
 *  the owning view wires. */
/** util.ts's toast keeps its auto-hide timer on ITSELF (toast._t) - the Node-era idiom
 *  for a module-scoped singleton timer. Describing it as a property of Function is the one
 *  zero-token way: a module-side interface declaration would emit a stray semicolon through
 *  ts-blank-space, and docs/36 D9 holds the emitted bytes to whitespace-identical. _t is
 *  unusual enough that this augmentation describes exactly one thing in the tree. */
interface Function {
  _t?: ReturnType<typeof setTimeout>;
  /* main.ts stamps one-shot guards on two module functions; the type is Function so the
   *  property is visible without changing the call sites (the toast._t pattern above). */
  _warned?: boolean;
}

/** One page descriptor as the registry stores it after valid(): the server rows plus the
 *  client-side pages, order normalised to a finite number. pluginId/path/sidebar/layout ride
 *  when the contributing side sent them (ApiPluginPage's shape); the index signature keeps
 *  the passthrough honest instead of enumerating the world. */
interface PageDescriptor {
  id: string;
  label: string;
  entry: string;
  order: number;
  pluginId?: string;
  path?: string;
  sidebar?: boolean;
  layout?: string;
  [key: string]: unknown;
}

/** What replace() accepts - the unvalidated wire/input form valid() checks and throws on.
 *  Fixed optional fields only: an index signature would reject the ApiPluginPage interface
 *  (interfaces carry no implicit index signatures), and the wire rows all land here. */
interface PageInput {
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
interface PageGroup {
  id: string;
  label: string;
  order: number;
  pages: PageDescriptor[];
}


/** A grouped list's slice (group-logic slice): every group in stored order, each holding its
 *  members in the caller's flat order. Empty groups keep their slot - a group you just made
 *  has to stay visible. */
interface GroupSlice<Row> {
  name: string;
  rows: Row[];
}

/** A row carrying the one field grouping reads: the stored group name (absent when unset). */
interface GroupedRow {
  group?: string | null;
}

/** The phantom row a pane renders when the selected MCP has no live registry row yet: the
 *  fields every renderer reads, absent the ones only a real row carries. */
type PhantomMcpRow = Partial<ApiMcpRow> & { name: string; state: string; type: string; source: string; lifecycle: string };

/** The one grouped-list component's configuration (groups.ts mountGroup, docs/20 section 4):
 *  everything a scope owns - row markup, ids, its own moves - while the component owns the
 *  band, the folds, the drags and the /api/groups/{scope} family. Row-generic: mcps, conns,
 *  rules, jobs, secrets, tokens and targets all pass their own row type through it. */
interface GroupCfg<Row extends GroupedRow> {
  scope: string;
  density: "side" | "page";
  names: string[];
  collapsed: Record<string, boolean>;
  noun: string;
  addTitle?: (group: string) => string;
  onAdd: (group: string) => void;
  reload: () => void | Promise<void>;
  render?: () => void;
  afterDrag?: () => void;
  drag: { get(): string | null | undefined; set(value: string | null): void };
  dragGroup: { get(): string | null | undefined; set(value: string | null): void };
  rowNode?: (row: Row) => HTMLElement;
  rowsHtml?: (group: GroupSlice<Row>) => string;
  wireRow?: (node: HTMLElement, row: Row) => void;
  rowId: (row: Row) => string;
  rowSel?: (row: Row) => string;
  rowsById: () => Row[];
  groupOfRow: (row: Row) => string;
  onMoveRow: (id: string, targetId: string, before: boolean) => unknown;
  onAssign: (id: string, group: string) => unknown;
  draggable?: boolean;
  filtered: boolean;
}

/* connect.ts's document-click guard probes e.target.closest; lib.dom types a click target
 *  as bare EventTarget, which declares no closest, and D9 forbids the parenthesised cast
 *  an in-place fix would need. Optional so concrete elements keep their required method. */
interface EventTarget {
  closest?: (selector: string) => Element | null;
  /* The DDL sheet's Enter guard reads tagName/type off the (untyped) key event's target.
   *  tagName stays string-optional (Element narrows it back to required); type must be
   *  unknown because SVG elements declare their own required `type` members, which an
   *  optional string here would clash with (interface merges need assignable members). */
  tagName?: string;
  type?: unknown;
}


/* A JSON-tree node (logs.ts buildJsonTree/jtNode): an object whose every value is more
 *  tree material - arrays arrive the same way, narrowed by Array.isArray at the branch.
 *  The index signature is what lets the tree index val[k] with no parenthesised cast. */
interface JtBox {
  [key: string]: unknown;
}

/* detail.ts openDetail builds the detail object as one literal and then stamps the three
 *  kind pages onto it (KINDS.forEach); this is the literal's shape - everything but the
 *  kind keys, plus the index the forEach writes through. Cast back to McpDetail at the
 *  state assignment, once all three are in. */
type FreshDetail = Omit<McpDetail, "tools" | "resources" | "prompts"> & { [key: string]: unknown };

/* The search box's oninput reads this.value in place (main.ts). The DOM declares oninput
 *  on GlobalEventHandlers with a this-param that has no value, and every in-place fix is a
 *  token change docs/36 D9 forbids; this override drops the this-param so the handler's
 *  this is loosely typed (see noImplicitThis in tsconfig) and the read stands as written. */
interface FilterInput extends HTMLInputElement {
  oninput: ((ev: Event) => unknown) | null;
  /* shiftKey: the grid's select-boxes read it from the change event (Shift-click ranges);
     optional so the redeclaration stays assignable to HTMLElement's own handler type. */
  onchange: ((ev: Event & { shiftKey?: boolean }) => unknown) | null;
  onkeydown: ((ev: KeyboardEvent) => unknown) | null;
}

/* The console's textarea: handlers read this.value / this.scrollTop inside oninput, onscroll
   and onkeydown - the same redeclare-without-this-param trick as FilterInput. */
interface FilterTextArea extends HTMLTextAreaElement {
  oninput: ((ev: Event) => unknown) | null;
  onscroll: ((ev: Event) => unknown) | null;
  onkeydown: ((ev: KeyboardEvent) => unknown) | null;
}

/* A button whose click handler mutates the button itself (this.disabled / this.textContent):
   redeclaring onclick without the GlobalEventHandlers this-param restores this-typing. */
interface ActionButton extends HTMLButtonElement {
  onclick: ((ev: PointerEvent) => any) | null;
}

/* The same this-value read on selects (data-filters.ts column/operator pickers). */
interface FilterSelect extends HTMLSelectElement {
  onchange: ((ev: Event) => unknown) | null;
}

/* main.ts's typing guard tests `document.activeElement && document.activeElement.tagName`,
 *  a string | null; the runtime coerces null to "null" and the pattern still answers no,
 *  so the panel-side overload accepts what the expression actually is. */
interface RegExp {
  test(s: string | null): boolean;
}

/** One form field's schema (fields.ts TYPE_FIELDS rows): k is the def key, bool/num/area/
 *  kv/json pick the input kind, half pairs it into two columns, def is the checkbox default. */
interface FieldSpec {
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
interface PgUrlParts {
  host: string;
  port: string;
  user: string;
  password: string;
  database: string;
  params: string;
}

/** A registry group decorated with plugin availability (page-registry's decoratedGroups
 *  output, the palette's rows): off = every page's plugin is currently unavailable. */
interface PaletteGroup {
  id: string;
  label: string;
  pages: PageDescriptor[];
  off?: boolean;
  offDetail?: string;
}

/** One palette section (plugin-palette paletteRows): Pinned / All plugins, empties dropped. */
interface PaletteSection {
  section: string;
  groups: PaletteGroup[];
}

interface EmptyStateSpec {
  icon: string;
  title: string;
  hint?: string;
  action?: string;
}
