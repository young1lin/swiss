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
   served truth (byte-identical upstream module + a re-export), this file only types the slice
   ui/markdown.ts calls, checked against upstream's own dist/markdown-it.d.mts of the same version.
   A new call means a new line here. Upgrading the vendored package means upgrading this mirror in
   the same commit. */

export interface MarkdownItOptions {
  /** Pass a note's own HTML tags through. The panel never does. */
  html?: boolean;
  /** Turn a bare URL in the text into a link (linkify-it). */
  linkify?: boolean;
  /** A single newline in a paragraph is a <br>. */
  breaks?: boolean;
  typographer?: boolean;
}

export interface Token {
  /** A fence's text, verbatim, with its final newline. */
  content: string;
  /** A fence's info string: the language word and whatever follows it. */
  info: string;
}

export type RendererRule = (tokens: Token[], idx: number) => string;

export interface LinkifyItOptions {
  /** Link a bare domain with no scheme ("example.com"). */
  fuzzyLink?: boolean;
  /** Link a bare email address with no mailto:. */
  fuzzyEmail?: boolean;
  fuzzyIP?: boolean;
}

export interface MarkdownIt {
  linkify: { set(options: LinkifyItOptions): unknown };
  renderer: { rules: Record<string, RendererRule | undefined> };
  utils: { escapeHtml(str: string): string };
  /** Whether a link's or an autolink's target may be a link; replaceable on the instance. */
  validateLink(url: string): boolean;
  /** Switch rules off by name ("image", "html_inline" ...). */
  disable(list: string | string[], ignoreInvalid?: boolean): MarkdownIt;
  render(src: string): string;
}

/** Callable with or without new (upstream's MarkdownItCallable). */
declare const markdownit: (options?: MarkdownItOptions) => MarkdownIt;
export default markdownit;
