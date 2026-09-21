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

/* The Port Forwards page (#tunnel-forwards): the tunnels plugin's second L2 page, sibling
   of #tunnels. Same shape as views/tunnels.js on purpose — a page descriptor's entry module
   states its scope and nothing else; the shared module owns the rendering, so the two pages
   can never drift apart. */
import { mountTunnelsPage, pollTunnelsPage, refreshTunnelsPage, tunnelsCountText, unmountTunnelsPage } from "../tunnels.js";

export function mount() { return mountTunnelsPage("rules"); }
export function refresh() { return refreshTunnelsPage(); }
export function poll() { return pollTunnelsPage(); }
export function countText() { return tunnelsCountText("rules"); }
export function unmount() { unmountTunnelsPage(); }
