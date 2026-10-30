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

/* Markdown drawn by mature libraries, never a parser written here (SPEC "mature libraries",
 * §panel.vendor): markdown-it parses a note, a hub or a brief (CommonMark, GFM tables and
 * strikethrough, a URL written with its scheme linked), and a ```mermaid fence becomes its
 * diagram, drawn by mermaid once its bundle is in. A note's own HTML never reaches the panel:
 * markdown-it runs in its safe mode - html off, so a tag in a note is escaped text - and a link
 * opens only http, https or mailto (any other link stays the text it was written as). The HTML is
 * markdown-it's own, read into DOM by DOMParser (an inert document: nothing in it runs or loads)
 * and moved over - no innerHTML write (SPEC §panel.lint R5). A single line break inside a
 * paragraph is kept (a note is typed in a textarea, where Enter means a new line). Images are not
 * drawn: a note never pulls a remote picture into the panel. With `copy` (an assistant's column)
 * each code block carries its own Copy at its top end. */
import { h } from "../h.js";
import { tr } from "../i18n.js";
import { copyBtn } from "./copy.js";
import markdownit from "../vendor/markdown-it/15.0.2/index.js";
import { loadMermaid,              } from "../vendor/mermaid/12.1.0/index.js";

const SAFE_HREF = /^(?:https?:|mailto:)/i;

const md = markdownit({ html: false, linkify: true, breaks: true });
// "pipeline.py" and "a@b.c" in a note are a file and a placeholder, not a site and an inbox:
// only a URL written with its scheme becomes a link.
md.linkify.set({ fuzzyLink: false, fuzzyEmail: false });
md.disable("image");
md.validateLink = (url) => SAFE_HREF.test(url.trim());

/** A code block keeps its text verbatim (no final newline) and its language on the <pre>, where
 *  the stylesheet and the diagram pass below read it. */
function codeBlock(text        , lang        )         {
  const attr = lang ? ` data-lang="${md.utils.escapeHtml(lang)}"` : "";
  return `<pre class="md-code"${attr}><code>${md.utils.escapeHtml(text.replace(/\n$/, ""))}</code></pre>\n`;
}
md.renderer.rules.fence = (tokens, idx) => codeBlock(tokens[idx].content, tokens[idx].info.trim().split(/\s+/)[0]);
md.renderer.rules.code_block = (tokens, idx) => codeBlock(tokens[idx].content, "");
// A wide table scrolls inside its own box instead of widening the page.
md.renderer.rules.table_open = () => '<div class="md-table"><table>\n';
md.renderer.rules.table_close = () => "</table></div>\n";

/** Markup a library wrote, as the nodes of an inert document. */
function parsed(html        )              {
  return new DOMParser().parseFromString(html, "text/html").body;
}

export function markdownNode(src        , o                                     = {})              {
  const box = h("div", { class: "md" + (o.class ? " " + o.class : "") });
  box.append(...Array.from(parsed(md.render(src)).childNodes));
  for (const a of Array.from(box.querySelectorAll("a"))) {
    a.target = "_blank";
    a.rel = "noopener noreferrer";
  }
  if (o.copy) {
    // A code block's Copy takes its text verbatim; a diagram's fence becomes its drawing, no Copy.
    for (const pre of Array.from(box.querySelectorAll             ("pre.md-code:not([data-lang=mermaid])"))) {
      const wrap = h("div", { class: "md-codebox" });
      pre.replaceWith(wrap);
      wrap.append(pre, copyBtn(() => pre.textContent || "", { label: tr("ui.copyCode") }));
    }
  }
  const fences = Array.from(box.querySelectorAll             ("pre[data-lang=mermaid]"));
  if (fences.length) void drawDiagrams(fences);
  return box;
}

/* --- diagrams ---------------------------------------------------------------------------------- */

/* mermaid at its safest, upstream's advice for text it did not write (securityLevel "sandbox"):
   the diagram is drawn inside a sandboxed iframe on a data: URL, so nothing a diagram carries runs
   in the panel's origin, where the session and the secrets are. mermaid hands back that one
   iframe; anything else is refused. Until the bundle is in, and when a diagram does not parse, the
   fence stays as code - the reader still has the source, and its title says why. */
const sources = new WeakMap                     ();
let seq = 0;
let themeWatch                          = null;

function configure(mermaid         )       {
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: "sandbox",
    suppressErrorRendering: true,
    theme: document.documentElement.getAttribute("data-theme") === "dark" ? "dark" : "default",
    fontFamily: getComputedStyle(document.body).fontFamily,
  });
}

async function figure(mermaid         , src        )                       {
  const { svg } = await mermaid.render("md-mermaid-" + ++seq, src);
  const body = parsed(svg);
  const frame = body.firstElementChild;
  const sandbox = frame ? frame.getAttribute("sandbox") : null;
  if (body.childElementCount !== 1 || !frame || frame.localName !== "iframe" || sandbox === null || /allow-(scripts|same-origin)/.test(sandbox)) {
    throw new Error("mermaid drew outside its sandbox");
  }
  const fig = h("div", { class: "md-mermaid" }, frame);
  sources.set(fig, src);
  return fig;
}

function why(e         )         {
  return e instanceof Error ? e.message : String(e);
}

async function drawDiagrams(fences               )                {
  let mermaid         ;
  try {
    mermaid = await loadMermaid();
  } catch (e) {
    for (const pre of fences) pre.title = why(e);
    return;
  }
  configure(mermaid);
  watchTheme(mermaid);
  for (const pre of fences) {
    try {
      pre.replaceWith(await figure(mermaid, pre.textContent || ""));
    } catch (e) {
      pre.title = why(e);
    }
  }
}

/** A theme switch redraws every diagram on screen in the new theme, as ui/graph.ts restyles its graph. */
function watchTheme(mermaid         )       {
  if (themeWatch || typeof MutationObserver !== "function") return;
  themeWatch = new MutationObserver(() => {
    configure(mermaid);
    for (const fig of Array.from(document.querySelectorAll             (".md-mermaid"))) {
      const src = sources.get(fig);
      if (src !== undefined) figure(mermaid, src).then((again) => fig.replaceWith(again), () => undefined);
    }
  });
  themeWatch.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
}
