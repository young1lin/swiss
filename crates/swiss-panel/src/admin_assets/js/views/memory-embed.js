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
   Memory - the semantic search sheet (SPEC §memory.embedding), opened from the projects rail.

   Search matches words until an embedding model is set; with one, the plugin embeds every live
   note in the background and a search matches on meaning too. The model is the llm plugin's
   (SPEC §llm.embedding) - the sheet shows the block in force read-only and links to its page.
   It says where the index stands, tests the model memory serves by, and re-embeds every note.
   ================================================================================================ */
import { toast } from "../util.js";
                                      
import { fill, h } from "../h.js";
import { tk, tr } from "../i18n.js";
import { btn, card, closeSheet, dot, hint, kvRow, sheet, showSheet, tag } from "../ui/index.js";
                                               
                                                                                                               
import { errMessage, req } from "./memory-core.js";

const STATE_DOT                                                = {
  off: "off", ready: "up", indexing: "starting", stale: "starting", error: "error",
};
const STATE_KEYS                                              = {
  off: tk("memory.embed.state.off"),
  ready: tk("memory.embed.state.ready"),
  indexing: tk("memory.embed.state.indexing"),
  stale: tk("memory.embed.state.stale"),
  error: tk("memory.embed.state.error"),
};

/** The index at a glance: state, fingerprint, how much of the store has a vector, and the last error. */
function statusNode(r                   )              {
  const s = r.status;
  const total = s.indexed + s.pending;
  const rows           = [
    kvRow(tr("memory.embed.state"), [dot(STATE_DOT[s.state], tr(STATE_KEYS[s.state])), " ", tr(STATE_KEYS[s.state])]),
  ];
  if (s.state !== "off") {
    rows.push(kvRow(tr("memory.embed.fingerprint"), s.model || "-", { mono: true, title: tr("memory.embed.fingerprintWhy") }));
    rows.push(kvRow(tr("memory.embed.vectors"), [
      tr("memory.embed.vectorsN", { indexed: s.indexed, total }),
      s.dims ? " · " + tr("memory.embed.dimsN", { n: s.dims }) : "",
      s.pending ? [" ", tag(tr("memory.embed.pendingN", { n: s.pending }), { tone: "warn" })] : null,
      s.incompatible ? [" ", tag(tr("memory.embed.incompatibleN", { n: s.incompatible }), { tone: "warn" })] : null,
    ]));
  }
  if (s.lastError) rows.push(kvRow(tr("memory.embed.lastError"), s.lastError));
  return h("div", { id: "memEmbStatus" },
    card(rows),
    r.configError ? hint(tr("memory.embed.configError", { error: r.configError }), { bad: true }) : null);
}

/** The block memory embeds with, read-only: it is the llm plugin's, edited on its page. */
function configNode(r                   )              {
  const c = r.config;
  if (c.provider === "off") return h("div", { id: "memEmbConfig" }, hint(tr("memory.embed.offHint")));
  return h("div", { id: "memEmbConfig" },
    card(
      kvRow(tr("memory.embed.provider"), [c.provider,
        r.remote ? [" ", tag(tr("memory.embed.remoteTag"), { tone: "warn", title: tr("memory.embed.remoteWhy") })] : null]),
      kvRow(tr("memory.embed.model"), c.model || "-", { mono: true }),
      c.dimensions ? kvRow(tr("memory.embed.dimensions"), String(c.dimensions)) : null),
    r.remote ? hint(tr("memory.embed.remoteNote"), { bad: true }) : null);
}

/** The semantic search sheet. Loads the embedding state first; a stopped plugin says so in a toast. */
export async function openEmbedding()                {
  const got = await req                   ("GET", "/embedding");
  if (!got.ok || !got.data) { toast(errMessage(got.err), true); return; }
  let reply = got.data;

  const status = h("div", null, statusNode(reply));
  const config = h("div", null, configNode(reply));
  const err = hint("", { id: "memEmbErr", bad: true, hidden: true, live: true });
  const result = hint("", { id: "memEmbResult", hidden: true, live: true });
  const test = btn(tr("memory.embed.test"), { id: "memEmbTest" });
  const reindex = btn(tr("memory.embed.reindex"), { id: "memEmbReindex" });
  const manage = btn(tr("memory.embed.manage"), { id: "memEmbManage", kind: "ghost" });
  const close = btn(tr("memory.close"), { kind: "primary" });

  const sync = ()       => {
    // Test and re-embed act on the provider in force; with none there is nothing to ask.
    const running = reply.status.state !== "off";
    test.disabled = !running;
    reindex.disabled = !running;
  };
  const say = (node             , text        )       => { node.textContent = text; node.hidden = !text; };

  showSheet(sheet({
    title: tr("memory.embed.title"),
    body: [hint(tr("memory.embed.intro")), status, config, err, result],
    foot: [test, reindex, h("span", { class: "grow" }), manage, close],
  }));
  sync();

  close.onclick = closeSheet;
  manage.onclick = ()       => {
    closeSheet();
    location.hash = "#llm-embedding";
  };
  const onTest = async ()                => {
    say(err, "");
    test.disabled = true;
    say(result, tr("memory.embed.testing"));
    const r = await req                  ("POST", "/embedding/test", {});
    test.disabled = false;
    if (!r.ok || !r.data) { say(result, ""); say(err, errMessage(r.err)); return; }
    const t = r.data;
    if (t.ok) say(result, tr("memory.embed.testOk", { model: t.model || "", dims: t.dims ?? 0, ms: t.ms ?? 0 }));
    else { say(result, ""); say(err, tr("memory.embed.testFail", { error: t.error || "" })); }
  };
  const onReindex = async ()                => {
    say(err, "");
    reindex.disabled = true;
    const r = await req                                                 ("POST", "/embedding/reindex", { drop: true });
    if (!r.ok || !r.data) { reindex.disabled = false; say(err, errMessage(r.err)); return; }
    say(result, tr("memory.embed.reindexed", { n: r.data.dropped }));
    reply = { ...reply, status: r.data.status };
    fill(status, statusNode(reply));
    sync();
  };
  test.onclick = () => { void onTest(); };
  reindex.onclick = () => { void onReindex(); };
  close.focus();
}

/** The list foot's word on the semantic tier after a search: nothing while it is off, a quiet
 *  "+ meaning" when it ran, a warning tag with the reason when it could not. */
export function semanticNode(s                         )         {
  if (!s || s.state === "off") return null;
  if (s.state === "on") return h("span", { data: { msemantic: "on" }, title: tr("memory.semantic.onTitle", { model: s.model || "" }) }, tr("memory.semantic.on"));
  return h("span", { data: { msemantic: "unavailable" } },
    tag(tr("memory.semantic.unavailable"), { tone: "warn", title: tr("memory.semantic.unavailableTitle", { reason: s.reason || "" }) }));
}
