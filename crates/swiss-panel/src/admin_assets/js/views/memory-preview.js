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

/* ================================================================================================
   Memory page - previews (SPEC §memory.preview): what a note points at, read in place. An
   anchor opens the file as it is now (numbered lines, a Markdown file drawn, its source one
   toggle away), a directory as its listing, a glob as the files it matches; the History tab is
   the commits that touched the path - the ones after the note was last saved or verified are
   tagged, they are what may have made it wrong - and its uncommitted change. A commit ref opens
   the commit: its message, its files and the coloured patch. A `path: heading` ref opens the
   file at that heading.

   A preview is a wide sheet; one opened from a preview (a commit from History) stacks over it,
   so Escape walks back one layer at a time. Each sheet owns its clicks - the page's delegated
   listener never sees a sheet.
   ================================================================================================ */
                                      
import { h } from "../h.js";
import { tr, trn } from "../i18n.js";
import {
  btn, card, closeSheet, diffNode, diffStatusWord, hint, iconBtn, iconNode, markdownNode, note, refLink, relTime, row, seg, sheet,
  sheetOpen, showSheet, stackSheet, tag, tokenCodeNode, valueBlock,
} from "../ui/index.js";
                                                
             
                                                                            
                            
                                              
import { S, errMessage, fullTime, qs, req, sizeText } from "./memory-core.js";
import { toast } from "../util.js";

/** The server's caps (swiss-plugins' memory_preview.rs), quoted when a preview is cut. */
const FILE_LINES_MAX = 5000;
const FILE_KIB_MAX = 512;
const PATCH_KIB_MAX = 256;

/* ------------------------------------------------------------------ what a ref points at */

                       
                      
                                   
                    

const COMMIT_REF = /^(?:commit\s+)?([0-9a-f]{7,64})\b/i;
const URL_REF = /^https?:\/\/\S+$/i;
/** A repo-relative path - a slash or an extension, no blanks - and an optional `: heading`. */
const PATH_REF = /^((?:[\w.@-]+\/)*[\w.@-]*(?:\/|\.[\w-]+))(?::\s*(.+))?$/;

/** A ref as a place: `commit abc1234` (or a bare hash with a letter in it), a URL, or
 *  `path[: heading]`. Anything else - "ADR-019", "PR #12" - is only text. */
export function refTarget(ref        )                   {
  const text = ref.trim();
  const commit = COMMIT_REF.exec(text);
  if (commit && (/^commit\s/i.test(text) || /[a-f]/i.test(commit[1]))) return { commit: commit[1].toLowerCase() };
  if (URL_REF.test(text)) return { url: text };
  const path = PATH_REF.exec(text);
  if (path && !path[1].startsWith("/")) return path[2] ? { path: path[1].replace(/\/$/, ""), find: path[2].trim() } : { path: path[1].replace(/\/$/, "") };
  return null;
}

/* ------------------------------------------------------------------ layers */

;                
                                       
                    
                                                                                   
                                                                                       
                                                                                       
                                                                                      
                                                                                           
                            
 

/** The painted-node liveness probe one layer shares: null before the first paint
 *  (nothing to have lost yet), the node's isConnected after it. */
function nodeProbe()                                                          {
  let painted                     = null;
  return { note: (n) => { painted = n; }, live: () => painted === null || painted.isConnected };
}

/** The page's sheet when none is open, else one stacked over the open one. */
function layer()        {
  if (sheetOpen()) {
    const stack = stackSheet();
    const probe = nodeProbe();
    return {
      paint: (o) => { const node = stack.paint(o); probe.note(node); return node; },
      close: stack.close,
      stillShown: probe.live,
    };
  }
  const probe = nodeProbe();
  return {
    paint: (o) => { const node = sheet(o); showSheet(node); probe.note(node); return node; },
    close: closeSheet,
    stillShown: probe.live,
  };
}

function closeBtn()              {
  return btn(tr("memory.close"), { data: { mpvclose: "" } });
}

function baseName(path        )         {
  const at = path.lastIndexOf("/");
  return at < 0 ? path : path.slice(at + 1);
}

function isMarkdown(path        )          {
  return /\.(md|markdown)$/i.test(path);
}

/** The line a ref's heading names: a Markdown heading that contains it first, then any line. */
export function findLine(text        , find        , markdown         )                {
  const lines = text.split("\n");
  const needle = find.toLowerCase();
  if (markdown) {
    const at = lines.findIndex((l) => /^ {0,3}#{1,6}\s/.test(l) && l.toLowerCase().includes(needle));
    if (at >= 0) return at + 1;
  }
  const at = lines.findIndex((l) => l.toLowerCase().includes(needle));
  return at >= 0 ? at + 1 : null;
}

/* ------------------------------------------------------------------ the file sheet */

;                         
                                                      
               
                                                                    
                 
                           
                                              
                
                                                                               
                        
 

/** A file, a directory or a glob's matches, with the path's history one tab away. A directory
 *  entry or a matched file opens in the same sheet; Back walks back. */
export function openFile(ask         )       {
  const project = S.project;
  const top = layer();
  const trail            = [];
  let cur          = ask;
  let tab                     = ask.tab || "file";
  let source = false;
  let file                               = null;
  let globbed                             = null;
  let log                           = null;
  /** The find scrolls once per path, not on every repaint. */
  let scrolled = false;

  const title = ()         => cur.glob ? cur.path : cur.path ? baseName(cur.path) : tr("memory.pv.root");

  const fileBody = ()         => {
    if (cur.glob) {
      if (!globbed) return hint(tr("memory.pv.loading"));
      if (!globbed.ok || !globbed.data) return hint(errMessage(globbed.err), { bad: true });
      const g = globbed.data;
      if (!g.files.length) return hint(tr("memory.pv.globNone", { glob: g.glob }));
      return [
        hint([trn(g.files.length, "memory.pv.globN.one", "memory.pv.globN.other"),
          g.truncated ? " " + tr("memory.pv.globTruncated", { n: g.files.length }) : "",
          g.git ? "" : " " + tr("memory.pv.globNoGit")]),
        card(g.files.map((f) => row({
          lead: iconNode("file"),
          name: refLink(f, { code: true, data: { mpvopen: f } }),
        }))),
      ];
    }
    if (!file) return hint(tr("memory.pv.loading"));
    if (!file.ok || !file.data) return hint(errMessage(file.err), { bad: true });
    const f = file.data;
    if (f.kind === "dir") {
      if (!f.entries.length) return hint(tr("memory.pv.dirEmpty"));
      return [
        f.truncated ? hint(tr("memory.pv.dirTruncated", { n: f.entries.length })) : null,
        card(f.entries.map((e) => {
          const child = f.path ? f.path + "/" + e.name : e.name;
          return row({
            lead: iconNode(e.dir ? "folder" : "file"),
            name: e.sensitive ? h("code", null, e.name) : refLink(e.name, { code: true, data: { mpvopen: child } }),
            cols: [
              e.sensitive ? tag(tr("memory.pv.notShown"), { title: tr("memory.pv.sensitiveWhy") }) : null,
              e.size != null ? { v: sizeText(e.size), w: "s"          } : null,
            ],
            muted: e.sensitive,
          });
        })),
      ];
    }
    const notes = [trn(f.lines, "memory.pv.lines.one", "memory.pv.lines.other"), sizeText(f.size)];
    if (f.modifiedMs) notes.push(tr("memory.pv.modified", { when: relTime(f.modifiedMs) }));
    if (f.binary || f.text == null) return note(tr("memory.pv.binary", { size: sizeText(f.size) }));
    const md = isMarkdown(f.path);
    const mark = cur.find ? findLine(f.text, cur.find, md) : null;
    const body = !f.text
      ? hint(tr("memory.pv.empty"))
      : md && !source
        ? markdownNode(f.text)
        : tokenCodeNode([["", f.text]], { all: true, lined: true, mark: mark ?? undefined }).node;
    return [
      cur.find && mark == null ? hint(tr("memory.pv.findNone", { find: cur.find })) : null,
      valueBlock({
        label: tr("memory.pv.contents"), notes,
        tools: [
          md ? iconBtn("braces", tr("memory.agent.source"), { ghost: true, pressed: source, data: { mpvsource: "" } }) : null,
          iconBtn("copy", tr("memory.pv.copy"), { ghost: true, data: { mpvcopy: "" } }),
        ].filter((b)                         => b != null),
      }, body),
      f.truncated ? hint(tr("memory.pv.truncated", { lines: FILE_LINES_MAX, kib: FILE_KIB_MAX })) : null,
    ];
  };

  const historyBody = ()         => {
    if (!log) return hint(tr("memory.pv.loading"));
    if (!log.ok || !log.data) return hint(errMessage(log.err), { bad: true });
    const l = log.data;
    const since = cur.since ?? null;
    const head           = [];
    if (l.untracked) head.push(row({ lead: iconNode("pencil"), name: tr("memory.pv.untracked") }));
    else if (l.dirty) {
      head.push(row({
        lead: iconNode("pencil"),
        name: refLink(tr("memory.pv.uncommitted"), { data: { mpvdiff: l.path } }),
        sub: tr("memory.pv.uncommittedSub"),
      }));
    }
    const rows = l.commits.map((c) => {
      const after = since != null && c.dateMs > since;
      return row({
        lead: iconNode("history"),
        name: refLink(c.subject, { data: { mpvcommit: c.hash } }),
        sub: [h("code", null, c.short), " · ", c.author, " · ", h("span", { title: fullTime(c.dateMs) }, relTime(c.dateMs))],
        cols: after ? [tag(tr("memory.pv.afterNote"), { tone: "warn", title: tr("memory.pv.afterNoteWhy") })] : [],
      });
    });
    return [
      head.length || rows.length ? card(head, rows) : hint(tr("memory.pv.noCommits")),
      l.more ? hint(tr("memory.pv.more", { n: l.commits.length })) : null,
    ];
  };

  const paint = ()       => {
    const tabs = cur.glob ? null : seg([
      { id: "file", label: file?.data?.kind === "dir" ? tr("memory.pv.listing") : tr("memory.pv.file") },
      { id: "history", label: tr("memory.pv.history"), n: log?.data ? log.data.commits.length + (log.data.more ? "+" : "") : null },
    ], tab, { key: "mpvtab", label: tr("memory.pv.tabs") });
    const node = top.paint({
      title: title(),
      sub: cur.path || undefined,
      body: [tabs, tab === "history" && !cur.glob ? historyBody() : fileBody()],
      foot: [
        trail.length ? btn(tr("memory.pv.back"), { kind: "ghost", icon: "chevron-left", data: { mpvback: "" } }) : null,
        h("span", { class: "grow" }),
        closeBtn(),
      ],
      wide: true,
    });
    node.onclick = (e) => { void onClick(e); };
    if (!scrolled && tab === "file") {
      const hit = node.querySelector             (".jv-line.hit") || headingFor(node, cur.find);
      if (hit) { scrolled = true; hit.scrollIntoView({ block: "start" }); }
    }
  };

  const load = async ()                => {
    const want = cur;
    if (want.glob) {
      globbed = null;
      paint();
      globbed = await req              ("GET", "/preview/files" + qs({ project, glob: want.path }));
    } else if (tab === "history") {
      if (!log) {
        paint();
        log = await req            ("GET", "/preview/log" + qs({ project, path: want.path }));
      }
    } else if (!file) {
      paint();
      file = await req                ("GET", "/preview/file" + qs({ project, path: want.path }));
    }
    // The reply may land after the user closed this preview, replaced it with
    // another sheet, or navigated away: a repaint then would reopen the sheet over
    // whatever they moved on to.
    if (want === cur && top.stillShown()) paint();
  };

  const go = (next         )       => {
    trail.push(cur);
    cur = next;
    tab = next.tab || "file";
    file = null; globbed = null; log = null; source = false; scrolled = false;
    void load();
  };

  const onClick = async (e            )                => {
    const t = e.target instanceof Element ? e.target : null;
    if (!t) return;
    const at = (sel        )                     => t.closest             (sel);
    if (at("[data-mpvclose]")) { top.close(); return; }
    const tabBtn = at("[data-mpvtab]");
    if (tabBtn) {
      tab = tabBtn.dataset.mpvtab === "history" ? "history" : "file";
      await load();
      return;
    }
    if (at("[data-mpvsource]")) { source = !source; paint(); return; }
    if (at("[data-mpvcopy]")) {
      const text = file?.data?.kind === "file" ? file.data.text : null;
      if (text == null) return;
      try {
        await navigator.clipboard.writeText(text);
        toast(tr("memory.pv.copied"));
      } catch (err) {
        toast(tr("memory.agent.copyFailed"), true);
      }
      return;
    }
    if (at("[data-mpvback]")) {
      const prev = trail.pop();
      if (!prev) return;
      cur = prev;
      tab = prev.tab || "file";
      file = null; globbed = null; log = null; source = false; scrolled = true;
      await load();
      return;
    }
    const open = at("[data-mpvopen]");
    if (open) { go({ path: open.dataset.mpvopen || "", since: cur.since }); return; }
    const commit = at("[data-mpvcommit]");
    if (commit) { openCommit(commit.dataset.mpvcommit || "", { path: cur.glob ? undefined : cur.path || undefined }); return; }
    const diff = at("[data-mpvdiff]");
    if (diff) openWorking(diff.dataset.mpvdiff || "");
  };

  void load();
}

/** A rendered Markdown file's heading that contains `find`. */
function headingFor(node             , find                    )                     {
  if (!find) return null;
  const needle = find.toLowerCase();
  const heads = Array.from(node.querySelectorAll             (".md :is(h1, h2, h3, h4, h5, h6)"));
  return heads.find((x) => (x.textContent || "").toLowerCase().includes(needle)) || null;
}

/* ------------------------------------------------------------------ the commit sheet */

/** git's status letter as a word; a letter this does not know stays the letter. */
function changeWord(letter        )         {
  switch (letter) {
    case "A": return diffStatusWord("added");
    case "D": return diffStatusWord("deleted");
    case "M": return diffStatusWord("modified");
    case "R": return diffStatusWord("renamed");
    case "T": return tr("memory.pv.typeChanged");
    default: return letter;
  }
}

/** One commit: who and when, its message, the files it changed and the patch. Opened for a
 *  path (from a file's History), the patch is that file's alone until Show every file. */
export function openCommit(rev        , o                    = {})       {
  const project = S.project;
  const top = layer();
  let only = o.path || null;
  let got                                = null;
  let shown                     = null;

  const body = ()         => {
    if (!got) return hint(tr("memory.pv.loading"));
    if (!got.ok || !got.data) return hint(errMessage(got.err), { bad: true });
    const c = got.data;
    const merge = c.parents.length > 1 ? tr("memory.pv.merge", { parents: c.parents.map((p) => p.slice(0, 7)).join(", ") }) : null;
    return [
      hint([c.author, " · ", h("span", { title: fullTime(c.dateMs) }, fullTime(c.dateMs)), merge ? " · " + merge : null]),
      c.body ? valueBlock({ label: tr("memory.pv.message"), text: c.body }) : null,
      valueBlock({ label: trn(c.files.length, "memory.pv.filesN.one", "memory.pv.filesN.other") },
        card(c.files.map((f) => row({
          name: refLink(f.path, { code: true, data: { mpvjump: f.path } }),
          cols: [
            tag(changeWord(f.status)),
            { v: f.additions == null ? tr("memory.pv.binaryShort") : "+" + String(f.additions) + " −" + String(f.deletions), w: "s", mono: true },
          ],
        })))),
      c.path ? note([tr("memory.pv.onlyPath", { path: c.path }), " ", btn(tr("memory.pv.allFiles"), { kind: "ghost", data: { mpvall: "" } })]) : null,
      c.patch ? diffNode(c.patch, { open: c.path ?? null }) : hint(tr("memory.pv.noPatch")),
      c.truncated ? hint(tr("memory.pv.patchTruncated", { kib: PATCH_KIB_MAX })) : null,
    ];
  };

  const paint = ()       => {
    const c = got?.data;
    const node = shown = top.paint({
      title: c ? c.subject : tr("memory.pv.commit"),
      label: tr("memory.pv.commit"),
      sub: c ? c.short : rev.slice(0, 7),
      body: body(),
      foot: [h("span", { class: "grow" }), closeBtn()],
      wide: true,
    });
    node.onclick = (e) => { void onClick(e); };
  };

  const load = async ()                => {
    got = null;
    paint();
    got = await req                 ("GET", "/preview/commit" + qs({ project, rev, path: only }));
    if (!top.stillShown()) return; // the sheet closed or was replaced under the reply
    paint();
  };

  const onClick = async (e            )                => {
    const t = e.target instanceof Element ? e.target : null;
    if (!t) return;
    if (t.closest("[data-mpvclose]")) { top.close(); return; }
    if (t.closest("[data-mpvall]")) { only = null; await load(); return; }
    const jump = t.closest             ("[data-mpvjump]");
    if (jump) {
      const path = jump.dataset.mpvjump || "";
      if (only && only !== path) { only = null; await load(); }
      const fold = Array.from(shown?.querySelectorAll                    (".diff-file") || []).find((d) => d.dataset.dpath === path);
      if (fold) { fold.open = true; fold.scrollIntoView({ block: "start" }); }
    }
  };

  void load();
}

/* ------------------------------------------------------------------ the uncommitted change */

export function openWorking(path        )       {
  const project = S.project;
  const top = layer();
  let got                               = null;
  const paint = ()       => {
    let body        ;
    if (!got) body = hint(tr("memory.pv.loading"));
    else if (!got.ok || !got.data) body = hint(errMessage(got.err), { bad: true });
    else if (got.data.untracked) body = hint(tr("memory.pv.untracked"));
    else if (!got.data.patch) body = hint(tr("memory.pv.noChanges"));
    else {
      body = [diffNode(got.data.patch, { open: null }),
        got.data.truncated ? hint(tr("memory.pv.patchTruncated", { kib: PATCH_KIB_MAX })) : null];
    }
    const node = top.paint({
      title: tr("memory.pv.uncommitted"),
      sub: path || undefined,
      body: [hint(tr("memory.pv.uncommittedSub")), body],
      foot: [h("span", { class: "grow" }), closeBtn()],
      wide: true,
    });
    node.onclick = (e) => {
      const t = e.target instanceof Element ? e.target : null;
      if (t && t.closest("[data-mpvclose]")) top.close();
    };
  };
  paint();
  void req                ("GET", "/preview/diff" + qs({ project, path })).then((r) => {
    got = r;
    if (top.stillShown()) paint(); // a closed or replaced sheet stays that way
  });
}
