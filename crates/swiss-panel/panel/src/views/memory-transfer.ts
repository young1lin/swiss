/* Copyright 2026 young1lin
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

/** ==============================================================================================
 * Memory - export and import (SPEC §memory.panel): a project's notes, or one domain's, out as
 * the JSONL the CLI writes; a file in as a knowledge PR - Check first, a dry run that says where
 * the file came from, what it adds and which of its notes this project holds differently, then
 * Import. Opened from the project menu, which memory.ts places - this module only draws and runs
 * the two sheets.
 * ================================================================================================ */
import { fill, h } from "../h.js";
import type { HChild } from "../h.js";
import { tr } from "../i18n.js";
import { api, toast } from "../util.js";
import { btn, card, closeSheet, field, form, hint, row, sheet, showSheet, tag } from "../ui/index.js";
import type { MemErrorBody, MemExportHeader, MemImportConflict, MemImportReport, MemProject } from "../types/memory.js";
import { domainList, domainSelect, errMessage, fullTime, hooks, qs, req } from "./memory-core.js";

/** The gateway's request body limit: the file travels as a JSON string inside it. */
const BODY_MAX_BYTES = 2 * 1024 * 1024;

/** The file name a download's Content-Disposition gives, else the fallback. Pure. */
export function downloadName(disposition: string, fallback: string): string {
  const m = /filename="([^"]+)"/.exec(disposition);
  return m ? m[1] : fallback;
}

/** The export sheet: the whole project or one domain (with the domains under it). */
export async function openExport(p: MemProject): Promise<void> {
  const items = await domainList();
  const what = domainSelect({ id: "memXferWhat", label: tr("memory.xfer.what") }, items, "", [["", tr("memory.xfer.whole")]]);
  const err = hint("", { id: "memXferErr", bad: true, hidden: true, live: true });
  const go = btn(tr("memory.xfer.exportGo"), { id: "memXferExport", kind: "primary" });
  const cancel = btn(tr("memory.close"));
  showSheet(sheet({
    title: tr("memory.xfer.exportTitle"), sub: p.name,
    body: form(field({ label: tr("memory.xfer.what"), control: what, hint: tr("memory.xfer.exportWhy") }), err),
    foot: [h("span", { class: "grow" }), cancel, go],
  }));
  cancel.onclick = closeSheet;
  go.onclick = async (): Promise<void> => {
    err.hidden = true;
    go.disabled = true;
    try {
      const r = await api("/api/knowledge/export" + qs({ project: p.id, domain: what.value }), { method: "GET" });
      if (!r.ok) {
        const j: unknown = await r.json().catch((): unknown => null);
        err.textContent = errMessage(j && typeof j === "object" && "error" in j ? j as MemErrorBody : null);
        err.hidden = false;
        return;
      }
      const name = downloadName(r.headers.get("Content-Disposition") || "", "memory-" + p.id + ".jsonl");
      const a = document.createElement("a");
      a.href = URL.createObjectURL(await r.blob());
      a.download = name;
      document.body.appendChild(a);
      a.click();
      a.remove();
      setTimeout((): void => { URL.revokeObjectURL(a.href); }, 1000);
      closeSheet();
      toast(tr("memory.xfer.exported", { name }));
    } catch {
      err.textContent = tr("memory.xfer.exportFailed");
      err.hidden = false;
    } finally {
      go.disabled = false;
    }
  };
}

/** Where a file came from, in one line; an older build's bare header says only its format. */
export function sourceText(s: MemExportHeader): string {
  const parts: string[] = [];
  if (s.project && s.domain) parts.push(tr("memory.xfer.fromDomain", { domain: s.domain, name: s.project.name }));
  else if (s.project) parts.push(tr("memory.xfer.fromProject", { name: s.project.name }));
  else if (s.scope === "all") parts.push(tr("memory.xfer.fromAll"));
  else return tr("memory.xfer.fromOld", { n: s.formatVersion });
  if (s.visibility === "global") parts.push(tr("memory.xfer.readByGlobal"));
  if (s.notes != null) parts.push(tr("memory.xfer.notes", { n: s.notes, confirmed: s.confirmed ?? 0 }));
  if (s.linksLeftOut) parts.push(tr("memory.xfer.leftOut", { n: s.linksLeftOut }));
  if (s.exportedMs) parts.push(tr("memory.xfer.exportedAt", { at: fullTime(s.exportedMs) }));
  return parts.join(" · ");
}

function conflictRow(c: MemImportConflict): HTMLElement {
  const why = c.kind === "revision"
    ? tr("memory.xfer.revisionWhy", { file: c.incoming.revision, here: c.stored.revision })
    : tr("memory.xfer.titleWhy", { id: c.stored.id });
  return row({
    lead: tag(tr(c.kind === "revision" ? "memory.xfer.kindRevision" : "memory.xfer.kindTitle")),
    name: c.incoming.title,
    sub: [why, " · ", h("code", null, c.incoming.id)],
    meta: [
      h("div", { class: "quiet" }, tr("memory.xfer.inFile"), ": ", c.incoming.excerpt),
      h("div", { class: "quiet" }, tr("memory.xfer.here"), ": ", c.stored.excerpt),
    ],
    data: { mxconflict: c.incoming.id },
  });
}

/** The report a dry run or an import answers: the source, the counts, the conflicts. */
export function reportNode(r: MemImportReport): HChild {
  const counts = { notes: r.recordsCreated, domains: r.domainsCreated, links: r.linksCreated, same: r.recordsSkipped.length };
  return [
    h("p", { id: "memXferSource" }, h("b", null, tr("memory.xfer.from")), " ", sourceText(r.source)),
    h("p", { id: "memXferCounts" }, tr(r.dryRun ? "memory.xfer.wouldWrite" : "memory.xfer.wrote", counts)),
    r.conflicts.length
      ? [hint(tr("memory.xfer.conflicts")), h("div", { id: "memXferConflicts" }, card(r.conflicts.map(conflictRow)))]
      : hint(tr("memory.xfer.noConflicts")),
  ];
}

/** The import sheet: a file, Check (nothing written), then Import into this project. */
export function openImport(p: MemProject): void {
  const file = h("input", { type: "file", id: "memXferFile", accept: ".jsonl,.ndjson,.txt,application/x-ndjson,text/plain", style: "width:auto" }) as HTMLInputElement;
  const err = hint("", { id: "memXferErr", bad: true, hidden: true, live: true });
  const report = h("div", { id: "memXferReport" });
  const check = btn(tr("memory.xfer.check"), { id: "memXferCheck", kind: "primary" });
  const go = btn(tr("memory.xfer.importGo"), { id: "memXferImport", kind: "primary", hidden: true });
  const cancel = btn(tr("memory.close"));
  showSheet(sheet({
    title: tr("memory.xfer.importTitle"), sub: p.name, wide: true,
    body: form(field({ label: tr("memory.xfer.file"), control: file, hint: tr("memory.xfer.importWhy") }), err, report),
    foot: [h("span", { class: "grow" }), cancel, check, go],
  }));
  let text = "";
  const say = (msg: string): void => { err.textContent = msg; err.hidden = false; };
  // A new file is checked again before anything is written.
  file.onchange = (): void => {
    text = "";
    err.hidden = true;
    fill(report);
    check.hidden = false;
    go.hidden = true;
    const f = file.files && file.files[0];
    if (!f) return;
    f.text().then((t: string): void => { text = t; }, (): void => { say(tr("memory.xfer.readFailed")); });
  };
  const run = async (dryRun: boolean): Promise<MemImportReport | null> => {
    err.hidden = true;
    if (!text.trim()) { say(tr("memory.xfer.noFile")); return null; }
    const body = { project: p.id, text, dryRun };
    if (new TextEncoder().encode(JSON.stringify(body)).length > BODY_MAX_BYTES) { say(tr("memory.xfer.tooBig")); return null; }
    const r = await req<MemImportReport>("POST", "/import", body);
    if (!r.ok || !r.data) { say(errMessage(r.err)); return null; }
    return r.data;
  };
  cancel.onclick = closeSheet;
  check.onclick = async (): Promise<void> => {
    check.disabled = true;
    try {
      const r = await run(true);
      if (!r) return;
      fill(report, reportNode(r));
      check.hidden = true;
      go.hidden = false;
    } finally {
      check.disabled = false;
    }
  };
  go.onclick = async (): Promise<void> => {
    go.disabled = true;
    try {
      const r = await run(false);
      if (!r) return;
      closeSheet();
      toast(tr("memory.xfer.imported", { notes: r.recordsCreated, conflicts: r.conflicts.length }));
      await hooks.reload();
    } finally {
      go.disabled = false;
    }
  };
}
