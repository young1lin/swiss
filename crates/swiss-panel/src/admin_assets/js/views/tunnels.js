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

/* The SSH Connections page (#tunnels): one of the tunnels plugin's two L2 pages. A thin
   module by design — all data, rendering and actions live in ../tunnels.js and ../polling.js;
   this file only states which scope the page mounts, so the Context Bar's page switcher and
   the deep link #tunnels (kept for saved links) land on connections. */
import { mountTunnelsPage, pollTunnelsPage, refreshTunnelsPage, tunnelsCountText, unmountTunnelsPage } from "../tunnels.js";

export function mount() { return mountTunnelsPage("conns"); }
export function refresh() { return refreshTunnelsPage(); }
export function poll() { return pollTunnelsPage(); }
export function countText() { return tunnelsCountText("conns"); }
export function unmount() { unmountTunnelsPage(); }
