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
   LLM Embedding - the llm plugin's second page (#llm-embedding), SPEC §llm.embedding.

   The one embedding model every consumer shares: a provider from the Providers page (an
   OpenAI-style one - POST {baseUrl}/embeddings; the Anthropic wire has no embeddings), one of
   the embedding models it lists, and the knobs a model family wants (dimensions, prefixes,
   the similarity floor).
   Memory embeds its notes with it; a change of model or dimensions is a new fingerprint, and
   memory re-embeds on its own.

   The form is the row's block as stored; the card beside it is the block in force, defaults
   filled. Test asks the SAVED block, so it refuses while the form holds an unsaved edit.
   ================================================================================================ */
import { $, toast } from "../util.js";
import { followLlmConfig } from "../llm-live.js";
                                      
import { fill, h } from "../h.js";
import { tr } from "../i18n.js";
import {
  btn, card, failNote, field, form, formActions, hint, kvRow, note, pair, paneBody, paneHead, section, split, tag,
} from "../ui/index.js";
                                                                                          
import { call, isError, loadRow, loadStatus, modelsOf, putRow, remoteHost, testNode, wholeField } from "./llm-core.js";

const DIMS_MAX = 8192;
const BATCH_MAX = 256;
const TIMEOUT_MIN_MS = 1000;
const TIMEOUT_MAX_MS = 120_000;
const PREFIX_MAX = 200;

let status = null                    ;
let stored                    = { provider: "off" }; // the row's block: the form's baseline
let painted = "";
let visit = 0; // bumped by mount and unmount: a late answer from an ended visit paints nothing
let readForm                                            = null;

/** A block as the server reads it: empty fields dropped, keys in one order - two forms that
 *  say the same thing compare equal. */
function canon(b                   )         {
  return JSON.stringify(Object.entries(b).filter(([, v]) => v !== "" && v != null).sort(([a], [x]) => a.localeCompare(x)));
}

function input(id        , value                             , placeholder = "")                   {
  return h("input", { id, autocomplete: "off", spellcheck: false, value: value == null ? "" : String(value), placeholder });
}

/** The block in force: what memory embeds with right now. */
function inForceNode(s           )              {
  const e = s.embedding;
  if (!e) return h("div", { id: "leInForce" }, note(tr("llm.embedding.offNote")));
  const p = s.providers.find((x) => x.name === e.provider);
  const rows           = [
    kvRow(tr("llm.embedding.provider"), [e.provider, p ? [" · ", h("code", null, p.baseUrl)] : null,
      e.remote ? [" ", tag(tr("llm.embedding.remoteTag"), { tone: "warn", title: tr("llm.embedding.remoteWhy") })] : null]),
    kvRow(tr("llm.embedding.model"), e.model, { mono: true }),
    kvRow(tr("llm.embedding.dimensions"), e.dimensions ? String(e.dimensions) : tr("llm.embedding.dimsNative")),
    kvRow(tr("llm.embedding.batchTimeout"), tr("llm.embedding.batchTimeoutV", { batch: e.batch, ms: e.timeoutMs })),
  ];
  if (e.queryPrefix || e.documentPrefix) {
    rows.push(kvRow(tr("llm.embedding.prefixes"), JSON.stringify(e.queryPrefix) + " · " + JSON.stringify(e.documentPrefix), { mono: true }));
  }
  if (e.minSimilarity != null) rows.push(kvRow(tr("llm.embedding.minSimilarity"), String(e.minSimilarity)));
  rows.push(kvRow(tr("llm.embedding.fingerprint"), e.fingerprint, { mono: true, title: tr("llm.embedding.fingerprintWhy") }));
  return h("div", { id: "leInForce" }, card(rows));
}

function signature(s           )         {
  return JSON.stringify([s.serving, s.embedding, s.providers]);
}

/** The settings over the stored block, as three sections - the model, how retrieval reads its
 *  vectors, how texts are sent - and the page's one pair of actions under them. */
function formNode(s           )              {
  const b = stored;
  const usable = s.providers.filter((p) => p.style === "openai");
  // A provider the block names but the list lost still shows, so the select never lies.
  const known = new Set(s.providers.map((p) => p.name));
  const lost = b.provider !== "off" && !known.has(b.provider);
  const provider = h("select", { id: "leProvider" },
    h("option", { value: "off", selected: b.provider === "off" }, tr("llm.embedding.off")),
    s.providers.map((p) => p.style === "openai"
      ? h("option", { value: p.name, selected: b.provider === p.name }, p.name)
      : h("option", { value: p.name, disabled: true }, tr("llm.embedding.anthropicNo", { name: p.name }))),
    lost ? h("option", { value: b.provider, selected: true }, b.provider) : null);
  // One of the provider's embedding models; "" follows its first, whatever that becomes.
  const model = h("select", { id: "leModel" });
  const modelWhy = h("span", { id: "leModelWhy" });
  const dims = input("leDims", b.dimensions, tr("llm.embedding.dimsNative"));
  const batch = input("leBatch", b.batch, "16");
  const timeout = input("leTimeout", b.timeoutMs, "30000");
  const qPrefix = input("leQPrefix", b.queryPrefix, tr("llm.embedding.queryPrefixPh"));
  const dPrefix = input("leDPrefix", b.documentPrefix, tr("llm.embedding.documentPrefixPh"));
  const floor = input("leFloor", b.minSimilarity, "0.3");
  const privacy = hint("", { id: "leRemote", bad: true, hidden: true });
  const err = hint("", { id: "leErr", bad: true, hidden: true, live: true });
  const out = h("div", { id: "leTestOut" });
  const test = btn(tr("llm.test"), { id: "leTest" });
  const save = btn(tr("llm.save"), { id: "leSave", kind: "primary" });

  const fields = h("div", { id: "leFields" },
    pair(
      field({ label: tr("llm.embedding.model"), control: model, hint: modelWhy }),
      field({ label: tr("llm.embedding.dimensions"), control: dims, hint: tr("llm.embedding.dimensionsWhy") })),
    privacy);
  const retrieval = section({ cap: tr("llm.embedding.retrieval"), note: tr("llm.embedding.retrievalNote") }, card(form(
    field({ label: tr("llm.embedding.minSimilarity"), control: floor, hint: tr("llm.embedding.minSimilarityWhy") }),
    pair(
      field({ label: tr("llm.embedding.queryPrefix"), control: qPrefix }),
      field({ label: tr("llm.embedding.documentPrefix"), control: dPrefix })),
    hint(tr("llm.embedding.prefixWhy")))));
  retrieval.id = "leRetrieval";
  const throughput = section({ cap: tr("llm.embedding.throughput"), note: tr("llm.embedding.throughputNote") }, card(form(
    pair(
      field({ label: tr("llm.embedding.batch"), control: batch, hint: tr("llm.embedding.batchWhy") }),
      field({ label: tr("llm.embedding.timeout"), control: timeout, hint: tr("llm.embedding.timeoutWhy") })))));
  throughput.id = "leThroughput";

  // The pick per provider: switching away and back, or off and on, keeps what was picked.
  const picks = new Map                (b.model ? [[b.provider, b.model]] : []);
  let shown = "";
  const paintModels = ()       => {
    if (shown) picks.set(shown, model.value);
    // Hidden while off: the select keeps the last pick, saved with the block as it was. A
    // block stored off holds its stored pick the same way until a provider is shown.
    if (provider.value === "off") {
      if (!shown) {
        fill(model, b.model ? h("option", { value: b.model }, b.model) : null);
        model.value = b.model || "";
      }
      return;
    }
    shown = provider.value;
    const ids = modelsOf(s, shown, "embedding");
    const want = picks.get(shown) || "";
    // A stored pick the provider no longer lists still shows, so the select never lies.
    const stale = want !== "" && !ids.includes(want);
    fill(model,
      h("option", { value: "" }, ids.length ? tr("llm.embedding.modelDefault", { model: ids[0] }) : tr("llm.embedding.modelNone")),
      ids.map((id) => h("option", { value: id }, id)),
      stale ? h("option", { value: want }, tr("llm.embedding.modelUnlisted", { model: want })) : null);
    model.value = want;
    model.disabled = !ids.length && !stale;
    modelWhy.textContent = ids.length || stale ? tr("llm.embedding.modelWhy") : tr("llm.embedding.noModels", { provider: shown });
  };

  const say = (text        )       => { err.textContent = text; err.hidden = !text; };
  /** The block the form describes. Off keeps the rest of the block (the server reads only the
   *  provider then), so switching off and back on loses nothing typed. */
  const read = ()                             => {
    const next                    = { provider: provider.value };
    if (model.value) next.model = model.value;
    const nums                                                                                          = [
      ["dimensions", dims, tr("llm.embedding.dimensions"), 1, DIMS_MAX],
      ["batch", batch, tr("llm.embedding.batch"), 1, BATCH_MAX],
      ["timeoutMs", timeout, tr("llm.embedding.timeout"), TIMEOUT_MIN_MS, TIMEOUT_MAX_MS],
    ];
    for (const [k, el, label, min, max] of nums) {
      const v = wholeField(el.value, label, min, max);
      if (isError(v)) return v.error;
      if (v != null) next[k] = v;
    }
    // A prefix is kept as typed: its trailing space is the point ("query: ").
    for (const [k, el, label] of [["queryPrefix", qPrefix, tr("llm.embedding.queryPrefix")], ["documentPrefix", dPrefix, tr("llm.embedding.documentPrefix")]]         ) {
      if (el.value.length > PREFIX_MAX) return tr("llm.embedding.prefixLong", { field: label, max: PREFIX_MAX });
      if (el.value) next[k] = el.value;
    }
    const f = floor.value.trim();
    if (f) {
      const n = Number(f);
      if (!/^\d*\.?\d+$/.test(f) || n > 1) return tr("llm.embedding.fraction", { field: tr("llm.embedding.minSimilarity") });
      next.minSimilarity = n;
    }
    if (next.provider !== "off") {
      const ids = modelsOf(s, next.provider, "embedding");
      if (!next.model && !ids.length) return tr("llm.embedding.noModels", { provider: next.provider });
      // A stored pick the provider no longer lists shows, but the server refuses it.
      if (next.model && !ids.includes(next.model)) return tr("llm.embedding.modelNotListed", { provider: next.provider, model: next.model });
    }
    return next;
  };
  readForm = read;
  const paint = ()       => {
    const on = provider.value !== "off";
    fields.hidden = !on;
    retrieval.hidden = !on;
    throughput.hidden = !on;
    const p = s.providers.find((x) => x.name === provider.value);
    const host = on && p ? remoteHost(p.baseUrl) : null;
    privacy.hidden = !host;
    privacy.textContent = host ? tr("llm.embedding.remote", { host }) : "";
    // Test asks the block in force; with none in force there is nothing to ask.
    test.disabled = !s.embedding;
  };
  provider.onchange = ()       => { paintModels(); paint(); };
  const onSave = async ()                => {
    say("");
    fill(out);
    const next = read();
    if (typeof next === "string") { say(next); return; }
    save.disabled = true;
    try {
      const failure = await putRow((r        ) => { r.embedding = next; });
      if (failure) { say(failure); return; }
      toast(tr("llm.embedding.saved"));
      await reload();
    } finally {
      save.disabled = false;
    }
  };
  const onTest = async ()                => {
    say("");
    if (hasPendingChanges()) { say(tr("llm.embedding.saveFirst")); return; }
    test.disabled = true;
    fill(out, note(tr("llm.test.asking"), { busy: true }));
    const r = await call              ("POST", "/api/llm/embedding/test", {});
    test.disabled = false;
    fill(out, r.data ? testNode(r.data) : hint(r.err, { bad: true }));
  };
  save.onclick = ()       => { void onSave(); };
  test.onclick = ()       => { void onTest(); };
  const node = h("div", null,
    section({ cap: tr("llm.embedding.modelCap"), note: tr("llm.embedding.settingsNote") }, card(form(
      field({ label: tr("llm.embedding.provider"), control: provider, hint: usable.length ? tr("llm.embedding.providerWhy") : tr("llm.embedding.noProvider") }),
      fields))),
    retrieval,
    throughput,
    section({}, err, formActions(h("span", { class: "grow" }), test, save), out));
  paintModels();
  paint();
  return node;
}

function render()       {
  const s = status;
  if (!s) return;
  painted = signature(s);
  fill($("pane"), paneBody({ wide: true },
    paneHead({ desc: tr("llm.embedding.desc"), sub: s.serving ? null : tr("llm.notServing") }),
    split(formNode(s), section({ cap: tr("llm.embedding.inForce") }, inForceNode(s)))));
  $("countChip").textContent = countText();
}

/** Status and row together: the card shows the one, the form edits the other. */
async function reload()                   {
  const at = visit;
  const [st, row] = await Promise.all([loadStatus(), loadRow()]);
  if (at !== visit) return false;
  if (!st.data || !row.data) {
    fill($("pane"), paneBody({ wide: true }, failNote({ text: tr("llm.loadFailed"), why: st.err || row.err })));
    status = null;
    return false;
  }
  status = st.data;
  stored = row.data.embedding || { provider: "off" };
  render();
  return true;
}

let stopLive                      = null;

/** A CONFIG notice repaints the in-force card alone when the form is dirty. */
async function onConfig()                {
  const at = visit;
  const r = await loadStatus();
  if (at !== visit || !r.data || !status) return;
  status = r.data;
  if (signature(r.data) === painted) return;
  if (hasPendingChanges()) {
    painted = signature(r.data);
    $("leInForce").replaceWith(inForceNode(r.data));
  } else {
    await reload();
  }
}

export async function mount()                {
  visit++;
  painted = "";
  readForm = null;
  if (stopLive) stopLive();
  stopLive = followLlmConfig(() => onConfig());
  await reload();
}

export function unmount()       {
  visit++;
  readForm = null;
  if (stopLive) stopLive();
  stopLive = null;
}

export async function refresh()                {
  if (!hasPendingChanges()) await reload();
}

export function countText()         {
  return status && status.embedding ? status.embedding.model : "";
}

/** An unsaved edit holds the panel's auto reload and the language switch (SPEC §panel.nav). */
export function hasPendingChanges()          {
  if (!readForm) return false;
  const now = readForm();
  return typeof now === "string" || canon(now) !== canon(stored);
}
