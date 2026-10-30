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

/* The console history (SPEC §data.history): what every console ran, filed by the server under the
 * database the statement reached - two connection names over one database read one list, and a
 * MySQL database never lists what ran in a PostgreSQL one. The panel keeps no copy of its own:
 * the console's ⋯ lists this database's latest distinct statements from a small cache read again
 * after every run, and the History sheet pages, searches and clears the whole list, one quiet
 * line a run (ui/flow.ts). */
import type { ApiDbHistoryPage, ApiDbHistoryRun } from "./types/api.js";
import { api, apiJson, dbReqGuard, targetEl, toast } from "./util.js";
import { h } from "./h.js";
import { tr, trn } from "./i18n.js";
import { dbConn } from "./db-state.js";
import { dbIsRedis } from "./data-browsers.js";
import { dbIsMongo, mongoCurrentDb } from "./data-mongo.js";
import { dbIsPg } from "./data-view.js";
import { dbShellTokens, dbSqlTokens } from "./data-filters.js";
import { dbLoadConsoleLine } from "./data-grid.js";
import { dbOpenTab } from "./data-tabs.js";
import { tokenCodeNode } from "./ui/json-view.js";
import { btn, closeSheet, dot, emptyNode, filterInput, flow, flowIO, flowToggle, hint, relTime, sheet, showSheet } from "./ui/index.js";
import type { FlowStep } from "./ui/flow.js";

/** How many statements the ⋯ lists, and how many runs it reads to find them. */
const RECENT_MAX = 8;
const RECENT_READ = 50;
/** One page of the sheet. */
const PAGE = 100;

/** The history's address for the console on screen: the connection, and on MongoDB the database
 *  its commands go to (a command's database is in its body, not in the connection). Null with no
 *  connection, or a MongoDB one with no database picked. */
function dbHistoryPath(): string | null {
  const c = dbConn();
  if (!c.conn) return null;
  const path = "/api/db/" + encodeURIComponent(c.conn) + "/history";
  if (!dbIsMongo()) return path;
  const db = mongoCurrentDb();
  return db ? path + "?db=" + encodeURIComponent(db) : null;
}

function withParams(path: string, params: Record<string, string | number | null | undefined>): string {
  const q = Object.keys(params)
    .filter((k) => params[k] != null && params[k] !== "")
    .map((k) => k + "=" + encodeURIComponent(String(params[k])))
    .join("&");
  if (!q) return path;
  return path + (path.includes("?") ? "&" : "?") + q;
}

/** The latest distinct statements of a page of runs, newest first: a statement run five times
 *  is one line, at its newest place. Pure. */
function dbHistoryDistinct(runs: ApiDbHistoryRun[], max: number): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const r of runs) {
    const s = r.stmt.trim();
    if (!s || seen.has(s)) continue;
    seen.add(s);
    out.push(s);
    if (out.length >= max) break;
  }
  return out;
}

/* The ⋯'s list: read once per database, and again after every run. A failed read keeps what was
   there and says nothing - the menu is a convenience, and the sheet speaks for itself. */
let recent: { key: string; stmts: string[] } | null = null;
let recentAsked: string | null = null;
const recentReq = dbReqGuard();

/** The statements the ⋯ lists for the console on screen; empty until its database answered. */
function dbHistoryRecent(): string[] {
  const key = dbHistoryPath();
  return recent && key && recent.key === key ? recent.stmts : [];
}

/** Read the ⋯'s list for the console on screen. */
async function dbHistoryRefresh(): Promise<void> {
  const key = dbHistoryPath();
  if (!key) return;
  recentAsked = key;
  const token = recentReq.issue();
  try {
    const r = await api(withParams(key, { limit: RECENT_READ }));
    if (!r.ok) return;
    const j = (await r.json()) as ApiDbHistoryPage;
    if (!recentReq.accepts(token)) return;
    recent = { key, stmts: dbHistoryDistinct(j.runs, RECENT_MAX) };
  } catch (e) {
    /* the gateway went away mid-read: the menu keeps what it had */
  }
}

/** Read the ⋯'s list once for a database not asked yet - the console's head calls it on paint. */
function dbHistoryEnsure(): void {
  const key = dbHistoryPath();
  if (key && recentAsked !== key) void dbHistoryRefresh();
}

/** Text as a code block with no highlighting: a redis line, a failure's words. */
function plainCode(text: string): HTMLElement {
  return tokenCodeNode([["", text]], { all: true }).node;
}

/** The statement as a code block, in the console's own syntax. */
function stmtNode(stmt: string): HTMLElement {
  if (dbIsRedis()) return plainCode(stmt);
  const tokens = dbIsMongo() ? dbShellTokens(stmt) : dbSqlTokens(stmt, { dquoteIdent: dbIsPg() });
  return tokenCodeNode(tokens, { all: true }).node;
}

/** One run as one quiet line: its state dot, the statement on one line, what it answered and
 *  where from, and when. A failure's words are the second line. */
function runStep(r: ApiDbHistoryRun, conn: string | null): FlowStep {
  const facts = [
    r.rows != null ? trn(r.rows, "dataGrid.nRows.one", "dataGrid.nRows.other") : "",
    r.ms != null ? tr("dataHistory.ms", { ms: r.ms }) : "",
    r.conn !== conn ? tr("dataHistory.via", { conn: r.conn }) : "",
  ].filter(Boolean);
  return {
    id: String(r.id),
    lead: dot(r.ok ? "up" : "error", r.ok ? tr("dataHistory.ok") : tr("dataHistory.failed")),
    title: r.stmt.replace(/\s+/g, " ").trim(),
    mono: true,
    summary: facts.join(" · ") || undefined,
    detail: !r.ok && r.error ? r.error : undefined,
    detailBad: true,
    meta: h("span", { title: new Date(r.ts).toLocaleString() }, relTime(r.ts)),
    data: { hrun: String(r.id) },
  };
}

/** An opened run: the whole statement (and a failure's words), and the act that takes it back. */
function runBody(r: ApiDbHistoryRun): HTMLElement[] {
  return [
    flowIO([
      { label: tr("dataHistory.statement"), notes: r.cut ? [tr("dataHistory.cut")] : [], body: stmtNode(r.stmt) },
      !r.ok && r.error ? { label: tr("dataHistory.error"), body: plainCode(r.error) } : null,
    ]),
    btn(tr("dataHistory.open"), { data: { hload: String(r.id) } }),
  ];
}

/** The History sheet for the console on screen: this database's runs, newest first, a page at a
 *  time, searched by the words a statement holds; a run opens to its whole text and goes back
 *  into the console from there. */
async function dbHistoryOpen(): Promise<void> {
  const path = dbHistoryPath();
  if (!path) { toast(dbIsMongo() ? tr("dataMongo.pickDatabase") : tr("dataSql.databaseConnection"), true); return; }
  const first = await apiJson<ApiDbHistoryPage>(withParams(path, { limit: PAGE }));
  if (!first) return; // the toast said why (the Data view's history is not open, a lease refused)
  const conn = dbConn().conn;
  const runs: ApiDbHistoryRun[] = first.runs.slice();
  const open = new Set<string>();
  let more = first.more;
  let q = "";
  const req = dbReqGuard();

  const search = filterInput({ placeholder: tr("dataHistory.search"), label: tr("dataHistory.search") });
  const list = h("div", { class: "db-history" });
  const earlier = btn(tr("dataHistory.earlier"), { kind: "ghost" });
  const clear = btn(tr("dataHistory.clear"), { kind: "ghost" });
  const close = btn(tr("dataHistory.close"));

  const paint = (): void => {
    list.textContent = "";
    if (!runs.length) {
      list.appendChild(emptyNode(q
        ? { icon: "search", title: tr("dataHistory.noMatch"), hint: tr("dataHistory.noMatchHint") }
        : { icon: "history", title: tr("dataHistory.empty"), hint: tr("dataHistory.emptyHint") }));
    } else {
      list.appendChild(flow(runs.map((r) => runStep(r, conn)), { open, body: (s) => {
        const r = runs.find((x) => String(x.id) === s.id);
        return r ? runBody(r) : null;
      } }));
    }
    earlier.hidden = !more;
    clear.disabled = !runs.length && !q;
  };

  /** Read a page: the first one again (a new search), or the one before the oldest shown. */
  const read = async (next: boolean): Promise<void> => {
    const token = req.issue();
    const before = next && runs.length ? runs[runs.length - 1].id : null;
    const j = await apiJson<ApiDbHistoryPage>(withParams(path, { q, before, limit: PAGE }));
    if (!req.accepts(token) || !j) return;
    if (!next) { runs.length = 0; open.clear(); }
    runs.push(...j.runs);
    more = j.more;
    paint();
  };

  let typing: ReturnType<typeof setTimeout> | null = null;
  search.addEventListener("input", (): void => {
    if (typing !== null) clearTimeout(typing);
    typing = setTimeout((): void => { q = search.value.trim(); void read(false); }, 250);
  });
  earlier.onclick = (): void => { void read(true); };
  close.onclick = closeSheet;
  clear.onclick = (): void => {
    if (!confirm(tr("dataHistory.clearConfirm", { target: first.target }))) return;
    void (async (): Promise<void> => {
      const j = await apiJson<{ cleared: number }>(path, { method: "DELETE" });
      if (!j) return;
      toast(trn(j.cleared, "dataHistory.cleared.one", "dataHistory.cleared.other"));
      recent = null;
      recentAsked = null;
      q = "";
      search.value = "";
      await read(false);
    })();
  };
  list.addEventListener("click", (ev: Event): void => {
    const at = targetEl(ev);
    const load = at ? at.closest<HTMLElement>("[data-hload]") : null;
    if (load) {
      const r = runs.find((x) => String(x.id) === load.dataset.hload);
      if (!r) return;
      closeSheet();
      dbOpenTab({ kind: "sql" });
      dbLoadConsoleLine(r.stmt);
      return;
    }
    const sum = at ? at.closest(".fl-sum") : null;
    const step = sum ? sum.closest<HTMLElement>("[data-hrun]") : null;
    const id = step ? step.dataset.hrun : undefined;
    if (!id) return;
    const r = runs.find((x) => String(x.id) === id);
    const opened = flowToggle(list, id, r ? runBody(r) : undefined);
    if (opened) open.add(id);
    else open.delete(id);
  });

  paint();
  showSheet(sheet({
    title: tr("dataHistory.title"),
    sub: first.target,
    wide: true,
    body: [hint(tr("dataHistory.intro")), search, list, earlier],
    foot: [clear, h("span", { class: "grow" }), close],
  }));
  search.focus();
}

export { dbHistoryDistinct, dbHistoryEnsure, dbHistoryOpen, dbHistoryPath, dbHistoryRecent, dbHistoryRefresh };
