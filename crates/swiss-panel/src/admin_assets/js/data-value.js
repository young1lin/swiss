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

import { $, api } from "./util.js";
import { h } from "./h.js";
                                     
import { locale, moduleDbMessage, tr, tk } from "./i18n.js";
                                                   
import { btn } from "./ui/button.js";
import { formActions, hint } from "./ui/form.js";
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
   rides the wire as text (mysql.rs), and its honest presentation IS the text.
   One exception reads further than the wire: a binary cell of a module's own SQLite file
   (SPEC §data.sqlite "Decoded cells") asks the module that wrote it, which reads the row
   again by its primary key and answers the text, JSON or vector inside; Raw hex stays one
   click away. */

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

/** The hex dump with its byte count line. */
function dbHexBody(value        )              {
  const p = dbHexPreview(value, DB_VALUE_HEX_MAX);
  return h("div", null,
    h("div", { class: "db-val-meta" },
      p.truncated
        ? tr("dataValue.nBytesTruncated", { n: p.bytes.toLocaleString(locale()) })
        : tr("dataValue.nBytes", { n: p.bytes.toLocaleString(locale()) })),
    h("pre", { class: "db-val-pre" }, p.text));
}

/* --- decoded cells (SPEC §data.sqlite "Decoded cells") ----------------------------------------------- */

/** Where a binary cell lives, for the decoder of the module that wrote it: the connection,
 *  the table and column, and the row's primary key as the grid read it. */
;                          
               
                
                 
                               
 

/** The record nouns the modules answer, as keys (tk: painted later through tr). An unknown
 *  noun (a module added after this build) shows as served. */
const DB_DECODE_RECORDS                         = {
  call: tk("dataDecode.recordCall"),
  turn: tk("dataDecode.recordTurn"),
  piece: tk("dataDecode.recordPiece"),
  session: tk("dataDecode.recordSession"),
  run: tk("dataDecode.recordRun"),
  memory: tk("dataDecode.recordMemory"),
};

/** The facts line over a decoded cell: which record, which part and byte range, how the
 *  bytes were stored; for a vector its width, norm and model. Pure. */
function dbDecodedFacts(r              )         {
  const n = (x        )         => { return x.toLocaleString(locale()); };
  const out           = [];
  if (r.record) {
    const noun = Object.prototype.hasOwnProperty.call(DB_DECODE_RECORDS, r.record) ? tr(DB_DECODE_RECORDS[r.record]) : r.record;
    out.push(noun + " " + (r.recordId ?? ""));
  }
  const c = r.chunk;
  if (c) {
    out.push(tr("dataDecode.part", { n: c.part + 1, of: c.parts }));
    out.push(tr("dataDecode.range", { from: n(c.ofs), to: n(c.ofs + c.plain) }));
    out.push(c.codec === "lz4" ? tr("dataDecode.lz4", { stored: n(c.stored) }) : tr("dataDecode.raw"));
    out.push(c.sealed ? tr("dataDecode.sealed") : tr("dataDecode.plainBlock"));
  }
  const v = r.vectorFacts;
  if (v) {
    out.push(tr("dataDecode.dims", { n: v.dims }));
    out.push(tr("dataDecode.norm", { n: v.norm.toFixed(4) }));
    out.push(v.model);
  }
  return out.join(" · ");
}

/** A vector as rows of eight, each led by the index of its first value. Pure. */
function dbVectorText(v          )         {
  const width = String(Math.max(0, v.length - 1)).length;
  const lines           = [];
  for (let i = 0; i < v.length; i += 8) {
    const row = v.slice(i, i + 8).map((x        )         => { return (x < 0 ? "" : " ") + x.toFixed(6); });
    lines.push(String(i).padStart(width) + "  " + row.join(" "));
  }
  return lines.join("\n");
}

/** What the decoded bytes are: JSON as the code block, a vector as its numbers, the rest as
 *  text (a body part cut from a longer body is text, its JSON does not close). */
function dbDecodedContent(r              )              {
  if (r.kind === "vector" && r.vector) return h("pre", { class: "db-val-pre" }, dbVectorText(r.vector));
  if (r.kind === "json" && r.text != null) {
    try { return dbJsonValueNode(JSON.parse(r.text)); } catch (e) { /* not JSON after all: text */ }
  }
  return h("pre", { class: "db-val-pre" }, r.text ?? "");
}

const DECODE_SEAL_REFUSED = "the sealed part did not open: no key on this machine opens it, or it is bound to another row";
const DECODE_NOT_RUNNING = "the memory service is not running";

/** The decoder's English refusals as the panel's copy; anything else passes through the
 *  module-database map (an engine error stays as served). */
function dbDecodeMessage(served        )         {
  if (served === DECODE_SEAL_REFUSED) return tr("dataDecode.sealRefused");
  if (served === DECODE_NOT_RUNNING) return tr("dataDecode.notRunning");
  if (served.startsWith("no ") && served.includes(" row has that key any more")) return tr("dataDecode.rowGone");
  return moduleDbMessage(served);
}

async function dbFetchDecoded(ref                 )                                            {
  const q = "/api/db/" + encodeURIComponent(ref.conn) + "/decode?table=" + encodeURIComponent(ref.table) +
    "&column=" + encodeURIComponent(ref.column) + "&key=" + encodeURIComponent(JSON.stringify(ref.key));
  try {
    const r = await api(q);
    const j          = await r.json().catch(()          => { return {}; });
    if (!r.ok) {
      const msg = typeof j === "object" && j !== null && "error" in j && typeof j.error === "string" ? j.error : "";
      return { error: msg ? dbDecodeMessage(msg) : tr("util.httpN", { n: r.status }) };
    }
    return j                ;
  } catch (e) {
    return { error: tr("util.requestFailedGatewayRunning") };
  }
}

/** The sheet body for a module's binary cell: a waiting line, then the decoded view with a
 *  Raw hex toggle at the end of its facts line. A cell nothing decodes falls back to the hex
 *  dump; a refusal is the form's bad hint above the hex dump, never hidden. Library shapes
 *  only (SPEC §panel.ui): views.css does not grow. */
function dbDecodeBody(value        , ref                 )              {
  const wrap = h("div", null, h("div", { class: "db-val-meta" }, tr("dataDecode.decoding")));
  void dbFetchDecoded(ref).then((r)       => {
    if ("error" in r) {
      wrap.replaceChildren(hint(tr("dataDecode.failed", { msg: r.error }), { bad: true, id: "dbValDecodeErr" }), dbHexBody(value));
      return;
    }
    if (!r.decoded) { wrap.replaceChildren(dbHexBody(value)); return; }
    let raw = false;
    const toggle = btn(tr("dataDecode.rawHex"), { kind: "ghost", id: "dbValRawHex" });
    const slot = h("div", null, dbDecodedContent(r));
    toggle.onclick = ()       => {
      raw = !raw;
      toggle.textContent = raw ? tr("dataDecode.decodedView") : tr("dataDecode.rawHex");
      slot.replaceChildren(raw ? dbHexBody(value) : dbDecodedContent(r));
    };
    wrap.replaceChildren(formActions(h("span", { class: "db-val-meta grow", id: "dbValFacts" }, dbDecodedFacts(r)), toggle), slot);
  });
  return wrap;
}

/** Open the read-only viewer. One primary action (Close); Escape and the backdrop close
 *  too. `where` is the caption the caller knows (table for grid cells, "SQL result" for
 *  console results). `decode` names a module file's binary cell (SPEC §data.sqlite): its
 *  bytes are asked of the module that wrote them. */
function dbOpenValueSheet(column        , value         , where                           , decode                         )       {
  const kind = dbValueKind(value);
  let body        ;
  if (kind === "json") {
    const parsed = typeof value === "object" ? value : JSON.parse(value          );
    body = dbJsonValueNode(parsed);
  } else if (kind === "hex") {
    body = decode ? dbDecodeBody(value          , decode) : dbHexBody(value          );
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

export { DB_VALUE_HEX_MAX, dbDecodeMessage, dbDecodedFacts, dbHexPreview, dbJsonValueNode, dbOpenValueSheet, dbValueKind, dbVectorText };
                                
