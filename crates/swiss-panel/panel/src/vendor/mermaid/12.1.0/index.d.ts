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

/* The type mirror of this vendored shim (SPEC §panel.toolchain): the .js beside it in js/vendor is the
   served truth (byte-identical upstream bundle + loader), this file only types the export the
   panel imports - loadMermaid hands back the mermaid API. Only the slice ui/markdown.ts calls is
   typed here, checked against upstream's own dist/mermaid.d.ts and config.type.d.ts of the same
   version; a new call means a new line. Upgrading the vendored package means upgrading this
   mirror in the same commit. */

export interface MermaidConfig {
  /** Leave the page's .mermaid elements alone; the panel calls render itself. */
  startOnLoad?: boolean;
  /** sandbox: the diagram is drawn inside a sandboxed iframe (a data: URL, no scripts), so
   *  nothing in it runs in the page's origin. */
  securityLevel?: "strict" | "loose" | "antiscript" | "sandbox";
  theme?: "default" | "base" | "dark" | "forest" | "neutral";
  /** A diagram that fails to parse throws, rather than drawing mermaid's error picture. */
  suppressErrorRendering?: boolean;
  fontFamily?: string;
}

export interface RenderResult {
  /** The diagram as markup: SVG, or under securityLevel sandbox one <iframe> carrying it. */
  svg: string;
}

export interface Mermaid {
  initialize(config: MermaidConfig): void;
  /** Draw one diagram; `id` names the SVG and must be unique in the document. Rejects on a
   *  diagram that does not parse. */
  render(id: string, text: string): Promise<RenderResult>;
}

/** Inject the bundle once (shared classic-script loader) and hand back window.mermaid. */
export declare function loadMermaid(): Promise<Mermaid>;
