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
type MenuItem = { sep: true } | {
  label: string;
  fn: (ev?: MouseEvent) => void;
  danger?: boolean;
  pick?: boolean;
  on?: boolean;
  sep?: never;
};

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
}

interface EmptyStateSpec {
  icon: string;
  title: string;
  hint?: string;
  action?: string;
}
