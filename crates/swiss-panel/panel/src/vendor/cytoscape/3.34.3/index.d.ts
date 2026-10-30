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

/* The type mirror of this vendored shim (SPEC §panel.toolchain): the .js beside it is the served
   truth (byte-identical upstream bundle + loader), this file only types the export the
   panel imports - loadCytoscape hands back the cytoscape factory. Only the slice of the
   Core and Collection API ui/graph.ts calls is typed here; a new call means a new line,
   checked against upstream's own index.d.ts of the same version. Upgrading the vendored
   package means upgrading this mirror in the same commit. */

export interface CyPosition { x: number; y: number }

export interface CyElementDef {
  group: "nodes" | "edges";
  data: { id: string; source?: string; target?: string; [key: string]: unknown };
  classes?: string;
  position?: CyPosition;
}

export interface CyStyleBlock {
  selector: string;
  style: Record<string, unknown>;
}

export interface CyLayoutOptions {
  name: string;
  [key: string]: unknown;
}

export interface CyLayout {
  run(): CyLayout;
  stop(): CyLayout;
}

/** One element, or a collection of them - the library's own model is the same object. */
export interface CyCollection {
  readonly length: number;
  id(): string;
  data(key: string): unknown;
  isNode(): boolean;
  isEdge(): boolean;
  forEach(fn: (el: CyCollection, i: number) => void): void;
  addClass(classes: string): CyCollection;
  removeClass(classes: string): CyCollection;
  hasClass(cls: string): boolean;
  closedNeighborhood(selector?: string): CyCollection;
  connectedEdges(selector?: string): CyCollection;
  edges(selector?: string): CyCollection;
  union(other: CyCollection): CyCollection;
  not(other: CyCollection | string): CyCollection;
  degree(includeLoops?: boolean): number;
  position(): CyPosition;
  renderedPosition(): CyPosition;
  empty(): boolean;
  nonempty(): boolean;
}

export interface CyEvent {
  type: string;
  target: CyCollection | CyCore;
}

export interface CyCore {
  on(events: string, selector: string, handler: (ev: CyEvent) => void): CyCore;
  on(events: string, handler: (ev: CyEvent) => void): CyCore;
  elements(selector?: string): CyCollection;
  nodes(selector?: string): CyCollection;
  edges(selector?: string): CyCollection;
  getElementById(id: string): CyCollection;
  layout(options: CyLayoutOptions): CyLayout;
  style(sheet: CyStyleBlock[]): unknown;
  batch(fn: () => void): void;
  fit(eles?: CyCollection, padding?: number): CyCore;
  center(eles?: CyCollection): CyCore;
  zoom(): number;
  zoom(options: { level: number; renderedPosition: CyPosition }): CyCore;
  width(): number;
  height(): number;
  resize(): CyCore;
  animate(options: { center?: { eles: CyCollection }; zoom?: number; duration?: number }): CyCore;
  destroy(): void;
  destroyed(): boolean;
}

export interface CyOptions {
  container: HTMLElement;
  elements: CyElementDef[];
  style: CyStyleBlock[];
  layout: CyLayoutOptions;
  minZoom?: number;
  maxZoom?: number;
  wheelSensitivity?: number;
  boxSelectionEnabled?: boolean;
  selectionType?: "single" | "additive";
  autounselectify?: boolean;
  autoungrabify?: boolean;
  userZoomingEnabled?: boolean;
  userPanningEnabled?: boolean;
  pixelRatio?: number | "auto";
}

export type CytoscapeFactory = (options: CyOptions) => CyCore;

export declare function loadCytoscape(): Promise<CytoscapeFactory>;
