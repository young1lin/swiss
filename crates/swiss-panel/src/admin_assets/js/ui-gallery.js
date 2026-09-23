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

/* The UI library's gallery (docs/46 §2.6, U13): /admin/ui.html, reached by typing it - it is
 * not a user feature and has no seat in the panel. It shows every shape the library draws, in
 * every state, then the scenes (ui-scenes.ts): whole pages composed from those shapes, which is
 * where a new design is proposed before any page is built.
 *
 * It is a page of the panel in everything but navigation: the same base.css + ui.css (and not
 * views.css - what renders right here renders right on any page), the shell's context bar, the
 * one sprite (fetched from index.html rather than copied, so there is never a second list of
 * icons), the same tr() tables. Its three switches - theme, language, width - are the page's
 * own and live in the query string (?theme=dark&lang=zh&w=960), so a view is a link and
 * flipping one never touches the panel's stored preferences.
 *
 * Everything drawn here goes through ./ui/index.js; the gallery owns only its bar and the
 * few handlers that make the demos answer (a menu opens, a sheet opens, a call expands). */
                                     
import { fill, h } from "./h.js";
import { install, langPref, tk, tr } from "./i18n.js";
import { SCENES } from "./ui-scenes.js";
import {
  anchoredMenu, btn, card, checkField, closeMenu, closeSheet, decodeStrings, dot, emptyNode, failNote, field,
  filterInput, form, formActions, formCap, formFold, groupNode, hint, iconBtn, iconNode, initSelects, initSheet,
  inlineForm, jsonCodeNode, kvRow, menuOpen, moreBtn, note, openFieldSheet, pageFoot, pager, pair, pane, paneHead,
  popupMenu, resHead, row, section, seg, sheet, sheetOpen, showSheet, sideRow, spinner, sw, tag, timeline,
  timelineMeta, timelineToggle, toTop, valueBlock,
} from "./ui/index.js";
                                                            

                           
                                                                         

const MIN = 60 * 1000;

/* --- the view: theme, language, width --------------------------------------------------------- */

/** The query string wins; without one the gallery opens the way the panel is set, and never
 *  writes that setting back. */
function readView()       {
  const q = new URLSearchParams(location.search);
  const theme = q.get("theme");
  const lang = q.get("lang");
  return {
    theme: theme === "dark" || theme === "light" ? theme : document.documentElement.dataset.theme === "dark" ? "dark" : "light",
    lang: lang === "zh" ? "zh-CN" : lang === "en" ? "en" : langPref(),
    width: q.get("w") === "960" ? 960 : 1440,
  };
}

function writeView(v      )       {
  const q = new URLSearchParams(location.search);
  q.set("theme", v.theme);
  q.set("lang", v.lang === "zh-CN" ? "zh" : "en");
  q.set("w", String(v.width));
  history.replaceState(null, "", location.pathname + "?" + q.toString() + location.hash);
}

let view       = { theme: "light", lang: "en", width: 1440 };

async function applyView(v      )                {
  view = v;
  document.documentElement.setAttribute("data-theme", v.theme);
  if (v.lang === "zh-CN") {
    const table                                      = await import("./locales/zh.js");
    install("zh-CN", table.default);
  } else {
    install("en", null);
  }
  // The width is the viewport the page is judged at: the app column is held to it, and a
  // hairline marks its right edge when the window is wider.
  const app = appNode();
  app.style.width = v.width + "px";
  app.style.maxWidth = "100%";
  app.style.boxShadow = "1px 0 0 var(--sep)";
}

function appNode()              {
  const node = document.getElementById("app");
  if (!node) throw new Error("ui.html has no #app");
  return node;
}

/* --- the one sprite --------------------------------------------------------------------------- */

/** The icons live once, in index.html. Fetch it and move its sprite into this page - a second
 *  copy would drift, and a missing glyph here would pass unnoticed for the same reason. */
async function loadSprite()                    {
  const res = await fetch("/admin/index.html");
  const doc = new DOMParser().parseFromString(await res.text(), "text/html");
  const sprite = doc.querySelector("svg[hidden]");
  if (!sprite) return [];
  document.body.prepend(document.importNode(sprite, true));
  return Array.from(sprite.querySelectorAll("symbol")).map((s) => s.id.replace(/^i-/, ""));
}

let icons           = [];

/* --- the bar ---------------------------------------------------------------------------------- */

function route()         {
  const id = location.hash.replace(/^#/, "");
  return SCENES.some((s) => "scene-" + s.id === id) ? id : "components";
}

function bar(current        )              {
  const tabs = [{ id: "components", key: tk("gallery.components") }]
    .concat(SCENES.map((s) => ({ id: "scene-" + s.id, key: s.titleKey })));
  return h("header", { class: "ctxbar" },
    h("span", { class: "ctx-title" }, iconNode("puzzle"), h("span", { class: "ctx-name" }, tr("gallery.title"))),
    h("nav", { class: "ctx-tabs", aria: { label: tr("gallery.views") } },
      tabs.map((t) => h("a", { class: "ctx-tab", href: "#" + t.id, aria: { current: t.id === current ? "page" : null } }, tr(t.key)))),
    h("span", { class: "chip" }, tr("gallery.widthChip", { w: view.width })),
    h("span", { class: "app-zone" },
      iconBtn(view.width === 1440 ? "collapse" : "expand", tr(view.width === 1440 ? "gallery.to960" : "gallery.to1440"),
        { ghost: true, data: { g: "width" } }),
      iconBtn(view.theme === "dark" ? "sun" : "moon", tr(view.theme === "dark" ? "gallery.toLight" : "gallery.toDark"),
        { ghost: true, data: { g: "theme" } }),
      iconBtn("languages", tr("gallery.toOtherLang"), { ghost: true, data: { g: "lang" } })));
}

/* --- the catalogue ---------------------------------------------------------------------------- */

/** One component's section: its words, and a row per state. `uses` is the coverage ledger the
 *  gallery test reads (G6): every library component must be claimed by a section or a scene. */
function entry(uses          , cap        , note        , rows                         , extra         )              {
  const node = section({ cap: tr(cap), note: tr(note) },
    rows.length ? card(rows.map(([label, v]) => kvRow(label, v))) : null, extra);
  node.dataset.ui = uses.join(" ");
  return node;
}

/** Every dot state, with its word. */
const DOTS                            = [
  ["up", tk("gallery.dot.up")], ["starting", tk("gallery.dot.starting")], ["stopping", tk("gallery.dot.stopping")],
  ["idle", tk("gallery.dot.idle")], ["error", tk("gallery.dot.error")], ["down", tk("gallery.dot.down")],
  ["off", tk("gallery.dot.off")],
];
const CHOICES = ["default", "staging", "production"];
const MANY = ["* * * * *", "*/5 * * * *", "*/15 * * * *", "0 * * * *", "0 3 * * *", "0 3 * * 1", "0 0 1 * *", "@reboot"];

function sampleCalls(now        )                 {
  return [
    { id: "g1", at: now - 3 * MIN, title: "query_orders", arg: '{"status":"open"}', ms: 42, same: "open" },
    { id: "g2", at: now - 4 * MIN, title: "query_orders", arg: '{"status":"open"}', ms: 40, same: "open" },
    { id: "g3", at: now - 12 * MIN, title: "list_tables", arg: "{}", ms: 1650 },
    { id: "g4", at: now - 27 * 60 * MIN, title: "export_frame", arg: '{"frame":"hero"}', ms: 88,
      status: { text: tr("gallery.d.error"), tone: "bad" } },
  ];
}

const SAMPLE_JSON = {
  id: 1042, status: "open", paid: false, discount: null,
  lines: [{ sku: "A-17", qty: 2 }, { sku: "B-03", qty: 1 }],
  meta: '{"source":"import","batch":7}',
};

/** An open call, the way Logs draws one: what the row left out, then the arguments. */
function callBody(it              )         {
  return [
    timelineMeta([tr("gallery.d.via", { host: "mcp" }), tr("gallery.d.replySize", { n: 96 })]),
    valueBlock({ label: tr("gallery.d.arguments"), tools: [iconBtn("copy", tr("gallery.d.copy"), { ghost: true }), moreBtn(tr("gallery.d.more"))] },
      jsonCodeNode(decodeStrings(JSON.parse(it.arg || "{}")), false, { oneLine: true }).node),
  ];
}

/** A tool argument's name, its JSON type and a sample value - data, the same in every language. */
const SQL_ARG = "sql";
const SQL_TYPE = "string";
const SQL_SAMPLE = "SELECT 1";
const HOST_SAMPLE = "127.0.0.1";
const PORT_SAMPLE = "5432";
const KEY_SAMPLE = "~/.ssh/id_ed25519";

/** A status line's words that are the same in every language: an HTTP status is a value. */
const HTTP_502 = "HTTP 502";

function catalogue(now        )              {
  const sections = [
    entry(["btn", "iconBtn", "moreBtn"], tk("gallery.c.buttons"), tk("gallery.c.buttonsNote"), [
      [tr("gallery.st.push"), btn(tr("gallery.d.save"))],
      [tr("gallery.st.primary"), btn(tr("gallery.d.create"), { kind: "primary" })],
      [tr("gallery.st.ghost"), btn(tr("gallery.d.cancel"), { kind: "ghost" })],
      [tr("gallery.st.danger"), btn(tr("gallery.d.delete"), { kind: "danger" })],
      [tr("gallery.st.withIcon"), btn(tr("gallery.d.run"), { icon: "play" })],
      [tr("gallery.st.disabled"), btn(tr("gallery.d.save"), { disabled: true })],
      [tr("gallery.st.iconBtn"), iconBtn("copy", tr("gallery.d.copy"))],
      [tr("gallery.st.iconGhost"), iconBtn("folder-plus", tr("gallery.d.newGroup"), { ghost: true })],
      [tr("gallery.st.iconPressed"), iconBtn("star", tr("gallery.d.pin"), { ghost: true, pressed: true })],
      [tr("gallery.st.more"), moreBtn(tr("gallery.d.more"))],
    ]),
    entry(["dot", "tag", "spinner"], tk("gallery.c.status"), tk("gallery.c.statusNote"),
      DOTS.map(([s, k])                   => [tr(k), dot(s, tr(k))]).concat([
        [tr("gallery.st.tagWord"), tag(tr("gallery.d.proxy"))],
        [tr("gallery.st.tagMono"), tag("npx", { mono: true })],
        [tr("gallery.st.tagBad"), tag(tr("gallery.d.error"), { tone: "bad" })],
        [tr("gallery.st.tagWarn"), tag(tr("gallery.d.slow"), { tone: "warn" })],
        [tr("gallery.st.spinner"), [spinner(), " ", tr("gallery.d.loading")]],
      ])),
    entry(["sw"], tk("gallery.c.switch"), tk("gallery.c.switchNote"), [
      [tr("gallery.st.on"), sw(true, tr("gallery.d.enabled"))],
      [tr("gallery.st.off"), sw(false, tr("gallery.d.enabled"))],
      [tr("gallery.st.disabled"), sw(true, tr("gallery.d.enabled"), { disabled: true })],
    ]),
    entry(["styleSelect"], tk("gallery.c.select"), tk("gallery.c.selectNote"), [
      [tr("gallery.st.closed"), h("select", { aria: { label: tr("gallery.d.group") } }, CHOICES.map((c) => h("option", { value: c }, c)))],
      [tr("gallery.st.longList"), h("select", { aria: { label: tr("gallery.d.schedule") } }, MANY.map((c) => h("option", { value: c }, c)))],
      [tr("gallery.st.disabled"), h("select", { disabled: true, aria: { label: tr("gallery.d.group") } }, CHOICES.map((c) => h("option", { value: c }, c)))],
    ]),
    entry(["paneHead", "resHead", "anchoredMenu", "note", "failNote", "filterInput", "pager", "pageFoot", "inlineForm", "section", "card", "kvRow"], tk("gallery.c.page"), tk("gallery.c.pageNote"), [
      [tr("gallery.st.contentHead"), paneHead({ desc: tr("gallery.d.headDesc"), actions: [iconBtn("folder-plus", tr("gallery.d.newGroup")), btn(tr("gallery.d.newItem"), { kind: "primary", icon: "plus" })] })],
      // Out of a pane the two layers do not pin; the resource scene shows them stuck.
      [tr("gallery.st.resourceHead"), resHead({
        title: "orders-db", desc: tr("gallery.d.resourceDesc"),
        sub: [dot("up", tr("gallery.dot.up")), h("code", null, "/mcp/orders-db"), "·", tr("gallery.dot.up")],
        actions: [btn(tr("gallery.d.disable")), moreBtn(tr("gallery.d.more"), { data: { demo: "anchored" } })],
        nav: seg([{ id: "tools", label: tr("gallery.d.tools"), n: 4 }, { id: "logs", label: tr("gallery.d.logs") }], "logs"),
      })],
      [tr("gallery.st.note"), note(tr("gallery.d.noteQuiet"))],
      [tr("gallery.st.noteBusy"), note(tr("gallery.d.loading"), { busy: true })],
      [tr("gallery.st.noteErr"), note(tr("gallery.d.noteErr"), { err: true })],
      [tr("gallery.st.failNote"), failNote({ text: tr("gallery.d.failText"), why: HTTP_502, action: btn(tr("gallery.d.retry")) })],
      [tr("gallery.st.filter"), filterInput({ placeholder: tr("gallery.d.filter"), label: tr("gallery.d.filter") })],
      [tr("gallery.st.pager"), pager({ label: tr("gallery.d.pages"), status: tr("gallery.d.pageN", { n: 2 }), prev: btn(tr("gallery.d.newer")), next: btn(tr("gallery.d.older")) })],
      [tr("gallery.st.pagerBusy"), pager({
        label: tr("gallery.d.pages"), status: [tr("gallery.d.pageN", { n: 2 }), spinner(), tr("gallery.d.loading")],
        prev: btn(tr("gallery.d.newer"), { disabled: true }), next: btn(tr("gallery.d.older"), { disabled: true }),
      })],
      [tr("gallery.st.inlineForm"), inlineForm(h("input", { placeholder: tr("gallery.d.label"), aria: { label: tr("gallery.d.label") } }), btn(tr("gallery.d.create"), { kind: "primary" }))],
      [tr("gallery.st.foot"), pageFoot({ note: tr("gallery.d.footNote"), rev: "rev 14" })],
      [tr("gallery.st.kvSans"), tr("gallery.d.kvSentence")],
      [tr("gallery.st.kvMono"), h("code", null, "~/.swiss/gateway.json")],
    ]),
    entry(["row"], tk("gallery.c.rows"), tk("gallery.c.rowsNote"), [], card(
      row({ lead: dot("up", tr("gallery.dot.up")), name: "orders-db", sub: [h("code", null, "5432 → 127.0.0.1:15432"), " · ", tr("gallery.d.via", { host: "bastion-eu" })] }),
      row({ lead: dot("error", tr("gallery.dot.error")), name: "reports", err: tr("gallery.d.rowErr") }),
      row({ name: "nightly-backup", cols: [{ v: "0 3 * * *", mono: true, title: tr("gallery.d.schedule") }, tr("gallery.d.lastRun")] }),
      row({ lead: dot("up", tr("gallery.dot.up")), name: "grafana", sub: tr("gallery.d.rowSub"), toggle: sw(true, tr("gallery.d.enabled")), primary: btn(tr("gallery.d.copy"), { kind: "ghost", icon: "copy" }), more: moreBtn(tr("gallery.d.more")) }),
      row({ lead: dot("off", tr("gallery.dot.off")), name: "load-test", sub: tr("gallery.d.rowMuted"), muted: true, cols: [tag(tr("gallery.d.revoked"))] }),
      // A record behind the row (a tool): the name and sub open in place; the controls stay out.
      row({
        name: h("code", null, "query_orders"), sub: tr("gallery.d.toolDesc"), title: tr("gallery.d.toolDesc"),
        detail: [
          valueBlock({ label: tr("gallery.d.fullDescription"), text: tr("gallery.d.toolDesc") }),
          valueBlock({ label: tr("gallery.d.inputSchema") }, jsonCodeNode({ type: "object", required: ["status"], properties: { status: { type: "string" } } }, true).node),
        ],
        primary: btn(tr("gallery.d.try")), toggle: sw(true, tr("gallery.d.enabled")),
      }))),
    entry(["sideRow"], tk("gallery.c.sideRows"), tk("gallery.c.sideRowsNote"), [], card(
      groupNode({ name: "default", count: 3, density: "side", addTitle: tr("gallery.d.newIn", { g: "default" }), moreTitle: tr("gallery.d.groupActions") },
        sideRow({ name: "orders-db", lead: dot("up", tr("gallery.dot.up")), tail: iconNode("pg", "pg"), selected: true }),
        sideRow({ name: "cache", lead: dot("idle", tr("gallery.dot.idle")), tail: iconNode("redis", "redis") }),
        sideRow({ name: "design-files", lead: dot("error", tr("gallery.dot.error")), tail: tag("http", { mono: true }) })).root)),
    entry(["groupNode"], tk("gallery.c.groups"), tk("gallery.c.groupsNote"), [], [
      groupNode({ name: "default", count: 2, density: "page", addTitle: tr("gallery.d.newIn", { g: "default" }), moreTitle: tr("gallery.d.groupActions") },
        row({ name: "ci-runner", sub: h("code", null, "swk_…3f9a") }), row({ name: "grafana", sub: h("code", null, "swk_…c07e") })).root,
      groupNode({ name: "archive", count: 5, density: "page", collapsed: true, addTitle: tr("gallery.d.newIn", { g: "archive" }), moreTitle: tr("gallery.d.groupActions") }).root,
      groupNode({ name: "staging", count: 0, density: "page", emptyText: tr("gallery.d.emptyGroup"), addTitle: tr("gallery.d.newIn", { g: "staging" }), moreTitle: tr("gallery.d.groupActions") }).root,
      groupNode({ name: "tables", label: tr("gallery.d.derived"), count: 2, density: "page", addTitle: null, moreTitle: null },
        row({ name: "orders" }), row({ name: "order_lines" })).root,
    ]),
    entry(["seg"], tk("gallery.c.seg"), tk("gallery.c.segNote"), [
      [tr("gallery.st.counts"), seg([{ id: "tools", label: tr("gallery.d.tools"), n: 4 }, { id: "resources", label: tr("gallery.d.resources"), n: 0 }, { id: "logs", label: tr("gallery.d.logs") }], "tools")],
      [tr("gallery.st.second"), seg([{ id: "all", label: tr("gallery.d.all") }, { id: "errors", label: tr("gallery.d.errors"), n: 1 }], "errors")],
    ]),
    entry(["timeline", "timelineMeta"], tk("gallery.c.timeline"), tk("gallery.c.timelineNote"), [], timeline(sampleCalls(now), { now, open: new Set(["g3"]), body: callBody })),
    entry(["jsonCodeNode", "valueBlock"], tk("gallery.c.code"), tk("gallery.c.codeNote"), [
      [tr("gallery.st.codeOneLine"), valueBlock({ label: tr("gallery.d.arguments"), tools: [iconBtn("copy", tr("gallery.d.copy"), { ghost: true }), moreBtn(tr("gallery.d.more"))] },
        jsonCodeNode({ status: "open", limit: 20 }, false, { oneLine: true }).node)],
      [tr("gallery.st.codeBlock"), valueBlock({ label: tr("gallery.d.result"), notes: [tr("gallery.d.decodedNote")], tools: [iconBtn("copy", tr("gallery.d.copy"), { ghost: true }), moreBtn(tr("gallery.d.more"))] },
        jsonCodeNode(decodeStrings(SAMPLE_JSON), false, { oneLine: true }).node)],
    ]),
    entry(["form", "field", "checkField", "pair", "formActions", "formCap", "formFold", "hint"], tk("gallery.c.forms"), tk("gallery.c.formsNote"), [], card(form(
      pair(
        field({ label: tr("gallery.d.host"), control: h("input", { type: "text", value: HOST_SAMPLE }), hint: tr("gallery.d.hostHint") }),
        field({ label: tr("gallery.d.port"), control: h("input", { type: "text", value: PORT_SAMPLE }) })),
      field({ label: SQL_ARG, required: true, meta: SQL_TYPE, control: h("textarea", { placeholder: SQL_SAMPLE }) }),
      checkField({ label: tr("gallery.d.startNow"), control: h("input", { type: "checkbox", checked: true })                    , hint: tr("gallery.d.startNowHint") }),
      formFold({ summary: tr("gallery.d.advanced") },
        formCap(tr("gallery.d.sshKey")),
        field({ label: tr("gallery.d.keyPath"), control: h("input", { type: "text", placeholder: KEY_SAMPLE }), action: btn(tr("gallery.d.browse")) })),
      hint(tr("gallery.d.refused"), { bad: true }),
      formActions(btn(tr("gallery.d.save"), { kind: "primary" }), btn(tr("gallery.d.cancel")))))),
    entry(["popupMenu", "sheet", "openFieldSheet", "toTop"], tk("gallery.c.floating"), tk("gallery.c.floatingNote"), [
      [tr("gallery.st.menu"), btn(tr("gallery.d.openMenu"), { icon: "ellipsis", data: { demo: "menu" } })],
      [tr("gallery.st.sheet"), btn(tr("gallery.d.openSheet"), { data: { demo: "sheet" } })],
      [tr("gallery.st.fieldSheet"), btn(tr("gallery.d.openFieldSheet"), { data: { demo: "field" } })],
      // The real one is on this page already: it shows once the page is a screen down.
      [tr("gallery.st.toTop"), btn(tr("gallery.d.toEnd"), { data: { demo: "totop" } })],
    ]),
    entry(["emptyNode"], tk("gallery.c.empty"), tk("gallery.c.emptyNote"), [], card(emptyNode({ icon: "plug", title: tr("gallery.d.emptyTitle"), hint: tr("gallery.d.emptyHint"), action: tr("gallery.d.emptyAction") }))),
    entry(["iconNode"], tk("gallery.c.icons"), tk("gallery.c.iconsNote"), icons.map((id)                   => [id, iconNode(id)])),
  ];
  return h("div", { class: "shell" },
    pane({ wide: true }, paneHead({ desc: tr("gallery.intro") }), sections));
}

/* --- the demos -------------------------------------------------------------------------------- */

/** A table name - data, the same in every language. */
const OPEN_TABLE = "orders";

function openDemoMenu(anchor             )       {
  const r = anchor.getBoundingClientRect();
  popupMenu({ left: r.left, top: r.top, bottom: r.bottom, width: r.width }, [
    { label: tr("gallery.d.menuHeading"), fn: () => {}, heading: true },
    { label: OPEN_TABLE, fn: () => {}, icon: "table", dot: true },
    { label: tr("gallery.d.menuPicked"), fn: () => {}, pick: true, on: true },
    { label: tr("gallery.d.menuPick"), fn: () => {}, pick: true },
    { label: tr("gallery.d.menuMore"), fn: () => {}, affordance: "chevron-right" },
    { label: tr("gallery.d.menuRefused"), fn: () => {}, disabled: true, title: tr("gallery.d.menuRefusedWhy") },
    { sep: true },
    { label: tr("gallery.d.delete"), fn: () => {}, danger: true },
  ]);
}

/** The ⋯ of a resource head: the same rows, hung under the head's actions instead of floating. */
function openDemoAnchored(anchor             )       {
  const host = anchor.closest             (".pane-actions");
  if (!host) return;
  anchoredMenu(host, [
    { label: tr("gallery.d.menuHeading"), fn: () => {}, heading: true },
    { label: tr("gallery.d.menuPicked"), fn: () => {}, pick: true, on: true },
    { label: tr("gallery.d.menuPick"), fn: () => {}, pick: true },
    { sep: true },
    { label: tr("gallery.d.delete"), fn: () => {}, danger: true },
  ]);
}

/** Scroll the gallery's own pane to its end, where its back-to-top button is showing. */
function scrollToEnd()       {
  const p = appNode().querySelector             ("main.pane");
  if (p) p.scrollTop = p.scrollHeight;
}

function openDemoSheet()       {
  const cancel = btn(tr("gallery.d.cancel"));
  const ok = btn(tr("gallery.d.save"), { kind: "primary" });
  showSheet(sheet({
    title: tr("gallery.d.sheetTitle"),
    body: card(kvRow(tr("gallery.d.label"), "ci-runner"), kvRow(tr("gallery.d.group"), "default"),
      kvRow(tr("gallery.d.key"), h("code", null, "swk_…3f9a"))),
    // .sheet-foot right-aligns its buttons; a .grow spacer is only for a leading secondary.
    foot: [cancel, ok],
  }));
  cancel.onclick = closeSheet;
  ok.onclick = closeSheet;
}

function openDemoFieldSheet()       {
  openFieldSheet({
    title: tr("gallery.d.fieldTitle"),
    // "taken" shows the inline error a caller's refusal paints; anything else succeeds.
    submit: (v) => (v === "taken" ? tr("gallery.d.nameTaken") : true),
  });
}

/* --- render and wire -------------------------------------------------------------------------- */

/** The made-up data's "now": today at 14:00, whatever the clock says. Items sit minutes and days
 *  before it; measured from the REAL clock, a scene opened just after midnight put "30 minutes
 *  ago" on yesterday and drew a fourth day - the page moved with the time of day. */
function sceneNow()         {
  const d = new Date();
  d.setHours(14, 0, 0, 0);
  return d.getTime();
}

function render()       {
  closeMenu();
  if (sheetOpen()) closeSheet();
  const now = sceneNow();
  const current = route();
  const scene = SCENES.find((s) => "scene-" + s.id === current);
  fill(appNode(), h("div", { class: "workbench" }, bar(current), scene ? scene.build(now) : catalogue(now)));
  // Every render builds a new pane, so its back-to-top button is rebuilt with it (docs/46 U18).
  document.querySelectorAll(".to-top").forEach((b) => { b.remove(); });
  const scroller = appNode().querySelector             ("main.pane");
  if (scroller) document.body.appendChild(toTop(scroller));
  document.title = tr("gallery.docTitle");
}

async function flip(what        )                {
  const next       = { ...view };
  if (what === "theme") next.theme = view.theme === "dark" ? "light" : "dark";
  else if (what === "lang") next.lang = view.lang === "en" ? "zh-CN" : "en";
  else if (what === "width") next.width = view.width === 1440 ? 960 : 1440;
  await applyView(next);
  writeView(next);
  render();
}

function onClick(e            )       {
  const t = e.target instanceof Element ? e.target : null;
  if (!t) return;
  const g = t.closest             ("[data-g]");
  if (g && g.dataset.g) { void flip(g.dataset.g); return; }
  const demo = t.closest             ("[data-demo]");
  if (demo) {
    // The opening click must not reach the document closer below.
    e.stopPropagation();
    if (demo.dataset.demo === "menu") openDemoMenu(demo);
    else if (demo.dataset.demo === "sheet") openDemoSheet();
    else if (demo.dataset.demo === "field") openDemoFieldSheet();
    else if (demo.dataset.demo === "anchored") openDemoAnchored(demo);
    else if (demo.dataset.demo === "totop") scrollToEnd();
    return;
  }
  const sum = t.closest(".tl-sum");
  const item = sum && sum.closest             (".tl-item");
  const list = item && item.closest             (".tl");
  if (item && list && item.dataset.tlId) {
    const arg = item.querySelector(".tl-arg");
    timelineToggle(list, item.dataset.tlId, callBody({ id: "", at: 0, title: "", arg: arg ? arg.textContent || "{}" : "{}" }));
  }
}

/** Boot: the sprite, the view, the shared mechanisms, the first paint. ui.html calls it; the
 *  gallery test calls it on happy-dom. */
export async function boot()                {
  icons = await loadSprite().catch(() => []);
  await applyView(readView());
  writeView(view);
  initSelects();
  initSheet();
  const app = appNode();
  app.addEventListener("click", onClick);
  // The panel's closers live in main.ts and connect.ts; the gallery has just these two.
  document.addEventListener("click", (e) => {
    const menu = document.getElementById("menu");
    if (menuOpen() && menu && !(e.target instanceof Node && menu.contains(e.target))) closeMenu();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && sheetOpen()) closeSheet();
  });
  window.addEventListener("hashchange", render);
  render();
}

if (document.documentElement.dataset.page === "gallery") void boot();
