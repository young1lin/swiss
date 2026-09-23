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

/* A sprite reference as a NODE (docs/18 V2, docs/37 R5). The sprite is the <svg hidden> block
 * in index.html - Lucide-style, 24 viewBox, 1.5 stroke, currentColor - and .ic (base.css)
 * sizes it at 16px. SVG is its own namespace: createElement("svg") builds an
 * HTMLUnknownElement that renders nothing, so this goes through createElementNS and cannot be
 * folded into h(), which is typed for HTML tags. <use href> without the xlink alias is SVG 2.
 *
 * `label` makes the glyph speak (role=img + aria-label) - for an icon that stands in for a
 * word, like a launch-type mark. Without it the glyph is decoration and hidden from assistive
 * tech: the button or row around it carries the words. */
export function iconNode(name: string, label?: string): SVGSVGElement {
  const NS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("class", "ic");
  if (label) { svg.setAttribute("role", "img"); svg.setAttribute("aria-label", label); }
  else svg.setAttribute("aria-hidden", "true");
  const use = document.createElementNS(NS, "use");
  use.setAttribute("href", "#i-" + name);
  svg.appendChild(use);
  return svg;
}
