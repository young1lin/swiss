/*
 * Copyright 2026 The swiss authors
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

import { $, apiJson, el, state } from "./util.js";
import { dbIsRedis } from "./data-browsers.js";
import { dbSqlPaint } from "./data-filters.js";

/* --- SQL completion (docs/22 W3.1) ------------------------------------------------------------------ */
/* The console's suggestion list. The SERVER builds the candidate set (dialect keywords + table
   names + the FROM-nearest table's columns, cached per connection); this side only decides
   WHEN to ask: 150ms after a keystroke that ends in a word, never for redis, never more than
   8 rows on screen. Positioning borrows the highlight overlay's own trick — a hidden mirror
   that inherits the textarea's face and places a marker at the caret. */

var SUGGEST_DEBOUNCE_MS = 150;
var SUGGEST_MAX_ITEMS = 8;
var dbSuggestTimer                                       = null;
var dbSuggestItems                        = [];
var dbSuggestSel = -1;
var dbSuggestPrefix = "";

/** The word the server would complete: the [A-Za-z0-9_.$] run ending at the caret — the same
 *  character class sql_word_ending_at speaks on the server. Pure. */
function dbSuggestPrefixAt(text         , caret        )         {
  var s = String(text).slice(0, caret);
  var m = s.match(/[A-Za-z0-9_.$]+$/);
  return m ? m[0] : "";
}

/** The caret as a byte offset: the server slices bytes, a selection index counts UTF-16
 *  units, and the two only agree while the statement is ASCII. Pure. */
function dbSuggestByteOffset(text         , caret        )         {
  return new TextEncoder().encode(String(text).slice(0, caret)).length;
}

function dbSuggestHide()       {
  var box = $("dbSuggest");
  if (box && box.parentNode) box.parentNode.removeChild(box);
  dbSuggestItems = [];
  dbSuggestSel = -1;
  dbSuggestPrefix = "";
}

/** The input hook (debounced): a caret over no word closes the list at once — a debounce that
 *  kept a dead list on screen would be a lie about what is being completed. */
function dbSuggestOnInput()       {
  clearTimeout(dbSuggestTimer );
  var d = state.db;
  if (!d || !d.conn || dbIsRedis()) { dbSuggestHide(); return; }
  if (!dbSuggestPrefixAt(this.value, this.selectionStart)) { dbSuggestHide(); return; }
  var ta = this;
  dbSuggestTimer = setTimeout(function () { void dbSuggestFetch(ta); }, SUGGEST_DEBOUNCE_MS);
}

async function dbSuggestFetch(ta                     )                {
  var d = state.db;
  if (!d || !d.conn || dbIsRedis() || !ta.closest(".db-sql-wrap")) { dbSuggestHide(); return; }
  var caret = ta.selectionStart ;
  var text = ta.value;
  var prefix = dbSuggestPrefixAt(text, caret);
  if (!prefix) { dbSuggestHide(); return; }
  var j = await apiJson                      ("/api/db/" + encodeURIComponent(d.conn) + "/completion", {
    method: "POST",
    body: JSON.stringify({ sql: text, caret: dbSuggestByteOffset(text, caret) }),
  });
  if (!j) { dbSuggestHide(); return; }
  var items                        = (j.items || []).slice(0, SUGGEST_MAX_ITEMS);
  if (!items.length) { dbSuggestHide(); return; }
  // A reply for a word the caret has already left is stale: drop it, the next keystroke is
  // fetching its own.
  if (dbSuggestPrefixAt(ta.value, ta.selectionStart ) !== prefix) return;
  dbSuggestItems = items;
  dbSuggestPrefix = prefix;
  dbSuggestSel = -1;
  dbSuggestRender(ta);
}

function dbSuggestRender(ta                     )       {
  // Replace only the BOX ELEMENT here — dbSuggestHide() also clears the item list, and this
  // function draws from that list: calling it first once rendered an empty, positioned box
  // for every reply (caught live on 19998).
  var old = $("dbSuggest");
  if (old && old.parentNode) old.parentNode.removeChild(old);
  var wrap = ta.closest(".db-sql-wrap");
  if (!wrap) return;
  var box = el("div", "db-suggest");
  box.id = "dbSuggest";
  dbSuggestItems.forEach(function (it                     , i        )       {
    var row = el("div", "db-suggest-item");
    row.appendChild(el("span", "db-suggest-label", String(it.label)));
    row.appendChild(el("span", "db-suggest-kind", String(it.kind)));
    row.title = it.detail || it.kind;
    // mousedown, not click: the textarea's blur would tear the list down before a click on
    // it lands, and preventDefault keeps the caret where the accept will splice.
    row.onmousedown = function (e            )       { e.preventDefault(); dbSuggestAccept(i); };
    box.appendChild(row);
  });
  wrap.appendChild(box);
  dbSuggestPlace(ta, box);
}

/** Put the list at the caret. The mirror div inherits the textarea's face (same font, size,
 *  padding, width, wrapping) and carries a zero-width marker right after the text up to the
 *  caret; the marker's offset minus the textarea's scroll is the caret's point in the wrap. */
function dbSuggestPlace(ta                     , box             )       {
  var wrap = ta.closest(".db-sql-wrap");
  if (!wrap) return;
  var mirror = el("div", "db-sql-mirror db-sql-face");
  mirror.textContent = ta.value.slice(0, ta.selectionStart );
  var marker = el("span", "db-suggest-mark", "\u200b");
  mirror.appendChild(marker);
  wrap.appendChild(mirror);
  var left = marker.offsetLeft - ta.scrollLeft;
  var top = marker.offsetTop - ta.scrollTop + marker.offsetHeight;
  wrap.removeChild(mirror);
  // Below the caret line by default; flip above when the list would run past the console.
  if (top + box.offsetHeight > wrap.clientHeight) {
    top = Math.max(0, top - marker.offsetHeight - box.offsetHeight);
  }
  box.style.left = Math.max(0, left) + "px";
  box.style.top = top + "px";
}

function dbSuggestPaintSel()       {
  var box = $("dbSuggest");
  if (!box) return;
  var rows = box.children;
  for (var i = 0; i < rows.length; i++) {
    rows[i].classList.toggle("sel", i === dbSuggestSel);
  }
}

/** Replace the typed prefix with the chosen label and hand the console back its paint. */
function dbSuggestAccept(i        )       {
  var ta = $                     ("dbSql");
  if (!ta || !dbSuggestItems[i]) { dbSuggestHide(); return; }
  var label = String(dbSuggestItems[i].label);
  var caret = ta.selectionStart ;
  var start = caret - dbSuggestPrefix.length;
  ta.setRangeText(label, start, caret, "end");
  state.db .sqlText = ta.value;
  dbSqlPaint();
  dbSuggestHide();
  ta.focus();
}

/** The console's key hook while the list is open: arrows move, Tab/Enter accept, Esc closes.
 *  Ctrl/Cmd+Enter falls through untouched — Run keeps working with the list on screen. */
function dbSuggestKeys(e               )       {
  if (!dbSuggestItems.length) return;
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    var n = dbSuggestSel < 0
      ? (e.key === "ArrowDown" ? 0 : dbSuggestItems.length - 1)
      : (dbSuggestSel + (e.key === "ArrowDown" ? 1 : -1) + dbSuggestItems.length) % dbSuggestItems.length;
    dbSuggestSel = n;
    dbSuggestPaintSel();
  } else if (e.key === "Tab" || (e.key === "Enter" && !e.ctrlKey && !e.metaKey && !e.altKey)) {
    e.preventDefault();
    dbSuggestAccept(dbSuggestSel < 0 ? 0 : dbSuggestSel);
  } else if (e.key === "Escape") {
    e.preventDefault();
    dbSuggestHide();
  }
}

export { dbSuggestByteOffset, dbSuggestHide, dbSuggestKeys, dbSuggestOnInput, dbSuggestPrefixAt };
