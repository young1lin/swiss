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

/* The panel's UI library (docs/46, ADR-029): the ONLY way a page draws a shape.
 *
 * A page composes these; a shape the library lacks is added HERE first - the function in
 * ui/, its classes in styles/ui.css, a section in the gallery (/admin/ui.html), a test - and
 * only then used. views.css may lay a page out but may not restyle a class ui.css owns
 * (test/ui-css-ownership.test.ts), and ui/ imports nothing but ../h.js, ../i18n.js and itself
 * (test/ui-boundary.test.ts): no api, no state, no views - which is what lets the gallery
 * render every component with made-up data and nothing else loaded.
 *
 * A design is a gallery SCENE built from these, not a page of hand-written CSS (docs/46 U17). */
export { btn, iconBtn, moreBtn } from "./button.js";
export type { BtnOpts, IconBtnOpts } from "./button.js";
export { iconNode } from "./icon.js";
export { dot, spinner, tag } from "./status.js";
export type { DotState, TagOpts } from "./status.js";
export { sw } from "./switch.js";
export { card, emptyNode, failNote, filterInput, inlineForm, note, pageFoot, pager, pane, paneBody, paneHead, resHead, section } from "./page.js";
export type { EmptySpec, PaneOpts } from "./page.js";
export { kvRow, row, sideRow } from "./row.js";
export type { RowCol, RowOpts, SideRowOpts } from "./row.js";
export { groupNode } from "./group.js";
export type { GroupNodeOpts, GroupParts } from "./group.js";
export { seg } from "./seg.js";
export type { SegItem } from "./seg.js";
export { collapseRuns, dayLabel, fmtMs, timeLabel, timeline, timelineMeta, timelineToggle } from "./timeline.js";
export type { TimelineItem, TimelineOpts } from "./timeline.js";
export { anchoredMenu, clampMenuPos, closeMenu, menuOpen, popupMenu, setMenuOpen } from "./menu.js";
export type { MenuItem, MenuItemAction, MenuItemSep } from "./menu.js";
export { closeSelect, initSelects, selectOpen, styleSelect } from "./select.js";
export { DecodedString, JV_INLINE, JV_LINES, decodeStrings, fitsOneLine, formattedCopyText, hasDecoded, jsonCodeNode, plainValue, splitJsonBlock, stringLiteral, textNode, valueBlock } from "./json-view.js";
export type { Painted } from "./json-view.js";
export { closeSheet, initSheet, openFieldSheet, sheet, sheetOpen, showSheet, stackSheet } from "./sheet.js";
export type { FieldSheetSpec, SheetOpts } from "./sheet.js";
export { toTop } from "./to-top.js";
export { checkField, field, form, formActions, formCap, formFold, hint, pair } from "./form.js";
