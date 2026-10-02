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
// @vitest-environment happy-dom

import { afterAll, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { dbTab, unmountDbView } from "../src/db-state.js";
import { tr } from "../src/i18n.js";
import { TESTABLE_TYPES, TYPE_FIELDS, TYPE_LABELS } from "../src/fields.js";
import { dbConn } from "./db-fixtures.js";

/* SPEC §data.mongo-panel: the MongoDB workspace on the Data page, driven the way an operator
 * meets it - the real view mounted over the shell's skeleton, a fetch that answers like the
 * gateway's /api/db/{name}/mongo routes, and clicks that bubble to the pane's one delegated
 * listener. What is pinned: the sidebar lists one database's collections in bands, a
 * collection opens its own tab, the query bar sends canonical Extended JSON, documents read
 * in the shell's syntax, an edit is an optimistic replace whose conflict stays in the sheet,
 * and the console runs a command document. */

const here = dirname(fileURLToPath(import.meta.url));
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

document.body.innerHTML = shellSkeleton();
const view = await import("../src/data-view.js");
const tabs = await import("../src/data-tabs.js");
const mongo = await import("../src/data-mongo.js");

const OID = "65f0c0ffee00000000000001";
const DOCS = [
  { _id: { $oid: OID }, name: "Ada", qty: { $numberInt: "5" } },
  { _id: { $oid: "65f0c0ffee00000000000002" }, name: "Grace", qty: { $numberInt: "2" } },
  { _id: { $oid: "65f0c0ffee00000000000003" }, name: "Linus", qty: { $numberInt: "9" } },
];

type Sent = { url: string; method: string; body: unknown };
let sent: Sent[] = [];
/** One canned reply per route; a test overrides the ones it is about. */
let routes: Record<string, (body: unknown) => { status?: number; json: unknown }> = {};

function defaultRoutes(): typeof routes {
  return {
    "GET /api/db": () => ({ json: { connections: [dbConn("m", "mongo", { label: "shop @ 127.0.0.1:27017" })] } }),
    "GET /api/db/m/databases": () => ({
      json: {
        primary: null, current: "shop",
        databases: [
          { name: "shop", primary: true, browsable: true, system: false, size: 81920 },
          { name: "admin", primary: false, browsable: true, system: true, size: 4096 },
        ],
      },
    }),
    "GET /api/db/m/mongo/info": () => ({
      json: { version: "8.0.4", topology: "replicaSet", setName: "rs0", maxWireVersion: 25, transactions: true, allowDestructive: false, defaultDb: null },
    }),
    "GET /api/db/m/mongo/collections?db=shop": () => ({
      json: {
        db: "shop", statsOmitted: 0,
        collections: [
          { name: "orders", type: "collection", count: 3, size: 1024, indexes: 2 },
          { name: "open_orders", type: "view", viewOn: "orders" },
        ],
      },
    }),
    "POST /api/db/m/mongo/find": () => ({ json: { docs: DOCS, offset: 0, limit: 20, more: false, elapsedMs: 3 } }),
    "POST /api/db/m/mongo/count": () => ({ json: { total: 3, estimated: false } }),
    "POST /api/db/m/mongo/edits": () => ({ json: { ok: true, applied: 1, transaction: false } }),
    "POST /api/db/m/mongo/command": () => ({ json: { reply: { ok: { $numberDouble: "1.0" } }, elapsedMs: 1 } }),
  };
}

const realFetch = globalThis.fetch;
globalThis.fetch = ((url: string, init?: RequestInit): Promise<Response> => {
  const method = (init && init.method) || "GET";
  const body: unknown = init && typeof init.body === "string" ? JSON.parse(init.body) : null;
  sent.push({ url, method, body });
  const route = routes[method + " " + url];
  const r = route ? route(body) : { status: 404, json: { error: "no route " + url } };
  const status = r.status ?? 200;
  return Promise.resolve({ ok: status < 400, status, json: () => Promise.resolve(r.json) } as Response);
}) as typeof fetch;
afterAll(() => { globalThis.fetch = realFetch; });

const settle = async (): Promise<void> => { for (let i = 0; i < 12; i++) await new Promise((r) => { setTimeout(r, 0); }); };
const $ = (sel: string): HTMLElement | null => document.querySelector<HTMLElement>(sel);
const all = (sel: string): HTMLElement[] => Array.from(document.querySelectorAll<HTMLElement>(sel));
const lastTo = (path: string): Sent | undefined => sent.filter((s) => s.url.endsWith(path)).pop();
const click = (el: Element | null): void => {
  expect(el, "the control is on screen").toBeTruthy();
  el!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
};
function type(el: HTMLInputElement | HTMLTextAreaElement, text: string): void {
  el.value = text;
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

async function mountView(): Promise<void> {
  unmountDbView();
  document.body.innerHTML = shellSkeleton();
  sent = [];
  routes = defaultRoutes();
  await view.loadDbView();
  await settle();
}

async function openOrders(): Promise<void> {
  await mountView();
  click(all("#dbTables [data-mcoll]").find((b) => b.dataset.mcoll === "orders") ?? null);
  await settle();
}

beforeEach(() => { localStorage.clear(); });

describe("the sidebar (SPEC §data.mongo-panel)", () => {
  it("lists the current database's collections in Collections and Views bands", async () => {
    await mountView();
    expect(lastTo("/mongo/collections?db=shop"), "the tree asked for the primary database's collections").toBeTruthy();
    const rows = all("#dbTables [data-mcoll]").map((b) => b.dataset.mcoll);
    expect(rows).toEqual(["orders", "open_orders"]);
    const orders = $("#dbTables [data-mcoll=orders]")!;
    // The count rides the row's right edge, like a table's row estimate.
    expect(orders.textContent).toContain("3");
    expect($("#dbTablesPager")!.textContent).toBe("2 collections");
    expect($("#dbDatabaseRow")!.textContent).toContain("shop");
  });

  it("the search filters the names client-side, without asking the server again", async () => {
    await mountView();
    const before = sent.length;
    type($("#dbGrep") as HTMLInputElement, "open*");
    await new Promise((r) => { setTimeout(r, 350); });
    await settle();
    expect(all("#dbTables [data-mcoll]").map((b) => b.dataset.mcoll)).toEqual(["open_orders"]);
    expect(sent.slice(before).filter((s) => s.url.includes("/collections")), "no re-fetch for a name filter").toEqual([]);
  });

  it("a failed listing says so instead of an empty database, and a failed refresh keeps the rows", async () => {
    await mountView();
    routes["GET /api/db/m/mongo/collections?db=shop"] = () => ({ status: 500, json: { error: "not authorized" } });
    await mongo.mongoLoadCollections(true);
    await settle();
    expect(all("#dbTables [data-mcoll]").map((b) => b.dataset.mcoll), "a quiet refresh keeps what it had").toEqual(["orders", "open_orders"]);
    await mongo.mongoLoadCollections(false);
    await settle();
    expect(all("#dbTables [data-mcoll]")).toEqual([]);
    expect($("#dbTables")!.textContent).toContain(tr("dataMongo.listFailed"));
    expect($("#dbTables")!.textContent).not.toContain(tr("dataMongo.emptyDb"));
  });

  it("a mongo connection's strip falls back to an empty collection tab, and its console says Command", async () => {
    await mountView();
    expect(dbTab().kind).toBe("coll");
    expect($("#dbTabStrip")!.textContent).toContain(tr("dataTabs.command"));
  });
});

describe("a collection tab (SPEC §data.mongo-panel)", () => {
  it("opens on the collection, asks for the first page, and reads the documents in shell syntax", async () => {
    await openOrders();
    const t = dbTab();
    expect(t.kind === "coll" && t.db === "shop" && t.coll === "orders").toBe(true);
    expect(lastTo("/mongo/find")!.body).toEqual({ db: "shop", collection: "orders", filter: {}, skip: 0, limit: 20 });
    expect(all("#dbTabStrip .otab").some((c) => c.textContent?.includes("orders"))).toBe(true);
    const cards = all("#dbGridWrap .vblock");
    expect(cards).toHaveLength(3);
    expect(cards[0].textContent).toContain("ObjectId(\"" + OID + "\")");
    expect(cards[0].textContent).toContain("name: \"Ada\"");
    expect($("#dbStatus")!.textContent).toContain("1–3 of 3");
    expect($("#dbHead")!.textContent).toContain("orders");
    expect($("#dbHead")!.textContent).toContain("MongoDB 8.0.4");
  });

  it("Find sends the typed filter as canonical Extended JSON and starts from the first page", async () => {
    await openOrders();
    type($("[data-mq=filter]") as HTMLInputElement, "{ qty: { $gt: 1 }, at: ISODate(\"2024-01-01T00:00:00Z\") }");
    click($("[data-mfind]"));
    await settle();
    expect(lastTo("/mongo/find")!.body).toMatchObject({
      filter: { qty: { $gt: 1 }, at: { $date: { $numberLong: "1704067200000" } } },
      skip: 0,
    });
    // The count follows the same filter, so the status line's total is the filter's total.
    expect(lastTo("/mongo/count")!.body).toMatchObject({ filter: { qty: { $gt: 1 } } });
  });

  it("with Options open on a later page, a new filter's Find starts again from the first page", async () => {
    await openOrders();
    click($("[data-mopts]"));
    type($("[data-mq=skip]") as HTMLInputElement, "40");
    click($("[data-mfind]"));
    await settle();
    expect(lastTo("/mongo/find")!.body, "a skip the operator typed is honoured").toMatchObject({ skip: 40 });
    expect(($("[data-mq=skip]") as HTMLInputElement).value).toBe("40");
    type($("[data-mq=filter]") as HTMLInputElement, "{ qty: 1 }");
    click($("[data-mfind]"));
    await settle();
    expect(lastTo("/mongo/find")!.body).toMatchObject({ filter: { qty: 1 }, skip: 0 });
  });

  it("a filter that does not parse is said in the bar and never sent", async () => {
    await openOrders();
    const before = sent.filter((s) => s.url.endsWith("/mongo/find")).length;
    type($("[data-mq=filter]") as HTMLInputElement, "{ qty: }");
    click($("[data-mfind]"));
    await settle();
    expect(sent.filter((s) => s.url.endsWith("/mongo/find"))).toHaveLength(before);
    expect($("#dbFilters")!.textContent).toContain(tr("dataMongo.filter") + ":");
  });

  it("the table view lays the top-level fields out as columns", async () => {
    await openOrders();
    click($("[data-mview=table]"));
    await settle();
    const heads = all("#dbGridWrap table.db-grid th").map((th) => th.textContent);
    expect(heads).toEqual(["#", "_id", "name", "qty"]);
    expect(all("#dbGridWrap tbody tr")).toHaveLength(3);
  });

  it("the pane switch offers every workspace and keeps one primary action in the head", async () => {
    await openOrders();
    const panes = all("#dbHead [data-mpane]").map((b) => b.dataset.mpane);
    expect(panes).toEqual(["docs", "agg", "schema", "indexes", "explain", "validation"]);
    expect(all("#dbHead .btn:not(.icon)").map((b) => b.textContent)).toEqual([tr("dataMongo.addDocument")]);
    click($("[data-mpane=indexes]"));
    await settle();
    expect(all("#dbHead .btn:not(.icon)").map((b) => b.textContent)).toEqual([tr("dataMongo.addIndex")]);
  });
});

describe("editing a document (SPEC §data.mongo-edits)", () => {
  it("re-reads the document, and saves it as a replace that carries the read version", async () => {
    await openOrders();
    click($("[data-mact=edit][data-mdoc='0']"));
    await settle();
    expect(lastTo("/mongo/find")!.body).toMatchObject({ filter: { _id: { $oid: OID } }, limit: 1 });
    const ta = document.getElementById("mgText") as HTMLTextAreaElement;
    expect(ta.value).toContain("name: \"Ada\"");
    type(ta, ta.value.replace("\"Ada\"", "\"Ava\""));
    click(document.getElementById("mgSave"));
    await settle();
    const edit = lastTo("/mongo/edits")!.body as { db: string; collection: string; edits: { op: string; original: unknown; doc: Record<string, unknown> }[] };
    expect(edit.db).toBe("shop");
    expect(edit.collection).toBe("orders");
    expect(edit.edits).toHaveLength(1);
    expect(edit.edits[0].op).toBe("replace");
    expect(edit.edits[0].original).toEqual(DOCS[0]);
    // The type survives the round trip: qty stays an Int32, not a Double.
    expect(edit.edits[0].doc).toEqual({ _id: { $oid: OID }, name: "Ava", qty: 5 });
    expect((document.getElementById("sheet") as HTMLElement).hidden, "a saved edit closes the sheet").toBe(true);
  });

  it("a conflict keeps the sheet open with the operator's text and says why", async () => {
    await openOrders();
    routes["POST /api/db/m/mongo/edits"] = () => ({ status: 409, json: { error: "the document changed since it was read" } });
    click($("[data-mact=edit][data-mdoc='0']"));
    await settle();
    const ta = document.getElementById("mgText") as HTMLTextAreaElement;
    type(ta, ta.value.replace("\"Ada\"", "\"Ava\""));
    click(document.getElementById("mgSave"));
    await settle();
    expect((document.getElementById("sheet") as HTMLElement).hidden).toBe(false);
    expect(ta.value).toContain("\"Ava\"");
    const err = document.getElementById("mgErr")!;
    expect(err.hidden).toBe(false);
    expect(err.textContent).toContain("the document changed since it was read");
  });

  it("a save that changed nothing writes nothing", async () => {
    await openOrders();
    click($("[data-mact=edit][data-mdoc='0']"));
    await settle();
    click(document.getElementById("mgSave"));
    await settle();
    expect(lastTo("/mongo/edits")).toBeUndefined();
  });
});

describe("the analysis panes (SPEC §data.mongo-pipeline, §data.mongo-schema)", () => {
  it("the pipeline builder runs its stages as one aggregate, and a stage previews up to itself", async () => {
    await openOrders();
    routes["POST /api/db/m/mongo/aggregate"] = () => ({ json: { docs: [{ _id: "Ada", n: { $numberInt: "1" } }], more: false } });
    click($("[data-mpane=agg]"));
    await settle();
    click($("[data-maggadd]"));
    await settle();
    type($("[data-mstagebody='0']") as HTMLTextAreaElement, "{ qty: { $gte: 2 } }");
    click($("[data-maggadd]"));
    await settle();
    // The second stage starts as a $project: a builder that offered $match twice would be noise.
    expect(($("[data-mstageop='1']") as HTMLSelectElement).value).toBe("$project");
    click($("[data-mstagepreview='0']"));
    await settle();
    expect(lastTo("/mongo/aggregate")!.body).toMatchObject({ db: "shop", collection: "orders", pipeline: [{ $match: { qty: { $gte: 2 } } }] });
    click($("[data-maggrun]"));
    await settle();
    const run = lastTo("/mongo/aggregate")!.body as { pipeline: unknown[] };
    expect(run.pipeline).toEqual([{ $match: { qty: { $gte: 2 } } }, { $project: { field: 1 } }]);
    expect($("#dbGridWrap")!.textContent).toContain("_id: \"Ada\"");
  });

  it("a stage switched off is left out of the run", async () => {
    await openOrders();
    routes["POST /api/db/m/mongo/aggregate"] = () => ({ json: { docs: [], more: false } });
    click($("[data-mpane=agg]"));
    await settle();
    click($("[data-maggadd]"));
    click($("[data-maggadd]"));
    await settle();
    const off = $("[data-mstageon='1']") as HTMLInputElement;
    off.checked = false;
    off.dispatchEvent(new Event("change", { bubbles: true }));
    await settle();
    click($("[data-maggrun]"));
    await settle();
    expect((lastTo("/mongo/aggregate")!.body as { pipeline: unknown[] }).pipeline).toEqual([{ $match: {} }]);
  });

  it("Analyze samples the filtered documents and a top value filters the documents in one click", async () => {
    await openOrders();
    routes["POST /api/db/m/mongo/schema"] = () => ({
      json: {
        sampled: 3, paths: ["_id", "name", "qty"], elapsedMs: 4,
        fields: [{
          name: "name", path: "name", count: 3, missing: 0, probability: 1,
          types: [{ type: "String", count: 3, probability: 1, top: [{ value: "Ada", count: 2 }], distinct: 2 }],
        }],
      },
    });
    click($("[data-mpane=schema]"));
    await settle();
    click($("[data-mschemarun]"));
    await settle();
    expect(lastTo("/mongo/schema")!.body).toMatchObject({ db: "shop", collection: "orders", filter: {} });
    expect($("#dbGridWrap")!.textContent).toContain("name");
    click($("[data-mfpath=name]"));
    await settle();
    const t = dbTab();
    expect(t.kind === "coll" ? t.pane : null).toBe("docs");
    expect(lastTo("/mongo/find")!.body).toMatchObject({ filter: { name: "Ada" } });
  });

  it("Explain on the query bar's find shows the verdict: a collection scan is flagged", async () => {
    await openOrders();
    routes["POST /api/db/m/mongo/explain"] = () => ({
      json: {
        explain: {},
        summary: {
          collscan: true, inMemorySort: false, indexes: [], nReturned: 1, totalKeysExamined: 0, totalDocsExamined: 3,
          stages: [{ depth: 0, stage: "COLLSCAN", nReturned: 1, docsExamined: 3 }],
        },
      },
    });
    click($("[data-mpane=explain]"));
    await settle();
    type($("[data-mq=filter]") as HTMLInputElement, "{ name: \"Ada\" }");
    click($("[data-mfind]"));
    await settle();
    expect(lastTo("/mongo/explain")!.body).toMatchObject({ db: "shop", collection: "orders", filter: { name: "Ada" }, verbosity: "executionStats" });
    expect($("#dbGridWrap")!.textContent).toContain(tr("dataMongo.collscan"));
    expect($("#dbGridWrap")!.textContent).toContain("COLLSCAN");
  });

  it("the Indexes pane lists each index with its keys, its properties and its use", async () => {
    await openOrders();
    routes["GET /api/db/m/mongo/indexes?db=shop&collection=orders"] = () => ({
      json: {
        indexes: [
          { name: "_id_", key: { _id: { $numberInt: "1" } }, size: 4096, ops: 12 },
          { name: "name_1", key: { name: { $numberInt: "1" } }, unique: true, size: 2048, ops: 0 },
        ],
      },
    });
    click($("[data-mpane=indexes]"));
    await settle();
    const rows = all("#dbGridWrap tbody tr");
    expect(rows).toHaveLength(2);
    expect(rows[1].textContent).toContain("name_1");
    expect(rows[1].textContent).toContain(tr("dataMongo.ixUnique"));
  });
});

describe("the console (SPEC §data.mongo)", () => {
  it("runs one command document against the sidebar's database and prints the reply", async () => {
    await mountView();
    tabs.dbOpenTab({ kind: "sql" });
    await settle();
    expect($("#dbHead")!.textContent).toContain(tr("dataGrid.command"));
    type(document.getElementById("dbSql") as HTMLTextAreaElement, "{ ping: 1 }");
    click(document.getElementById("dbSqlRun"));
    await settle();
    expect(lastTo("/mongo/command")!.body).toEqual({ db: "shop", command: { ping: 1 } });
    expect($("#dbGridWrap pre.jv")!.textContent).toContain("ok: 1.0");
  });

  it("a command that does not parse is refused before it reaches the server", async () => {
    await mountView();
    tabs.dbOpenTab({ kind: "sql" });
    await settle();
    type(document.getElementById("dbSql") as HTMLTextAreaElement, "{ ping: }");
    click(document.getElementById("dbSqlRun"));
    await settle();
    expect(lastTo("/mongo/command")).toBeUndefined();
  });
});

describe("import parsing (SPEC §data.mongo)", () => {
  it("reads an array, one document, or NDJSON, in either syntax", () => {
    expect(mongo.mongoImportDocs("[{ a: 1 }, { \"a\": 2 }]")).toEqual([{ a: 1 }, { a: 2 }]);
    expect(mongo.mongoImportDocs("{ _id: ObjectId(\"" + OID + "\") }")).toEqual([{ _id: { $oid: OID } }]);
    expect(mongo.mongoImportDocs("{\"a\":1}\n\n{\"a\":2}\n")).toEqual([{ a: 1 }, { a: 2 }]);
  });

  it("names the line a broken NDJSON document is on", () => {
    expect(() => mongo.mongoImportDocs("{\"a\":1}\n{\"a\":}\n")).toThrow(/^Line 2:/);
  });

  it("batches under the per-request document cap", () => {
    const docs = Array.from({ length: 2500 }, (_, i) => ({ i }));
    expect(mongo.mongoImportBatches(docs).map((b) => b.length)).toEqual([1000, 1000, 500]);
  });

  it("weighs a batch in UTF-8 bytes, as sent - a CJK character is three", () => {
    // 600 000 code units together - far under the cap by length - but 900 KB of UTF-8 each:
    // two requests, never one 1.8 MB body.
    const docs = [{ s: "订".repeat(300000) }, { s: "单".repeat(300000) }];
    expect(mongo.mongoImportBatches(docs).map((b) => b.length)).toEqual([1, 1]);
    expect(mongo.mongoImportTooBig(docs)).toBe(-1);
    expect(mongo.mongoImportTooBig([{ a: 1 }, { s: "订".repeat(700000) }])).toBe(1);
  });

  it("names the download by the exact filename* the route sends", () => {
    const disp = "attachment; filename=\"app.__.json\"; filename*=UTF-8''app.%E8%AE%A2%E5%8D%95.json";
    expect(mongo.mongoDownloadName(disp, "x.json")).toBe("app.订单.json");
    expect(mongo.mongoDownloadName("attachment; filename=\"app.users.json\"", "x.json")).toBe("app.users.json");
    expect(mongo.mongoDownloadName("attachment; filename=\"a.json\"; filename*=UTF-8''%E8%ZZ", "x.json")).toBe("a.json");
    expect(mongo.mongoDownloadName("", "x.json")).toBe("x.json");
  });
});

describe("the MCP form (SPEC §mcp.db)", () => {
  it("a mongo def is a connection string, an optional database, a page size and the destructive switch", () => {
    expect(TYPE_FIELDS.mongo.map((f) => f.k)).toEqual(["description", "url", "database", "maxRows", "allowDestructive", "autostart"]);
    expect(TESTABLE_TYPES).toContain("mongo");
    expect(tr(TYPE_LABELS.mongo)).toBe("mongo — in-process driver");
  });
});
