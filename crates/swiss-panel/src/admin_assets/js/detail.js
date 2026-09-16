/*
 * Copyright 2026 The swiss authors
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

import { $, KINDS, api, apiJson, now, state, toast } from "./util.js";
import { readFields, translateOauth } from "./fields.js";
import { fmtJson } from "./logs.js";
import { patchSidebar } from "./menu.js";
import { patchDetailHead, renderPane } from "./pane.js";
import { loadList } from "./polling.js";
import { histOpen, renderCallsOnly } from "./run-history.js";
import { rowOf } from "./sidebar.js";

/* --- lifecycle actions ------------------------------------------------------------------------ */
async function act(name, verb) {
  if (state.busy[name]) return;
  state.busy[name] = verb;
  patchSidebar(); patchDetailHead();
  // docs/28 D2: the wire keeps the stop/start verbs; the panel says disable/enable — a stop
  // that survives a boot and refuses every client is a disable, and the word owed it.
  var shown = verb === "stop" ? "disable" : verb === "start" ? "enable" : verb;
  try {
    var r = await api("/api/mcps/" + encodeURIComponent(name) + "/" + verb, { method: "POST" });
    var j = await r.json();
    if (!r.ok) {
      state.lastAction[name] = { msg: shown + " failed: " + (j.error || "HTTP " + r.status), err: true, at: now() };
      toast(name + ": " + (j.error || "failed"), true);
    } else {
      var state_word = j.lifecycle === "stopped" ? "disabled" : j.lifecycle;
      state.lastAction[name] = { msg: shown + " → " + (state_word || "ok"), err: false, at: now() };
      toast(name + ": " + state.lastAction[name].msg);
    }
  } catch (e) {
    state.lastAction[name] = { msg: shown + " request failed", err: true, at: now() };
  }
  delete state.busy[name];
  await loadList();
  // The server rebuilt on start/restart, so any cached page list is stale.
  if (state.detail && state.detail.name === name) {
    KINDS.forEach(function (k) { state.detail[k] = pageState(); });
    renderPane();
    loadMeta(name);
    if (KINDS.indexOf(state.detail.tab) >= 0) loadPage(name, state.detail.tab);
  }
}

async function renameMcp(name) {
  var next = prompt("Rename '" + name + "' to:", name);
  if (!next || next.trim() === name) return;
  next = next.trim();
  var ok = await apiJson("/api/mcps/" + encodeURIComponent(name) + "/rename", { method: "POST", body: JSON.stringify({ name: next }) });
  if (!ok) return;
  if (state.selected === name) state.selected = next;
  if (state.detail && state.detail.name === name) state.detail.name = next;
  toast("Renamed " + name + " → " + next);
  await loadList();
  renderPane();
}

async function removeMcp(name) {
  // Config-sourced MCPs are removed from gateway.config.json too (server-side), so the confirm
  // says so — "removes it permanently" alone used to hide that the file edit is part of it.
  var fromConfig = !!(state.detail && state.detail.source === "config");
  if (!confirm("Delete '" + name + "'?\n\nThis stops it and removes it permanently" +
      (fromConfig ? ", including its entry in gateway.config.json." : "."))) return;
  var ok = await apiJson("/api/mcps/" + encodeURIComponent(name), { method: "DELETE" });
  if (!ok) return;
  if (state.selected === name) { state.selected = null; state.detail = null; }
  toast("Deleted " + name);
  await loadList();
  renderPane();
}

/* --- OAuth authorize (docs/24 D5) -------------------------------------------------------------- */
/** One click, one flow: the POST plants it (or hands back the live one — the server
 *  single-flights per name), this opens the provider's consent page in a real browser
 *  window the moment its URL exists, and keeps polling every 3s until approval lands
 *  (credentials stored, MCP auto-started) or the flow errors. Status rides the pane's
 *  lastAction note; renderPane is skipped while a config edit is open, so a status update
 *  never eats a form the user is filling in. */
function pause(ms) { return new Promise(function (res) { setTimeout(res, ms); }); }

async function authorizeMcp(name) {
  var d = state.detail;
  if (!d || d.name !== name || d.oauthBusy) return;
  d.oauthBusy = true;
  var note = function (msg, err) {
    state.lastAction[name] = { msg: msg, err: !!err, at: now() };
    if (!state.detail || state.detail.editing) return;
    renderPane();
  };
  try {
    var r = await api("/api/mcps/" + encodeURIComponent(name) + "/authorize", { method: "POST" });
    var j = await r.json().catch(function () { return {}; });
    if (!r.ok) { note("authorize failed: " + (j.error || "HTTP " + r.status), true); return; }
    note("authorize: preparing the consent page…");
    var opened = false;
    // 100 polls x 3s = 5 min, the server flow's own callback cap.
    for (var i = 0; i < 100; i++) {
      if (i > 0) await pause(3000);
      var p = await api("/api/mcps/" + encodeURIComponent(name) + "/authorize");
      var s = await p.json().catch(function () { return {}; });
      if (s.status === "authorization_required" && s.authorizationUrl && !opened) {
        opened = true;
        window.open(s.authorizationUrl, "_blank");
        note("authorize: consent page opened — approve it in the browser…");
      } else if (s.status === "approved") {
        state.lastAction[name] = { msg: "authorized · " + (s.tools != null ? s.tools + " tools" : "MCP started"), err: false, at: now() };
        toast(name + ": authorized");
        await loadList();
        loadMeta(name);
        if (state.detail && !state.detail.editing) renderPane();
        return;
      } else if (s.status === "error") {
        note("authorize failed: " + (s.error || "unknown error"), true);
        await loadList();
        return;
      }
      // starting, or the URL not yet in this answer: keep polling.
    }
    note("authorize: timed out waiting for approval", true);
  } catch (e) {
    note("authorize request failed", true);
  } finally {
    if (state.detail && state.detail.name === name) state.detail.oauthBusy = false;
  }
}

/* --- detail data ------------------------------------------------------------------------------ */
function pageState() {
  return { items: [], nextCursor: undefined, total: undefined, pageSize: 50, cursors: [""], loading: false, loaded: false, error: null };
}

function openDetail(name) {
  if (state.detail && state.detail.name === name) return;
  state.selected = name;
  var d = {
    name: name, tab: "tools", config: null, source: undefined, editing: false, editType: null, editVals: null,
    // OAuth (docs/24 D5): the detail's auth state ("authorized" | "needs-auth" | undefined),
    // and whether an authorize flow this panel started is still polling.
    oauth: undefined, oauthBusy: false,
    run: {
      tool: null, result: null, running: false,
      // The refill control next to Run: one tool's newest recorded runs (hist), which tool they
      // belong to (histTool — null/other means stale and needs (re)loading), an in-flight fetch,
      // whether the popover is open, the seq being previewed, full entries cached by seq, and the
      // search box's live query (histQ — the server matches it against the FULL stored arguments).
      // histFull MUST be initialized: member access on undefined (histFull[seq]) throws, which is
      // exactly how a missing field here once broke both the preview pane and the refill.
      hist: null, histTool: null, histLoading: false,
      histOpen: false, histSelSeq: null, histFull: {}, histQ: "",
    },
    // Logs tab: a page of recorded tool calls (null until loaded), any child stderr, which rows are
    // expanded, and replies fetched in full by seq.
    calls: null, stderr: "", callsOpen: {}, callsLoading: false,
    callsPage: 0, callsMore: false, callsFull: {},
  };
  KINDS.forEach(function (k) { d[k] = pageState(); });
  state.detail = d;
  state.menuOpen = false;
  patchSidebar();
  renderPane();
  loadMeta(name);
  if (rowOf(name) && rowOf(name).lifecycle === "started") loadPage(name, "tools");
}

async function loadMeta(name) {
  // Compare the detail OBJECT, not its name: leaving an MCP and coming back builds a fresh detail
  // under the same name, and a slow response from the first visit would otherwise write into the
  // second one. loadPage already does it this way.
  var d = state.detail;
  try {
    var r = await api("/api/mcps/" + encodeURIComponent(name) + "/details");
    if (!r.ok) return;
    var j = await r.json();
    if (state.detail !== d) return;
    d.config = j.config || null;
    d.source = j.source;
    d.tunnels = j.tunnels || [];
    d.oauth = j.oauth;
    if (d.editing) return; // never rebuild a form the user is filling in
    // The pane header grows an Authorize button the moment the config is an OAuth one (an
    // http def that says so, or the figma type that implies it), so the pane re-renders for
    // those MCPs too — not only when the config tab is open.
    if (d.tab === "config" || (d.config && (d.config.auth === "oauth" || d.config.type === "figma"))) renderPane();
  } catch (e) { /* handled */ }
}

/** One page of recorded tool calls (arguments + reply) and, for a proc MCP, its stderr. */
async function loadCalls(name) {
  var d = state.detail;
  if (!d || d.name !== name || d.callsLoading) return;
  d.callsLoading = true;
  try {
    var r = await api("/api/mcps/" + encodeURIComponent(name) + "/calls?page=" + d.callsPage);
    if (r.ok) {
      var j = await r.json();
      if (state.detail !== d) return; // a revisit built a new detail object — see loadMeta
      d.calls = j.calls || [];
      d.callsMore = !!j.more;
      d.stderr = j.stderr || "";
      renderCallsOnly();
    }
  } catch (e) { /* handled */ } finally {
    d.callsLoading = false; // this request's own flag, whatever the pane shows now
  }
}

function callsPageStep(delta) {
  var d = state.detail;
  if (!d) return;
  var next = d.callsPage + delta;
  if (next < 0 || (delta > 0 && !d.callsMore)) return;
  d.callsPage = next;
  d.calls = null; // show the loading state rather than the previous page's rows
  renderCallsOnly();
  loadCalls(d.name);
}

/** Fetch one reply in full — the log page ships only the first 2 KB of each. */
async function showFullResult(seq) {
  var d = state.detail;
  if (!d) return;
  try {
    var r = await api("/api/mcps/" + encodeURIComponent(d.name) + "/calls/" + encodeURIComponent(seq));
    if (!r.ok) { toast("HTTP " + r.status, true); return; }
    var j = await r.json();
    if (!state.detail || state.detail.name !== d.name || !j.call) return;
    if (j.call.bodyGone) {
      // Only the newest replies keep their payload; say which part is missing rather than showing a
      // short result as if it were whole.
      toast("The full reply is no longer stored — only the newest 50 per MCP are kept.", true);
      return;
    }
    d.callsFull[seq] = j.call.output;
    var pre = document.querySelector('#tabbody .call[data-seq="' + seq + '"] pre[data-out]');
    if (pre) pre.textContent = fmtJson(j.call.output);
    var btn = document.querySelector('#tabbody [data-full="' + seq + '"]');
    if (btn) btn.remove();
  } catch (e) { toast("request failed", true); }
}

async function clearCalls() {
  var d = state.detail;
  if (!d) return;
  try {
    var r = await api("/api/mcps/" + encodeURIComponent(d.name) + "/calls", { method: "DELETE" });
    if (!r.ok) { toast("HTTP " + r.status, true); return; }
    d.calls = [];
    d.callsOpen = {};
    d.callsFull = {};
    d.callsPage = 0;
    d.callsMore = false;
    renderCallsOnly();
  } catch (e) { toast("request failed", true); }
}

async function loadPage(name, kind) {
  var d = state.detail;
  if (!d || d.name !== name) return;
  var kd = d[kind];
  if (!kd || kd.loading) return;
  kd.loading = true;
  kd.error = null;
  if (d.tab === kind) renderPane();
  try {
    var cursor = kd.cursors[kd.cursors.length - 1];
    var url = "/api/mcps/" + encodeURIComponent(name) + "/" + kind + (cursor ? "?cursor=" + encodeURIComponent(cursor) : "");
    var r = await api(url);
    var j = await r.json().catch(function () { return {}; });
    if (r.ok) {
      kd.items = j[kind] || [];
      kd.nextCursor = j.nextCursor;
      kd.total = j.total;
      kd.pageSize = j.pageSize || kd.pageSize;
      kd.loaded = true;
      if (kind === "tools") kd.disabled = j.disabledTools || [];
      if (kind === "resources") kd.resourceEnabled = j.resourceEnabled !== false;
    } else {
      // Surface it instead of silently showing "none" — a direct adapter has no resources/prompts
      // capability at all, and that is worth saying out loud.
      kd.error = j.error || "HTTP " + r.status;
    }
  } catch (e) {
    kd.error = "request failed";
  }
  kd.loading = false;
  // Run builds its form from the tool list, so it also needs a repaint when tools land.
  if (state.detail === d && (d.tab === kind || (d.tab === "run" && kind === "tools"))) renderPane();
}

function showTab(tab) {
  var d = state.detail;
  if (!d) return;
  d.tab = tab;
  d.editing = false;
  renderPane();
  if (KINDS.indexOf(tab) >= 0 && !d[tab].loaded && !d[tab].loading) loadPage(d.name, tab);
  // The config tab's revision list (docs/28 D1) rides along with the tab, not the poll.
  if (tab === "config") loadRevisions(d.name);
  // Run needs the tool list to build its argument form.
  if (tab === "run" && !d.tools.loaded && !d.tools.loading) loadPage(d.name, "tools");
  if (tab === "logs") loadCalls(d.name);
}
function pageNext() {
  var d = state.detail, kd = d && d[d.tab];
  if (!kd || !kd.nextCursor || kd.loading) return;
  kd.cursors.push(kd.nextCursor);
  loadPage(d.name, d.tab);
}
function pagePrev() {
  var d = state.detail, kd = d && d[d.tab];
  if (!kd || kd.cursors.length <= 1 || kd.loading) return;
  kd.cursors.pop();
  loadPage(d.name, d.tab);
}

/* --- config edit ------------------------------------------------------------------------------ */
function startEdit() {
  var d = state.detail;
  if (!d || !d.config) return;
  d.editing = true;
  d.editMode = "edit";
  d.editType = d.config.type || "proc";
  d.editVals = null; // start from what is stored
  renderPane();
}
// docs/28 D1: the same form, another verb — Save parks the current def as a revision and
// installs the new one under the SAME name. The operator's rollback lives one click away.
function startReplace() {
  var d = state.detail;
  if (!d || !d.config) return;
  d.editing = true;
  d.editMode = "replace";
  d.editType = d.config.type || "proc";
  d.editVals = null;
  renderPane();
}
function cancelEdit() {
  if (!state.detail) return;
  state.detail.editing = false;
  state.detail.editMode = null;
  state.detail.editType = null;
  state.detail.editVals = null;
  renderPane();
}

/* --- def revisions (docs/28 D1) ---------------------------------------------------------------- */
async function loadRevisions(name) {
  var d = state.detail;
  if (!d || d.name !== name) return;
  try {
    var r = await api("/api/mcps/" + encodeURIComponent(name) + "/revisions");
    if (!r.ok) return;
    var j = await r.json();
    if (state.detail !== d) return; // a revisit built a new detail object — see loadMeta
    d.revisions = j.revisions || [];
    if (d.tab === "config" && !d.editing) renderPane();
  } catch (e) { /* handled */ }
}

async function saveReplace() {
  var d = state.detail;
  if (!d) return;
  var type = d.editType || (d.config && d.config.type) || "proc";
  var fields = readFields(type, "e-");
  var body = Object.assign({ type: type }, fields);
  if (body.autostart !== undefined) { body.lazy = !body.autostart; delete body.autostart; }
  translateOauth(body);
  var noteEl = $("e-note");
  if (noteEl) body.note = noteEl.value;
  if (type === "proc" && !body.command) { toast("Command is required", true); return; }
  var name = d.name;
  var restore = function () {
    if (!state.detail || state.detail !== d) return;
    d.editing = true;
    d.editType = type;
    d.editVals = fields;
  };
  state.busy[name] = "save";
  d.editing = false;
  renderPane(); patchSidebar();
  try {
    var r = await api("/api/mcps/" + encodeURIComponent(name) + "/replace", { method: "POST", body: JSON.stringify(body) });
    var j = await r.json();
    if (!r.ok) {
      state.lastAction[name] = { msg: "replace failed: " + (j.error || "HTTP " + r.status), err: true, at: now() };
      toast(j.error || "replace failed", true);
      restore();
    } else {
      state.lastAction[name] = { msg: "def replaced → revision " + (j.revisions || "?") + " parked", err: false, at: now() };
      toast(name + ": replaced — previous def parked as revision " + (j.revisions || "?"));
      // The swap stands even when the new def will not start; the panel must say so, not hide it.
      if (j.restartError) toast(name + " failed to start: " + j.restartError, true);
      if (state.detail === d) d.editVals = null;
      KINDS.forEach(function (k) { if (state.detail) state.detail[k] = pageState(); });
      loadMeta(name);
      loadRevisions(name);
    }
  } catch (e) {
    state.lastAction[name] = { msg: "replace request failed", err: true, at: now() };
    toast("replace request failed", true);
    restore();
  }
  delete state.busy[name];
  await loadList();
  renderPane();
}

async function restoreRevision(index) {
  var d = state.detail;
  if (!d) return;
  if (!confirm("Restore revision " + (index + 1) + "?\n\nThe current def is parked as a new revision first — this is reversible too.")) return;
  try {
    var r = await api("/api/mcps/" + encodeURIComponent(d.name) + "/revisions/" + index + "/restore", { method: "POST", body: "{}" });
    var j = await r.json();
    if (!r.ok) { toast(j.error || "restore failed", true); return; }
    toast(d.name + ": revision " + (index + 1) + " restored");
    if (j.restartError) toast(d.name + " failed to start: " + j.restartError, true);
    KINDS.forEach(function (k) { if (state.detail) state.detail[k] = pageState(); });
    loadMeta(d.name);
    loadRevisions(d.name);
    await loadList();
    renderPane();
  } catch (e) { toast("restore request failed", true); }
}

async function deleteRevision(index) {
  var d = state.detail;
  if (!d) return;
  if (!confirm("Delete parked revision " + (index + 1) + "? This only drops the snapshot — the live def is untouched.")) return;
  if (!await apiJson("/api/mcps/" + encodeURIComponent(d.name) + "/revisions/" + index, { method: "DELETE" })) return;
  toast("Revision " + (index + 1) + " deleted");
  loadRevisions(d.name);
}
/** Switching type re-renders the form, so read what is in it first and carry it across — a field
 *  both types share (description, host, password) survives the switch. A masked secret carried into
 *  a type that never stored one is dropped server-side by unmaskBody, never saved as dots. */
function changeEditType(t) {
  var d = state.detail;
  if (!d) return;
  var prev = d.editType || (d.config && d.config.type) || "proc";
  d.editVals = Object.assign({}, d.editVals, readFields(prev, "e-"));
  d.editType = t;
  renderPane();
}

/**
 * Test connection — the DB types' form carries this beside Save. It POSTs the form's CURRENT
 * values (env refs intact) to /api/mcps/test, which expands them server-side and opens a real
 * driver connection with exactly these credentials — the same adapter and ping the health probe
 * uses. Nothing is saved: this is "will these values work", asked before committing them.
 */
async function runConnTest(p) {
  // p is the form's id prefix: "e-" for the inline editor, "a-" for the Add sheet.
  var d = state.detail;
  var type = p === "a-" ? $("a-type").value : (d && (d.editType || (d.config && d.config.type))) || "proc";
  var body = Object.assign({ type: type }, readFields(type, p));
  delete body.autostart; // a boot-time switch, not a credential — irrelevant to a connection test
  // figma implies OAuth the way a checked auth box states it: no keyless test exists for either.
  var wantsOauth = type === "figma" || (type === "http" && body.auth === true);
  delete body.auth; delete body.oauthClientName; // a credential question, not a connectivity one
  var btn = $(p + "test"), out = $(p + "test-out");
  if (!btn || !out) return;
  // A keyless handshake cannot test an OAuth remote: its endpoint answers 401 until the flow
  // runs, and the flow needs the MCP saved first (credentials are name-keyed). Say so rather
  // than firing a request whose only possible answer is the 401.
  if (wantsOauth) {
    out.textContent = "OAuth remote: use Authorize on the detail view — a keyless handshake is always 401.";
    out.style.color = "";
    return;
  }
  // A metered vision API: the only honest "test" is a real generation, and that spends
  // tokens. The health probe already skips it for the same reason — say so instead.
  if (type === "zai-vision") {
    out.textContent = "Metered GLM API: every call spends tokens — save it and call a tool from an MCP client instead.";
    out.style.color = "";
    return;
  }
  btn.disabled = true;
  btn.textContent = "Testing…";
  out.hidden = false;
  out.textContent = "connecting with these exact values — ${ENV} refs expand server-side…";
  out.style.color = "";
  try {
    var r = await api("/api/mcps/test", { method: "POST", body: JSON.stringify(body) });
    var j = await r.json();
    if (j.ok) {
      // For a rest target any HTTP answer is reachable — show which one came back (404 from the
      // base path is fine; the tools live under their own paths).
      out.textContent = "✓ connected — these values work (" + j.ms + " ms" + (j.status ? " · HTTP " + j.status : "") + ")";
      out.style.color = "var(--green)";
    } else {
      out.textContent = "✗ " + (j.error || "failed") + (j.ms != null ? " (" + j.ms + " ms)" : "");
      out.style.color = "var(--red)";
    }
  } catch (e) {
    out.textContent = "test request failed — is the gateway running?";
    out.style.color = "var(--red)";
  } finally {
    btn.disabled = false;
    btn.textContent = "Test connection";
  }
}

async function saveEdit() {
  var d = state.detail;
  if (!d) return;
  var type = d.editType || (d.config && d.config.type) || "proc";
  var fields = readFields(type, "e-");
  var body = Object.assign({ type: type }, fields);
  if (body.autostart !== undefined) { body.lazy = !body.autostart; delete body.autostart; }
  translateOauth(body); // the auth checkbox is the def auth string (docs/24 D1)
  if (type === "proc" && !body.command) { toast("Command is required", true); return; }
  var name = d.name;
  // Rendering the pane destroys the form, so hold on to what was typed: a save the server rejects
  // used to cost the user the whole form, with nothing to do but reopen it and retype.
  var restore = function () {
    if (!state.detail || state.detail !== d) return;
    d.editing = true;
    d.editType = type;
    d.editVals = fields;
  };
  state.busy[name] = "save";
  d.editing = false;
  renderPane(); patchSidebar();
  try {
    var r = await api("/api/mcps/" + encodeURIComponent(name), { method: "PUT", body: JSON.stringify(body) });
    var j = await r.json();
    if (!r.ok) {
      state.lastAction[name] = { msg: "edit failed: " + (j.error || "HTTP " + r.status), err: true, at: now() };
      toast(j.error || "edit failed", true);
      restore();
    } else {
      state.lastAction[name] = { msg: "config saved → restarted", err: false, at: now() };
      toast(name + ": config saved, restarted");
      if (state.detail === d) d.editVals = null;
      KINDS.forEach(function (k) { if (state.detail) state.detail[k] = pageState(); });
      loadMeta(name);
    }
  } catch (e) {
    state.lastAction[name] = { msg: "edit request failed", err: true, at: now() };
    toast("edit request failed", true);
    restore();
  }
  delete state.busy[name];
  await loadList();
  renderPane();
}

export { act, authorizeMcp, callsPageStep, cancelEdit, changeEditType, clearCalls, deleteRevision, loadCalls, loadMeta, loadPage, loadRevisions, openDetail, pageNext, pagePrev, pageState, removeMcp, renameMcp, restoreRevision, runConnTest, saveEdit, saveReplace, showFullResult, showTab, startEdit, startReplace };
