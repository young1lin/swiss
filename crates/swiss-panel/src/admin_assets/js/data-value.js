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

import { $ } from "./util.js";
import { h } from "./h.js";
                                     
import { locale, tr } from "./i18n.js";
import { btn } from "./ui/button.js";
import { JV_LINES, decodeStrings, jsonCodeNode } from "./ui/json-view.js";
import { closeSheet, sheet, showSheet } from "./ui/sheet.js";

/* --- value viewer sheet (SPEC §data.grid) ----------------------------------------------------------- */
/* A cell is a 30px row; its content is not. "View value\u2026" opens the full value in a
   read-only sheet — the same surface the editor lives in, minus every write affordance
   (editing stays in the editor sheet; this one only looks). cloudbeaver registers value
   presentations by MIME (TextValuePresentationBootstrap); the equivalent here is one pure
   kind picker over the value itself, because the wire already says what the value is:
   - the \\x hex form (both adapters' binary ride, SPEC §data.edits) opens as a hex dump,
   - an http(s) text opens as itself plus the link,
   - parseable JSON opens as a details-folded tree,
   - everything else opens as full text.
   The column type deliberately does not sway the choice: a MySQL BLOB that is valid UTF-8
   rides the wire as text (mysql.rs), and its honest presentation IS the text. */

/** The hex cap: the first 512 bytes of a binary value, then the panel's standing
 *  truncation word. The WIRE stays uncapped on purpose — the whole hex string is the
 *  value's exact address (the keyless-table md5 digests the decoded bytes, W4b) — so the
 *  sheet caps its own view, never the value it was handed. */
const DB_VALUE_HEX_MAX = 512;

function dbValueJsonLike(s         )          {
  const t = String(s == null ? "" : s).trim();
  return t.startsWith("{") || t.startsWith("[");
}

/** Which of the four presentations a value opens as. Pure; the sheet is a thin wrapper. */
function dbValueKind(value         )                                  {
  if (typeof value === "string") {
    if (/^\\x[0-9a-fA-F]*$/.test(value)) return "hex";
    if (/^https?:\/\//i.test(value)) return "url";
    if (dbValueJsonLike(value)) {
      try { JSON.parse(value); return "json"; } catch (e) { /* not JSON — plain text */ }
    }
  } else if (value !== null && value !== undefined && typeof value === "object") {
    return "json"; // a JSON column the adapter already parsed
  }
  return "text";
}

/** The first maxBytes of the \\x wire form as a spaced hex dump, 16 bytes per line, with
 *  the real byte count (hex chars / 2, never the character count) and the truncated flag
 *  that reuses the redis value view's word ("\u00b7 truncated"). Pure. */
function dbHexPreview(value        , maxBytes        )                                                      {
  const hex = String(value).replace(/^\\x/, "");
  const bytes = Math.floor(hex.length / 2);
  const truncated = bytes > maxBytes;
  const show = truncated ? hex.slice(0, maxBytes * 2) : hex;
  const pairs = show.match(/.{2}/g) || [];
  const lines           = [];
  for (let i = 0; i < pairs.length; i += 16) lines.push(pairs.slice(i, i + 16).join(" "));
  return { text: lines.join("\n"), bytes: bytes, truncated: truncated };
}

/** A JSON value as the panel's code block (ui/json-view.ts) - the one every other page shows
 *  JSON in: standard JSON, two-space indent, coloured by token, a string that holds JSON shown
 *  as the JSON it holds. It replaced a folding tree (SPEC §panel.toolchain) whose sans-serif keys and click
 *  per level read unlike the rest of the panel (2026-09-28). Past JV_LINES the first lines are
 *  painted and Show all swaps in the rest: a large document builds its DOM on request. Every
 *  string lands as a TEXT node, so a document full of tags stays inert. */
function dbJsonValueNode(v         )              {
  const value = decodeStrings(v);
  const code = jsonCodeNode(value, false);
  const wrap = h("div", { class: "db-val-json" }, code.node);
  if (code.lines > JV_LINES) {
    const showAll = btn(tr("logs.showAllLines", { n: code.lines }));
    const more = h("div", { class: "db-val-more" }, showAll);
    showAll.onclick = ()       => {
      code.node.replaceWith(jsonCodeNode(value, true).node);
      more.remove();
    };
    wrap.appendChild(more);
  }
  return wrap;
}

/** Open the read-only viewer. One primary action (Close); Escape and the backdrop close
 *  too. `where` is the caption the caller knows (table for grid cells, "SQL result" for
 *  console results). */
function dbOpenValueSheet(column        , value         , where                           )       {
  const kind = dbValueKind(value);
  let body        ;
  if (kind === "json") {
    const parsed = typeof value === "object" ? value : JSON.parse(value          );
    body = dbJsonValueNode(parsed);
  } else if (kind === "hex") {
    const p = dbHexPreview(value          , DB_VALUE_HEX_MAX);
    body = h("div", null,
      h("div", { class: "db-val-meta" },
        p.truncated
          ? tr("dataValue.nBytesTruncated", { n: p.bytes.toLocaleString(locale()) })
          : tr("dataValue.nBytes", { n: p.bytes.toLocaleString(locale()) })),
      h("pre", { class: "db-val-pre" }, p.text));
  } else if (kind === "url") {
    const s = value          ;
    body = h("pre", { class: "db-val-pre" },
      h("a", { class: "db-link", href: s, target: "_blank", rel: "noopener noreferrer" }, s));
  } else {
    body = h("pre", { class: "db-val-pre" }, String(value));
  }
  // The library's sheet (SPEC §panel.pages): showSheet unhides the host before it paints, the sub
  // names where the value came from. Per-open button wiring stays, per the sheet idiom.
  showSheet(sheet({
    title: column,
    sub: where || undefined,
    label: tr("dataValue.viewValue"),
    body,
    foot: btn(tr("dataValue.close"), { kind: "primary", id: "dbValClose" }),
  }));
  const onKey = (e               )       => {
    if (e.key === "Escape") { closeValueSheet(); }
  };
  const closeValueSheet = ()       => {
    document.removeEventListener("keydown", onKey);
    closeSheet();
  };
  $("dbValClose").onclick = closeValueSheet;
  document.addEventListener("keydown", onKey);
  $("sheet").onclick = (e) => { if (e.target === $("sheet")) closeValueSheet(); };
}

export { DB_VALUE_HEX_MAX, dbHexPreview, dbJsonValueNode, dbOpenValueSheet, dbValueKind };
