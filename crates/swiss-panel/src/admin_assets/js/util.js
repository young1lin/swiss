var TOKEN_ID_KEY = "mcp_gateway_token_id"; // which token copied connect commands embed
var THEME_KEY = "swiss_theme";       // auto | light | dark — the preference, not the result
var KINDS = ["tools", "resources", "prompts"];
/* Mirrors DEFAULT_GROUP in managed.ts: the name a group list starts from — an ordinary group
   the user can rename or delete, whose only privilege is being the initial FIRST entry (the
   slot unassigned MCPs render under). The server now stores it like any other name. */
var DEFAULT_GROUP = "default";

var state = {
  mcps: [],          // rows from /api/mcps (each carries .group)
  groups: [],        // group names in sidebar order — the FIRST entry is the sink slot for unassigned rows
  collapsed: {},     // mcps fold map; group name -> true. Panel-only, so it lives in localStorage (groups.js)
  selected: null,    // selected MCP name
  filter: "",
  detail: null,      // { name, tab, config, source, editing, editType, tools:{...}, calls, ... }
  busy: {},          // name -> verb in flight
  lastAction: {},    // name -> { msg, err, at }
  mem: null,
  menuOpen: false,
  info: null,        // { tokenEnv } from /api/info — the env var name
  tokens: [],        // named tokens from /api/tokens (no secrets) — the management list
  activeSecret: null, // most-recently shown token secret, embedded into copied connect commands
  traffic: [],       // ONE PAGE of interactions from /api/traffic — no .body/.response on a row
  trafficClients: [],       // every client in the ring, folded server-side (not just this page)
  trafficFilter: "actions", // "actions" (tools/call, resources/read, …) or "all" (incl. protocol)
  trafficClient: null,      // a clientKey to narrow the activity log, or null
  trafficPage: 0,           // 0-based, newest first
  trafficMore: false,       // is there an older page?
  trafficTotal: 0,          // rows matching the current filter, across all pages
  trafficAll: 0,            // rows in the ring before filtering — the "n of m" readout
  trafficOpen: {},          // seq -> expanded: shows the raw request JSON for that interaction
  trafficFull: {},          // seq -> { body, response }, fetched on first expand (cf. d.callsFull)
  trafficSig: null,         // data signature; a poll skips re-render when unchanged (keeps expansion)
  view: "mcps",      // "mcps" | "tunnels" | "traffic" | "data" | "jobs" — the toolbar switcher
  panelVersion: null, // admin.html mtime stamp from /api/info; a change means a new build landed
  addGroup: null,     // sidebar group a header "+" targets for the next created MCP (add-sheet.js)
  draggingGroup: null, // name of the GROUP HEADER being dragged — polls must not rebuild under it either
  db: null,           // the whole Data view state (data-view.js builds/owns it; redisValue rides on it)
  tun: {             // tunnel view state; `data` is the last /api/tunnels response
    tab: "conns",    // "conns" | "rules"
    data: null,
    busy: {},        // id -> verb in flight
    keys: null,      // cached /api/tunnels/keys
    dragging: null,  // id of the row being dragged — polls must not rebuild under it
    draggingGroup: null, // name of the group header being dragged — same rebuild freeze as a row drag
  pendingGroup: null, // group chosen via a header "+", preselected in the sheet it opens
    pendingGroup: null, // group chosen via a header "+", preselected in the sheet it opensands in
  },
  jobs: {             // jobs view state (jobs.js renders it; polling.js loads it)
    data: [],        // rows from /api/jobs
    busy: {},        // name -> Run now in flight
    painted: "",     // joined row names at last render — the poll's structural signature
    hist: null,      // the open history sheet: { name, runs }
  },
};

function $(id) { return document.getElementById(id); }
function el(tag, cls, text) {
  var n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text != null) n.textContent = text;
  return n;
}
function esc(s) {
  return String(s == null ? "" : s).replace(/[&<>"']/g, function (c) {
    return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
  });
}
function now() { return new Date().toLocaleTimeString(); }

/** One inline icon from the shell's sprite (docs/18 V2). Stroke follows currentColor and .ic
 *  sizes it; decorative by default, an image with a name when `label` is passed (icon buttons). */
function icon(name, label) {
  return label
    ? '<svg class="ic" role="img" aria-label="' + esc(label) + '"><use href="#i-' + name + '"></use></svg>'
    : '<svg class="ic" aria-hidden="true"><use href="#i-' + name + '"></use></svg>';
}

/** One empty state (docs/18 V7): icon, title, one line of hint, optional ghost action. Every
 *  view's "nothing here" is this shape — the Terminal alone keeps its own, because it lives in
 *  the black frame with its own tokens. The action button carries data-empty-action so the
 *  owning view can wire it without inventing per-view ids. */
function emptyHtml(opts) {
  var action = opts.action
    ? '<button class="btn ghost" data-empty-action="' + esc(opts.action) + '">' + esc(opts.action) + "</button>"
    : "";
  return '<div class="empty"><div>' +
    '<span class="empty-ic">' + icon(opts.icon) + "</span>" +
    "<h2>" + esc(opts.title) + "</h2>" +
    (opts.hint ? '<p class="hint">' + esc(opts.hint) + "</p>" : "") +
    action +
    "</div></div>";
}

/** The status dot's tooltip (docs/18 V6): a colour — and the idle hollow ring above all —
 *  names no behaviour of its own, so the title says it aloud, reusing the state words the row
 *  already paints, never a synonym of our own. ONE builder for every dot in the panel: the
 *  chip text once drifted between two copies, and a dot whose title disagrees with its class
 *  is the same bug one hover wide. */
function dotTitle(word, latencyMs, reason) {
  if (word === "up") return latencyMs != null ? "up · " + latencyMs + " ms" : "up";
  // Idle is the one word that explains nothing: say what the ring means — nothing is wrong,
  // it starts when it is first needed.
  if (word === "idle") return "idle — starts on first request";
  if (word === "error") return reason ? "error: " + reason : "error";
  return word || ""; // starting / stopping / reconnecting / down — the word the row already shows
}

/** One time format for row lists: time-of-day inside the last 24h, date+time beyond it (the
 *  pure time becomes ambiguous the moment a list spans midnight). Run-history had this logic as
 *  histWhen; Traffic rows now share it instead of printing bare times on pages days old. */
function whenLabel(iso) {
  var d = new Date(iso);
  if (isNaN(d.getTime())) return String(iso);
  var day = 24 * 60 * 60 * 1000;
  return (Date.now() - d.getTime() >= day)
    ? d.toLocaleString()
    : d.toLocaleTimeString();
}

// Fold state for every scope lives in groups.js now — keyed swiss.groups.<scope>.collapsed, one
// key per scope, because a group named "prod" in two lists folding together would be a
// coincidence, not a feature.

function toast(msg, isErr) {
  var t = $("toast");
  t.textContent = msg;
  t.className = "toast" + (isErr ? " err" : "");
  t.hidden = false;
  clearTimeout(toast._t);
  toast._t = setTimeout(function () { t.hidden = true; }, 3400);
}


async function api(path, opts) {
  opts = opts || {};
  opts.headers = Object.assign({ "Content-Type": "application/json" }, opts.headers || {});
  return fetch(path, opts);
}

/** Call the API and hand back the parsed body, or null once the failure has been reported. Every
 *  mutation repeated the same fetch → parse → toast dance, so the dance lives here once. */
async function apiJson(path, opts) {
  try {
    var r = await api(path, opts);
    var j = await r.json().catch(function () { return {}; });
    if (!r.ok) { toast(j.error || "HTTP " + r.status, true); return null; }
    return j;
  } catch (e) {
    // A network-level failure (gateway stopped mid-click) must be reported too: every caller is
    // `if (!j) return;`, and a silent null made clicking Commit do literally nothing after the
    // confirm dialog.
    toast("request failed — is the gateway running?", true);
    return null;
  }
}

export { $, DEFAULT_GROUP, KINDS, THEME_KEY, TOKEN_ID_KEY, api, apiJson, dotTitle, el, emptyHtml, esc, icon, now, state, toast, whenLabel };
