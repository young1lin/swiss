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

/* The gallery's scenes (docs/46 §2.6, U17): whole pages drawn from the library and nothing
 * else. A new design is a new scene here - composed from ui/, reviewed at
 * /admin/ui.html#scene-<id> in both themes, both languages and both widths - and only then
 * built into a page. The rules (test/ui-gallery.test.ts, G6):
 *
 *   - calls into ./ui/index.js, ./h.js and ./i18n.js only: no api, no state, no views;
 *   - no style, and no class that base.css or ui.css does not define - a scene that needs a
 *     shape the library lacks is the signal to add it to the library first;
 *   - the data is made up (no real host, database or server names), and every word a reader
 *     sees goes through tr() so the Chinese pass reads Chinese.
 *
 * Each scene returns the shell's body - a .shell holding a pane, and for the resource page the
 * source list beside it - so it renders exactly where a real page would. */
                                     
import { h } from "./h.js";
import { tk, tr } from "./i18n.js";
import {
  btn, card, decodeStrings, dot, emptyNode, groupNode, iconBtn, iconNode, inlineForm, jsonCodeNode, moreBtn,
  pageFoot, pane, paneHead, row, section, seg, sideRow, sw, tag, timeline,
} from "./ui/index.js";
                                                  

                        
             
                                         
                   
                                                                                         
                 
                                      
 

const MIN = 60 * 1000;

/* --- made-up data ---------------------------------------------------------------------------- */

const GROUPS = ["default", "staging"];

const TOKENS = [
  { group: "default", name: "ci-runner", key: "swk_…3f9a", age: 3 },
  { group: "default", name: "grafana", key: "swk_…c07e", age: 12 },
  { group: "staging", name: "load-test", key: "swk_…91d2", age: 40, revoked: true },
];

const SERVERS = [
  { group: "default", name: "orders-db", glyph: "pg", state: "up"          },
  { group: "default", name: "cache", glyph: "redis", state: "up"          },
  { group: "default", name: "docs-search", glyph: "globe", state: "idle"          },
  { group: "tools", name: "design-files", glyph: "figma", state: "error"          },
];

function calls(now        )                 {
  return [
    { id: "c1", at: now - 2 * MIN, title: "query_orders", arg: '{"status":"open","limit":20}', ms: 38, same: "q-open" },
    { id: "c2", at: now - 3 * MIN, title: "query_orders", arg: '{"status":"open","limit":20}', ms: 41, same: "q-open" },
    { id: "c3", at: now - 4 * MIN, title: "query_orders", arg: '{"status":"open","limit":20}', ms: 36, same: "q-open" },
    { id: "c4", at: now - 9 * MIN, title: "list_tables", arg: "{}", ms: 1840 },
    { id: "c5", at: now - 26 * 60 * MIN, title: "query_orders", arg: '{"status":"lost"}', ms: 12,
      status: { text: tr("gallery.s.error"), tone: "bad" } },
  ];
}

function traffic(now        )                 {
  return [
    { id: "e1", at: now - 1 * MIN, title: "orders-db · query_orders", arg: '{"limit":5}', who: "claude-code", ms: 44 },
    { id: "e2", at: now - 5 * MIN, title: "cache · get", arg: '{"key":"session:42"}', who: "cursor", ms: 3, same: "get-42" },
    { id: "e3", at: now - 6 * MIN, title: "cache · get", arg: '{"key":"session:42"}', who: "cursor", ms: 2, same: "get-42" },
    { id: "e4", at: now - 30 * MIN, title: "design-files · export", arg: '{"frame":"hero"}', who: "claude-code", ms: 2400,
      status: { text: tr("gallery.s.timeout"), tone: "bad" } },
    { id: "e5", at: now - 25 * 60 * MIN, title: "docs-search · search", arg: '{"q":"retry policy"}', who: "cursor", ms: 820 },
    { id: "e6", at: now - 50 * 60 * MIN, title: "orders-db · list_tables", arg: "{}", who: "claude-code", ms: 96 },
  ];
}

/** An expanded call's body: the arguments and the reply, the Logs code block. */
function callBody(it              )         {
  const reply = { rows: [{ id: 1042, status: "open", total: "18.40" }], more: false, note: '{"cached":true}' };
  return [
    jsonCodeNode(decodeStrings(JSON.parse(it.arg || "{}")), false).node,
    jsonCodeNode(decodeStrings(reply), false).node,
  ];
}

/* --- the scenes ------------------------------------------------------------------------------- */

/** A content page (skill §6 A): the pinned head, the inline create form, two groups of rows,
 *  the foot - the Tokens page's shape. */
function content()              {
  const groups = GROUPS.map((g) => {
    const members = TOKENS.filter((t) => t.group === g);
    return groupNode({
      name: g, count: members.length, density: "page",
      addTitle: tr("gallery.s.newIn", { g }), moreTitle: tr("gallery.s.groupActions"),
    }, members.map((t) => row({
      name: t.name,
      sub: [h("code", null, t.key), " · ", tr("gallery.s.createdDaysAgo", { n: t.age })],
      muted: !!t.revoked,
      cols: t.revoked ? [tag(tr("gallery.s.revoked"))] : [],
      primary: t.revoked ? undefined : btn(tr("gallery.s.copy"), { kind: "ghost", icon: "copy" }),
      more: moreBtn(tr("gallery.s.moreFor", { name: t.name })),
    }))).root;
  });
  return h("div", { class: "shell" },
    pane({ wide: true },
      paneHead({
        desc: tr("gallery.s.tokensDesc"),
        actions: [iconBtn("folder-plus", tr("gallery.s.newGroup"))],
      }),
      inlineForm(
        h("input", { placeholder: tr("gallery.s.tokenLabel"), aria: { label: tr("gallery.s.tokenLabel") } }),
        h("select", { aria: { label: tr("gallery.s.group") } },
          GROUPS.map((g) => h("option", { value: g }, g))),
        btn(tr("gallery.s.create"), { kind: "primary" })),
      groups,
      pageFoot({ note: tr("gallery.s.tokensFoot"), rev: "rev 14" })));
}

/** A resource page (skill §6 B): the source list, and the selected resource's pinned head,
 *  its sections as a seg, and its log as the event list with one call open. */
function resource(now        )              {
  const stateWord                         = {
    up: tr("gallery.s.up"), idle: tr("gallery.s.idle"), error: tr("gallery.s.error"),
  };
  const bands = ["default", "tools"].map((g) => {
    const members = SERVERS.filter((s) => s.group === g);
    return groupNode({
      name: g, count: members.length, density: "side",
      addTitle: tr("gallery.s.newIn", { g }), moreTitle: tr("gallery.s.groupActions"),
    }, members.map((s) => sideRow({
      name: s.name, lead: dot(s.state, stateWord[s.state]), tail: iconNode(s.glyph, s.glyph),
      selected: s.name === "orders-db", data: { name: s.name },
    }))).root;
  });
  return h("div", { class: "shell" },
    h("aside", { class: "sidebar" },
      h("div", { class: "side-head" },
        h("input", { type: "search", placeholder: tr("gallery.s.search"), aria: { label: tr("gallery.s.search") } }),
        iconBtn("folder-plus", tr("gallery.s.newGroup"))),
      h("nav", { class: "side-list", role: "listbox", aria: { label: tr("gallery.s.servers") } }, bands)),
    pane({ wide: true },
      paneHead({
        title: "orders-db",
        desc: tr("gallery.s.ordersDesc"),
        sub: [dot("up", stateWord.up), " ", h("code", null, "/mcp/orders-db"), " · ", stateWord.up, " · pg · 38 ms"],
        actions: [btn(tr("gallery.s.disable")), moreBtn(tr("gallery.s.moreFor", { name: "orders-db" }))],
      }),
      seg([
        { id: "tools", label: tr("gallery.s.tools"), n: 4 },
        { id: "resources", label: tr("gallery.s.resources") },
        { id: "run", label: tr("gallery.s.run") },
        { id: "config", label: tr("gallery.s.config") },
        { id: "logs", label: tr("gallery.s.logs") },
      ], "logs", { label: tr("gallery.s.sections") }),
      section({ cap: tr("gallery.s.toolCalls") },
        timeline(calls(now), { now, open: new Set(["c1"]), body: callBody })),
      section({ cap: tr("gallery.s.childProcess") },
        card(row({ lead: dot("up", stateWord.up), name: "postgres-mcp", sub: h("code", null, "npx -y @example/postgres-mcp"),
          toggle: sw(true, tr("gallery.s.enabled")) })))));
}

/** An event page: the event list across three days, with a folded run, several clients (so the
 *  who column earns its place) and a failure as a red tag on an ordinary row. */
function events(now        )              {
  return h("div", { class: "shell" },
    pane({ wide: true },
      paneHead({ desc: tr("gallery.s.trafficDesc"), actions: [moreBtn(tr("gallery.s.trafficActions"))] }),
      timeline(traffic(now), { now, body: callBody })));
}

/** An empty page: the one "nothing here" shape, with the action that fills it. */
function empty()              {
  return h("div", { class: "shell" },
    pane({},
      emptyNode({ icon: "plug", title: tr("gallery.s.emptyTitle"), hint: tr("gallery.s.emptyHint"), action: tr("gallery.s.emptyAction") })));
}

export const SCENES          = [
  { id: "content", titleKey: tk("gallery.scene.content"), build: content,
    uses: ["pane", "paneHead", "iconBtn", "inlineForm", "btn", "groupNode", "row", "tag", "moreBtn", "pageFoot"] },
  { id: "resource", titleKey: tk("gallery.scene.resource"), build: resource,
    uses: ["sideRow", "dot", "iconNode", "groupNode", "pane", "paneHead", "seg", "section", "card", "timeline", "jsonCodeNode", "row", "sw"] },
  { id: "event", titleKey: tk("gallery.scene.event"), build: events,
    uses: ["pane", "paneHead", "moreBtn", "timeline"] },
  { id: "empty", titleKey: tk("gallery.scene.empty"), build: empty,
    uses: ["pane", "emptyNode"] },
];
