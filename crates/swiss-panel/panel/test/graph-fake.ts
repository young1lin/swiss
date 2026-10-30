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

/* A stand-in for the graph engine ui/graph.ts is handed (the vendored Cytoscape.js in the panel):
 * jsdom has no canvas, and what the tests ask is what graphView TOLD the engine - the elements,
 * the layout, the classes a selection puts on - and what it does when the engine reports a tap.
 * The model is the slice GraphEls and GraphCore type: ids, data, classes, the closed neighbourhood. */
import type { GraphCore, GraphEls, GraphEngine, GraphEngineLoader, GraphEngineOptions } from "../src/ui/index.js";

interface Model {
  data: Map<string, Record<string, unknown>>;
  classes: Map<string, Set<string>>;
  edges: Array<{ id: string; source: string; target: string }>;
  /** Node positions: where the elements said, or where `place` put them (a layout's result). */
  pos: Map<string, { x: number; y: number }>;
}

class Els implements GraphEls {
  private readonly m: Model;
  readonly ids: string[];
  constructor(m: Model, ids: string[]) {
    this.m = m;
    this.ids = ids;
  }
  get length(): number { return this.ids.length; }
  id(): string { return this.ids[0] ?? ""; }
  data(key: string): unknown {
    const d = this.m.data.get(this.ids[0] ?? "");
    return d ? d[key] : undefined;
  }
  addClass(c: string): GraphEls {
    for (const id of this.ids) for (const w of c.split(/\s+/).filter(Boolean)) this.m.classes.get(id)?.add(w);
    return this;
  }
  removeClass(c: string): GraphEls {
    for (const id of this.ids) for (const w of c.split(/\s+/).filter(Boolean)) this.m.classes.get(id)?.delete(w);
    return this;
  }
  hasClass(cls: string): boolean {
    return this.ids.some((id) => this.m.classes.get(id)?.has(cls));
  }
  closedNeighborhood(): GraphEls {
    const out = new Set(this.ids);
    for (const e of this.m.edges) {
      if (this.ids.includes(e.source) || this.ids.includes(e.target)) {
        out.add(e.id);
        out.add(e.source);
        out.add(e.target);
      }
    }
    return new Els(this.m, Array.from(out));
  }
  edges(): GraphEls {
    const edgeIds = new Set(this.m.edges.map((e) => e.id));
    return new Els(this.m, this.ids.filter((id) => edgeIds.has(id)));
  }
  not(other: GraphEls): GraphEls {
    const drop = new Set((other as Els).ids);
    return new Els(this.m, this.ids.filter((id) => !drop.has(id)));
  }
  boundingBox(): { w: number; h: number } {
    const ps = this.ids.map((id) => this.m.pos.get(id)).filter((p): p is { x: number; y: number } => !!p);
    if (!ps.length) return { w: 0, h: 0 };
    const xs = ps.map((p) => p.x);
    const ys = ps.map((p) => p.y);
    return { w: Math.max(...xs) - Math.min(...xs), h: Math.max(...ys) - Math.min(...ys) };
  }
  position(): { x: number; y: number } {
    return { ...(this.m.pos.get(this.ids[0] ?? "") || { x: 0, y: 0 }) };
  }
  positions(fn: (node: GraphEls) => { x: number; y: number }): GraphEls {
    for (const id of this.ids) if (this.m.pos.has(id)) this.m.pos.set(id, fn(new Els(this.m, [id])));
    return this;
  }
}

type Handler = (ev: { target: unknown }) => void;

export interface FakeCore extends GraphCore {
  opts: GraphEngineOptions;
  /** How many times the stylesheet was reapplied in place. */
  restyles: number;
  /** A node's position now. */
  at(id: string): { x: number; y: number } | undefined;
  destroyed: boolean;
  fits: number;
  zoomLevel: number;
  /** The classes the view has put on an element now. */
  cls(id: string): string[];
  /** Report an event as the engine would: on a node (its id), or on the background (null). */
  fire(event: string, id: string | null): void;
}

/** `place` stands in for the layout's result (node id and index to a position); `size` for the
 *  container the engine measures (800 x 600 unless given). */
export function fakeEngine(o: { place?: (id: string, i: number) => { x: number; y: number }; size?: { w: number; h: number } } = {}): { engine: GraphEngineLoader; cores: FakeCore[] } {
  const cores: FakeCore[] = [];
  const make: GraphEngine = (opts) => {
    const m: Model = { data: new Map(), classes: new Map(), edges: [], pos: new Map() };
    let n = 0;
    for (const e of opts.elements) {
      m.data.set(e.data.id, { ...e.data });
      m.classes.set(e.data.id, new Set((e.classes || "").split(/\s+/).filter(Boolean)));
      if (e.group === "edges") m.edges.push({ id: e.data.id, source: String(e.data.source), target: String(e.data.target) });
      else m.pos.set(e.data.id, o.place ? o.place(e.data.id, n++) : { ...(e.position || { x: 0, y: 0 }) });
    }
    const all = (): Els => new Els(m, Array.from(m.data.keys()));
    const handlers: Array<{ events: string; selector: string | null; fn: Handler }> = [];
    const core: FakeCore = {
      opts, destroyed: false, fits: 0, restyles: 0, zoomLevel: 1,
      on(events: string, a: string | Handler, b?: Handler): unknown {
        if (typeof a === "string" && b) handlers.push({ events, selector: a, fn: b });
        else if (typeof a === "function") handlers.push({ events, selector: null, fn: a });
        return core;
      },
      elements: () => all(),
      nodes: () => new Els(m, Array.from(m.pos.keys())),
      getElementById: (id: string) => new Els(m, m.data.has(id) ? [id] : []),
      layout: () => ({ run: () => core }),
      style: ((sheet?: unknown) => (sheet ? core : { update: () => { core.restyles++; return core; } })) as FakeCore["style"],
      batch: (fn: () => void) => { fn(); },
      fit: () => { core.fits++; return core; },
      zoom(o?: { level: number }): number {
        if (o) core.zoomLevel = o.level;
        return core.zoomLevel;
      },
      width: () => o.size ? o.size.w : 800,
      height: () => o.size ? o.size.h : 600,
      resize: () => core,
      destroy: () => { core.destroyed = true; },
      cls: (id: string) => Array.from(m.classes.get(id) || []).sort(),
      at: (id: string) => m.pos.get(id),
      fire(event: string, id: string | null): void {
        for (const hd of handlers) {
          if (!hd.events.split(/\s+/).includes(event)) continue;
          if (id === null) {
            if (hd.selector === null) hd.fn({ target: core });
            continue;
          }
          const isEdge = m.edges.some((e) => e.id === id);
          const wants = hd.selector === null || hd.selector.split(",").map((s) => s.trim()).includes(isEdge ? "edge" : "node");
          if (wants) hd.fn({ target: new Els(m, [id]) });
        }
      },
    } as FakeCore;
    cores.push(core);
    return core;
  };
  return { engine: () => Promise.resolve(make), cores };
}

/** Let the loader's promise settle and the view draw (it draws once its frame is attached). */
export function drawn(): Promise<void> {
  return new Promise((r) => { setTimeout(r, 0); });
}
