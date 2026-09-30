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

import type { ApiTunnelsKeysResponse, ApiTunnelsResponse } from "./types/api.js";

/* The tunnels domain owns its state (SPEC §panel.toolchain, slice 4 of 7): which scope is mounted, the last
   /api/tunnels answer, the per-row verb in flight, the cached key list, the two drag slots and the
   group a header "+" has staged for the sheet it opens.

   A leaf module like ui-state, and for the same reason: the domain is spread over three files —
   tunnels.ts renders, polling.ts loads, tunnel-sheets.ts edits — and tunnels.ts already imports
   polling.ts, so hosting the record in either one closes a cycle. This file imports types only. */
const tun: {
  tab: "conns" | "rules";
  data: ApiTunnelsResponse | null;
  busy: Record<string, string>;
  keys: ApiTunnelsKeysResponse | null;
  dragging: string | null;
  draggingGroup: string | null;
  pendingGroup: string | null;
  collapsed: { conns: Record<string, boolean>; rules: Record<string, boolean> };
} = {
  tab: "conns",        // which tunnels page is mounted — the /api/groups family's own scope word
  data: null,          // the last /api/tunnels answer; null between unmount and the next load
  busy: {},            // id -> verb in flight
  keys: null,          // cached /api/tunnels/keys
  dragging: null,      // id of the ROW being dragged — polls must not rebuild under it
  draggingGroup: null, // name of the GROUP HEADER being dragged — same rebuild freeze
  pendingGroup: null,  // group chosen via a header "+", preselected in the sheet it opens
  collapsed: { conns: {}, rules: {} }, // the two scopes' fold maps (localStorage, groups.ts)
};

/** The mounted scope. mountTunnelsPage pins it; everything else reads it. */
export function mountedTunScope(): "conns" | "rules" { return tun.tab; }
export function setMountedTunScope(scope: string): void { tun.tab = scope === "rules" ? "rules" : "conns"; }

/** The last /api/tunnels answer, raw. polling.ts wraps this in the empty-shaped tunData(). */
export function tunResponse(): ApiTunnelsResponse | null { return tun.data; }
export function setTunResponse(answer: ApiTunnelsResponse | null): void { tun.data = answer; }

/** The cached key list. tunnel-sheets fetches it once per session and reads it back per sheet. */
export function tunKeys(): ApiTunnelsKeysResponse | null { return tun.keys; }
export function setTunKeys(keys: ApiTunnelsKeysResponse): void { tun.keys = keys; }

/** One row's verb in flight: present means an action is running and the row must not act again. */
export function tunBusyOf(id: string): string | undefined { return tun.busy[id]; }
export function setTunBusy(id: string, verb: string): void { tun.busy[id] = verb; }
export function clearTunBusy(id: string): void { delete tun.busy[id]; }

/** The two drag slots. While either is set a poll defers its rebuild — see patchTunnels. */
export function tunDragging(): string | null { return tun.dragging; }
export function setTunDragging(id: string | null): void { tun.dragging = id; }
export function tunDraggingGroup(): string | null { return tun.draggingGroup; }
export function setTunDraggingGroup(name: string | null): void { tun.draggingGroup = name; }

/** The group a header "+" staged for the sheet it is about to open. */
export function setTunPendingGroup(group: string | null): void { tun.pendingGroup = group; }
/** Read it and clear it in one move: the sheet consumes the staged group exactly once, and the
 *  select is the truth from there on. Both sheets did this as two statements before. */
export function takeTunPendingGroup(): string | null {
  const group = tun.pendingGroup;
  tun.pendingGroup = null;
  return group;
}

/** One scope's fold map, handed out by reference — the groups component writes keys into it. */
export function tunFolds(scope: "conns" | "rules"): Record<string, boolean> { return tun.collapsed[scope]; }
export function setTunFolds(conns: Record<string, boolean>, rules: Record<string, boolean>): void {
  tun.collapsed = { conns: conns, rules: rules };
}

/** Leaving a tunnels page: drop the answer, the key cache and any half-finished drag. The fold
 *  maps and the mounted scope survive — they are the page's settings, not its contents. */
export function clearTunView(): void {
  tun.data = null;
  tun.keys = null;
  tun.dragging = null;
}
