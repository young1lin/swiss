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

import type { ApiDbColumn, ApiDbConnectionRow, ApiDbDataPage } from "../src/types/api.js";

/* Typed fixtures for the Data view's record (SPEC §panel.toolchain, slice 6).
 *
 * Until the db slice landed, the suite reached the record through util.js's `state`, which
 * the tests declare as `any` — so a row written as { name, dialect } type-checked against
 * nothing, and the wire shapes R3 wrote were never enforced on a single fixture. dbView()
 * is typed, so those partial literals became 17 errors the moment the record moved.
 *
 * The fix is these builders rather than seventeen `as unknown as` casts: a cast would put the
 * laundering back exactly where SPEC §panel.toolchain is taking it out, and it would keep the fixtures
 * free to drift from the wire. Each builder fills the fields a test does not care about with
 * neutral values and takes an override for the ones it does. */

export function dbConn(name: string, dialect: string, over: Partial<ApiDbConnectionRow> = {}): ApiDbConnectionRow {
  // label is deliberately empty: dbConnLabel builds the shown text from name + dialect, so a
  // fixture that filled it could disagree with the panel and nothing would notice.
  return { name: name, dialect: dialect, label: "", state: "ready", group: "default", ...over };
}

export function dbCol(name: string, over: Partial<ApiDbColumn> = {}): ApiDbColumn {
  return {
    name: name,
    dataType: "text",
    nullable: true,
    isPrimaryKey: false,
    defaultValue: null,
    comment: null,
    ...over,
  };
}

/** One page of rows as /api/db/:name/data answers it. The paging fields default to "this is
 *  the whole thing" — a test that cares about paging says so in the override. */
export function dbPage(over: Partial<ApiDbDataPage> = {}): ApiDbDataPage {
  return {
    schema: "",
    table: "t",
    columns: [],
    rows: [],
    total: 0,
    offset: 0,
    limit: 50,
    nextPage: false,
    primaryKey: [],
    editable: false,
    ...over,
  };
}
