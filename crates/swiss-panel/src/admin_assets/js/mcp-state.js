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

                                                                                
                                                              

/* The MCP domain owns its state (docs/37 R4, slice 7 of 7 — the last one).

   These eight fields are what was left of PanelState, and with this module util.ts stops
   holding state at all: the `state` literal and the PanelState type are both gone, and the
   ratchet's no-restricted-imports row can finally say so.

   McpDetail stays in types/state.ts rather than moving here, unlike DbState in slice 6. It
   has eight consumers and one of them is types/dom.ts — a types file importing a shape out
   of a value module would be the dependency pointing the wrong way for the sake of symmetry.

   A leaf module, same reason as the other slices: sidebar.ts, detail.ts and polling.ts all
   read this and all import each other, so hosting the record in any of them closes a cycle.
   This file imports types only. */
const mcp   
                    
                   
                          
                           
                               
                                         
                            
                               
  = {
  rows: [],           // rows from /api/mcps (each carries .group)
  groups: [],         // group names in sidebar order — the FIRST entry is the sink for unassigned rows
  selected: null,     // selected MCP name
  detail: null,       // the open detail pane, or null: nothing selected — see mcpDetail()
  busy: {},           // name -> the verb in flight ("start", "save", …)
  lastAction: {},     // name -> what the last action did, for the pane's one-line result
  mem: null,          // last /api/memory answer
  info: null,         // /api/info: the token env var's name, the panel build stamp
};

/** The MCP list, by reference: the sidebar's drag reorders it in place (splice/swap) before
 *  persisting, and a copy would reorder nothing the renderer can see. A poll replaces it
 *  wholesale through setMcpRows. */
export function mcpRows()              { return mcp.rows; }
export function setMcpRows(rows             )       { mcp.rows = rows; }

/** Group names in sidebar order. Also by reference — group drag reorders in place. */
export function mcpGroups()           { return mcp.groups; }
export function setMcpGroups(names          )       { mcp.groups = names; }

/** The selected row's name, or null when the sidebar has no selection. */
export function selectedMcp()                { return mcp.selected; }
export function setSelectedMcp(name               )       { mcp.selected = name; }

/** The open detail pane. Null is REAL here, unlike the Data view's record (slice 6): nothing
 *  selected is a state the pane renders, as the shared empty state with its own add action
 *  (docs/18 V7). Every reader keeps its guard, and that guard is now the only thing that has
 *  to be true — no assertion follows it. */
export function mcpDetail()                   { return mcp.detail; }
export function setMcpDetail(detail                  )       { mcp.detail = detail; }

/** The verb in flight on one row, or "" — the row menu shows it and refuses a second one. */
export function mcpBusyVerb(name        )         { return mcp.busy[name] || ""; }
export function setMcpBusy(name        , verb        )       { mcp.busy[name] = verb; }
export function clearMcpBusy(name        )       { delete mcp.busy[name]; }

/** What the last action on one row did: the pane's one-line result under the header. */
export function lastActionOf(name        )                         { return mcp.lastAction[name]; }
export function setLastAction(name        , action            )       { mcp.lastAction[name] = action; }

/** The gateway's memory readout, as the 6s poll last saw it. */
export function memoryInfo()                       { return mcp.mem; }
export function setMemoryInfo(info                      )       { mcp.mem = info; }

/** /api/info: the token env var's name (the connect snippets read it) and the panel build
 *  stamp (main.ts compares it against the loaded one to offer a reload). */
export function gatewayInfo()                         { return mcp.info; }
export function setGatewayInfo(info                        )       { mcp.info = info; }

/** Back to boot. There is no production caller: the MCP list is the root view and never
 *  unmounts, so unlike the tun/jobs/db slices this domain has no "leaving the page" reset.
 *  It exists because the record is module-level and therefore shared by every test in a
 *  file — the suite used to reopen the whole bag field by field to get the same effect,
 *  which is one forgotten field away from a test that passes on its neighbour's leftovers.
 *  gatewayInfo and the memory readout are deliberately kept: they describe the gateway, not
 *  any one list, and a reset that dropped them would make every case re-stub /api/info. */
export function resetMcpState()       {
  mcp.rows = [];
  mcp.groups = [];
  mcp.selected = null;
  mcp.detail = null;
  mcp.busy = {};
  mcp.lastAction = {};
}
