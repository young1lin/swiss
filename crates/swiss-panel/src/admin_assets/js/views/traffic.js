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

import { state } from "../util.js";
import { loadTraffic, trafficReload } from "../traffic.js";
import { mcpChipText } from "../polling.js";
export function mount() { return trafficReload(true); }
export function refresh() { state.trafficSig = null; return loadTraffic(); }
export function poll() { return loadTraffic(); }
export function countText() { return mcpChipText(); }
export function unmount() { state.traffic = []; state.trafficFull = {}; state.trafficOpen = {}; state.trafficSig = null; }
