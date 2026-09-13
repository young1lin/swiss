import { $, esc } from "./util.js";
import { closeSheet } from "./add-sheet.js";

/* --- value viewer sheet (docs/22 W5.3) ----------------------------------------------------------- */
/* A cell is a 30px row; its content is not. "View value\u2026" opens the full value in a
   read-only sheet — the same surface the editor lives in, minus every write affordance
   (editing stays in the editor sheet; this one only looks). cloudbeaver registers value
   presentations by MIME (TextValuePresentationBootstrap); the equivalent here is one pure
   kind picker over the value itself, because the wire already says what the value is:
   - the \\x hex form (both adapters' binary ride, docs/22 W4b) opens as a hex dump,
   - an http(s) text opens as itself plus the link,
   - parseable JSON opens as a details-folded tree,
   - everything else opens as full text.
   The column type deliberately does not sway the choice: a MySQL BLOB that is valid UTF-8
   rides the wire as text (mysql.rs), and its honest presentation IS the text. */

/** The hex cap: the first 512 bytes of a binary value, then the panel's standing
 *  truncation word. The WIRE stays uncapped on purpose — the whole hex string is the
 *  value's exact address (the keyless-table md5 digests the decoded bytes, W4b) — so the
 *  sheet caps its own view, never the value it was handed. */
var DB_VALUE_HEX_MAX = 512;

function dbValueJsonLike(s) {
  var t = String(s == null ? "" : s).trim();
  return t.startsWith("{") || t.startsWith("[");
}

/** Which of the four presentations a value opens as. Pure; the sheet is a thin wrapper. */
function dbValueKind(value) {
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
function dbHexPreview(value, maxBytes) {
  var hex = String(value).replace(/^\\x/, "");
  var bytes = Math.floor(hex.length / 2);
  var truncated = bytes > maxBytes;
  var show = truncated ? hex.slice(0, maxBytes * 2) : hex;
  var pairs = show.match(/.{2}/g) || [];
  var lines = [];
  for (var i = 0; i < pairs.length; i += 16) lines.push(pairs.slice(i, i + 16).join(" "));
  return { text: lines.join("\n"), bytes: bytes, truncated: truncated };
}

/** One JSON node as HTML. Containers fold behind <details> — the root open, everything
 *  nested closed — so a deep document opens to its top level and expands on demand. Object
 *  keys are identity (text face); values are values you would copy (mono). Everything is
 *  escaped on the way out: a document full of tags stays inert text. Pure. */
function dbJsonNode(v, isOpen) {
  if (v === null || v === undefined) return '<span class="db-val-v db-null">null</span>';
  if (Array.isArray(v) || typeof v === "object") {
    var isArr = Array.isArray(v);
    var keys = isArr ? v : Object.keys(v);
    var summary = isArr ? (keys.length ? "[ " + keys.length + " ]" : "[ ]")
      : (keys.length ? "{ " + keys.length + " }" : "{ }");
    var inner = "";
    if (isArr) {
      keys.forEach(function (x) {
        inner += '<div class="db-val-row">' + dbJsonNode(x, false) + "</div>";
      });
    } else {
      keys.forEach(function (k) {
        inner += '<div class="db-val-row"><span class="db-val-k">' + esc(k) + "</span>" + dbJsonNode(v[k], false) + "</div>";
      });
    }
    return '<details class="db-val-node"' + (isOpen ? " open" : "") + "><summary>" + summary + "</summary>" + inner + "</details>";
  }
  return '<span class="db-val-v">' + esc(JSON.stringify(v)) + "</span>";
}

function dbJsonTreeHtml(v) { return dbJsonNode(v, true); }

/** Open the read-only viewer. One primary action (Close); Escape and the backdrop close
 *  too. `where` is the caption the caller knows (table for grid cells, "SQL result" for
 *  console results). */
function dbOpenValueSheet(column, value, where) {
  var kind = dbValueKind(value);
  var body;
  if (kind === "json") {
    var parsed = typeof value === "object" ? value : JSON.parse(value);
    body = '<div class="db-val-tree">' + dbJsonTreeHtml(parsed) + "</div>";
  } else if (kind === "hex") {
    var p = dbHexPreview(value, DB_VALUE_HEX_MAX);
    body = '<div class="db-val-meta">' + p.bytes.toLocaleString() + " bytes" +
      (p.truncated ? " \u00b7 truncated" : "") + "</div>" +
      '<pre class="db-val-pre">' + esc(p.text) + "</pre>";
  } else if (kind === "url") {
    body = '<pre class="db-val-pre"><a class="db-link" href="' + esc(value) +
      '" target="_blank" rel="noopener noreferrer">' + esc(value) + "</a></pre>";
  } else {
    body = '<pre class="db-val-pre">' + esc(String(value)) + "</pre>";
  }
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="View value">' +
      '<div class="sheet-head"><div class="db-cell-head">' +
        "<h2>" + esc(column) + "</h2>" +
        '<span class="db-cell-where">' + esc(where || "") + "</span>" +
      "</div></div>" +
      '<div class="sheet-body">' + body + "</div>" +
      '<div class="sheet-foot"><span class="grow"></span>' +
        '<button class="btn primary" id="dbValClose">Close</button></div>' +
    "</div>";
  $("sheet").hidden = false;
  var onKey = function (e) {
    if (e.key === "Escape") { closeValueSheet(); }
  };
  var closeValueSheet = function () {
    document.removeEventListener("keydown", onKey);
    closeSheet();
  };
  $("dbValClose").onclick = closeValueSheet;
  document.addEventListener("keydown", onKey);
  $("sheet").onclick = function (e) { if (e.target === $("sheet")) closeValueSheet(); };
}

export { DB_VALUE_HEX_MAX, dbHexPreview, dbJsonTreeHtml, dbOpenValueSheet, dbValueKind };
