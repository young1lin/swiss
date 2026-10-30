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

/* A unified diff drawn as DOM - git's patch split per file, each a fold with its path and its
 * counts, every line coloured by what it does and numbered on both sides (the old file's line,
 * then the new one's). The numbers are CSS from data attributes, so a drag-selection copies the
 * patch as written and never the numbers. Text only: every line lands in a text node. */
import { h } from "../h.js";
import { tr } from "../i18n.js";

export type DiffLineKind = "add" | "del" | "ctx" | "hunk" | "note";

export interface DiffLine {
  kind: DiffLineKind;
  /** The line as the patch has it, its +, - or space included. */
  text: string;
  /** The old file's line number (context and removed lines). */
  o?: number;
  /** The new file's line number (context and added lines). */
  n?: number;
}

export type DiffStatus = "added" | "deleted" | "modified" | "renamed" | "binary";

export interface DiffFile {
  path: string;
  /** A rename's old path. */
  from?: string;
  status: DiffStatus;
  additions: number;
  deletions: number;
  lines: DiffLine[];
}

const HUNK = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;
const HEADER = /^diff --git a\/(.*) b\/(.*)$/;

/** git's unified patch, one entry per `diff --git` section. Header lines (index, modes, the
 *  ---/+++ pair) set the path and status and are not lines of the file. */
export function diffFiles(patch: string): DiffFile[] {
  const files: DiffFile[] = [];
  let cur: DiffFile | null = null;
  let inHunk = false;
  let o = 0;
  let n = 0;
  for (const line of patch.split("\n")) {
    if (line.startsWith("diff --git ")) {
      const m = HEADER.exec(line);
      cur = { path: m ? m[2] : line.slice(11), status: "modified", additions: 0, deletions: 0, lines: [] };
      files.push(cur);
      inHunk = false;
      continue;
    }
    if (!cur) continue;
    const hunk = HUNK.exec(line);
    if (hunk) {
      o = Number(hunk[1]);
      n = Number(hunk[2]);
      inHunk = true;
      cur.lines.push({ kind: "hunk", text: line });
      continue;
    }
    if (!inHunk) {
      if (line.startsWith("new file mode")) cur.status = "added";
      else if (line.startsWith("deleted file mode")) cur.status = "deleted";
      else if (line.startsWith("rename from ")) { cur.from = line.slice(12); cur.status = "renamed"; }
      else if (line.startsWith("rename to ")) cur.path = line.slice(10);
      else if (line.startsWith("+++ b/")) cur.path = line.slice(6);
      else if (line.startsWith("Binary files ")) {
        if (cur.status === "modified") cur.status = "binary";
        cur.lines.push({ kind: "note", text: tr("ui.diff.binary") });
      }
      continue;
    }
    const sign = line.charAt(0);
    if (sign === "+") { cur.lines.push({ kind: "add", text: line, n: n++ }); cur.additions++; }
    else if (sign === "-") { cur.lines.push({ kind: "del", text: line, o: o++ }); cur.deletions++; }
    else if (sign === " ") cur.lines.push({ kind: "ctx", text: line, o: o++, n: n++ });
    else if (sign === "\\") cur.lines.push({ kind: "note", text: line });
  }
  return files;
}

/** A status as a word. The keys are literal at each call so the i18n completeness check sees them. */
export function diffStatusWord(s: DiffStatus): string {
  switch (s) {
    case "added": return tr("ui.diff.added");
    case "deleted": return tr("ui.diff.deleted");
    case "modified": return tr("ui.diff.modified");
    case "renamed": return tr("ui.diff.renamed");
    case "binary": return tr("ui.diff.binaryShort");
  }
}

function lineNode(l: DiffLine): HTMLElement {
  return h("span", {
    class: "dl " + l.kind,
    data: { o: l.o != null ? String(l.o) : "", n: l.n != null ? String(l.n) : "" },
  }, l.text + "\n");
}

/** A patch, a fold per file. `open` names the file to open (the one a caller came for); with
 *  none, every file of a small patch (up to five) is open and a larger one opens none. */
export function diffNode(patch: string, o: { open?: string | null } = {}): HTMLElement {
  const files = diffFiles(patch);
  const openAll = o.open == null && files.length <= 5;
  return h("div", { class: "diff" }, files.map((f) => {
    return h("details", { class: "diff-file", open: openAll || f.path === o.open, data: { dpath: f.path } },
      h("summary", null,
        h("span", { class: "diff-st " + f.status }, diffStatusWord(f.status)),
        h("code", { class: "diff-path" }, f.from ? f.from + " → " + f.path : f.path),
        h("span", { class: "diff-n" },
          h("span", { class: "plus" }, "+" + String(f.additions)), " ",
          h("span", { class: "minus" }, "−" + String(f.deletions)))),
      h("pre", { class: "jv diff-body" }, f.lines.map(lineNode)));
  }));
}
