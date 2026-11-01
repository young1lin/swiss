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

/* The panel's UI library (SPEC §panel.ui, ADR-029): the ONLY way a page draws a shape.
 *
 * A page composes these; a shape the library lacks is added HERE first - the function in
 * ui/, its classes in styles/ui.css, a section in the gallery (/admin/ui.html), a test - and
 * only then used. views.css may lay a page out but may not restyle a class ui.css owns
 * (test/ui-css-ownership.test.ts), and ui/ imports nothing but ../h.js, ../i18n.js and itself
 * (test/ui-boundary.test.ts): no api, no state, no views - which is what lets the gallery
 * render every component with made-up data and nothing else loaded.
 *
 * A design is a gallery SCENE built from these, not a page of hand-written CSS (SPEC §panel.ui). */
export { btn, iconBtn, moreBtn } from "./button.js";
export { resultTable, resultChart } from "./result.js";
export { copyBtn, copyText } from "./copy.js";
export type { BtnOpts, IconBtnOpts } from "./button.js";
export { iconNode } from "./icon.js";
export { isLocalAddress, redacted } from "./redact.js";
export type { RedactOpts } from "./redact.js";
export { dot, heldDot, spinner, tag } from "./status.js";
export type { DotState, TagOpts } from "./status.js";
export { sw } from "./switch.js";
export { card, emptyNode, failNote, filterInput, inlineForm, note, pageFoot, pager, pane, paneBody, paneHead, resHead, section, split } from "./page.js";
export type { EmptySpec, PaneOpts } from "./page.js";
export { kvRow, row, sideRow } from "./row.js";
export type { RowCol, RowOpts, SideRowOpts } from "./row.js";
export { markdownNode } from "./markdown.js";
export { diffFiles, diffNode, diffStatusWord } from "./diff.js";
export type { DiffFile, DiffLine, DiffLineKind, DiffStatus } from "./diff.js";
export {
  dotSize, graphElements, graphHops, graphLayoutOf, graphLegend, graphPalette, graphStyle, graphView, groupElementId, mixHex, packBoxes,
} from "./graph.js";
export type {
  GraphCore, GraphEdge, GraphEdgeStyle, GraphElementDef, GraphEls, GraphEngine, GraphEngineLoader, GraphEngineOptions, GraphEvents,
  GraphGroup, GraphHandle, GraphKey, GraphLayoutOpts, GraphNode, GraphPalette, GraphSpec, GraphStyleBlock, GraphTokenRead, GraphTone,
} from "./graph.js";
export { catChip, docHead, pickList, pickRow, pickSelect, refLink, workspace, wsBar, wsHead, wsScroll, wsSide, wsSplit } from "./workspace.js";
export type { PickRowOpts } from "./workspace.js";
export { groupNode } from "./group.js";
export type { GroupNodeOpts, GroupParts } from "./group.js";
export { seg } from "./seg.js";
export { objTab } from "./tab.js";
export type { ObjTabOpts } from "./tab.js";
export type { SegItem } from "./seg.js";
export { collapseRuns, dayLabel, fmtMs, relTime, timeLabel, timeline, timelineMeta, timelineToggle } from "./timeline.js";
export type { TimelineItem, TimelineOpts } from "./timeline.js";
export { factLine, flow, flowHead, flowIO, flowProse, flowToggle, scrollBox } from "./flow.js";
export type { FlowOpts, FlowStep } from "./flow.js";
export { transcript, transcriptText, transcriptTurn, toolChip } from "./transcript.js";
export type { TranscriptTurn } from "./transcript.js";
export { chatCommands, chatFollow, chatGrow, chatPane, chatQueue } from "./chat.js";
export type { ChatCommand, ChatPaneOpts, ChatQueue, ChatQueued, ChatQueueOpts } from "./chat.js";
export { chatStats } from "./chat-stats.js";
export type { ChatContext, ChatStats, ChatUsage } from "./chat-stats.js";
export { anchoredMenu, clampMenuPos, closeMenu, menuOpen, popupMenu, setMenuOpen } from "./menu.js";
export type { MenuItem, MenuItemAction, MenuItemSep } from "./menu.js";
export { closeSelect, initSelects, selectOpen, styleSelect } from "./select.js";
export { DecodedString, JV_INLINE, JV_LINES, decodeStrings, fitsOneLine, formattedCopyText, hasDecoded, jsonCodeNode, plainValue, proseText, splitJsonBlock, stringLiteral, textNode, tokenCodeNode, tokenSpans, valueBlock } from "./json-view.js";
export type { CodeToken, Painted } from "./json-view.js";
export { CODE_SCHEMES, codeSchemeFor } from "./code-scheme.js";
export { closeSheet, initSheet, openFieldSheet, sheet, sheetOpen, showSheet, stackSheet } from "./sheet.js";
export type { FieldSheetSpec, SheetOpts } from "./sheet.js";
export { toTop } from "./to-top.js";
export { grip, gripFold, gripFolded, gripState } from "./grip.js";
export { dock, dockShow } from "./dock.js";
export type { DockOpts } from "./dock.js";
export type { GripOpts } from "./grip.js";
export { portChip } from "./chip.js";
export type { ChipTone } from "./chip.js";
export { pickPanel, pickerRow } from "./pick.js";
export type { PickPanel } from "./pick.js";
export { checkField, checkGrid, checkRow, editDetail, editLine, editList, editSub, field, form, formActions, formCap, formFold, hint, pair } from "./form.js";
