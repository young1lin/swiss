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

/* The graph engine the panel hands ui/graph.ts (SPEC §panel.ui, U2: the library imports nothing
   outside itself, so the engine arrives as an argument). The vendored Cytoscape loader, typed
   once as the slice ui/graph.ts calls: the vendor mirror types upstream's API whole, the
   library types the calls it makes - the same calls, but TypeScript cannot line the two
   sets of overloads up across modules, so this is the one place the two meet. */
                                                                    
import { loadCytoscape } from "./vendor/cytoscape/3.34.3/index.js";

export const graphEngine                    = () => loadCytoscape().then((lib) => lib                          );
