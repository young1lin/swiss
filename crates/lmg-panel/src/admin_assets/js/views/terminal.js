/* ================================================================================================
   Terminal - the panel half of the terminal plugin (docs/14 §2, §8).

   One real terminal per session: xterm.js (vendored, native ES modules via shims) over
   the gateway's WebSocket. The wire contract is exactly docs/14 §8: binary frames are
   raw PTY bytes both ways, the only text is a small control object, and a socket that
   drops does NOT end the session - the gateway holds it for the grace window and the
   catch-up bytes arrive on reconnect. The reconnect loop mints a fresh one-time ticket
   each round (the one from open lives 10 s; the grace lives 60 s - that asymmetry is
   the whole reason POST .../ticket exists).

   The pure half of this page (URLs, pacing, close stories, geometry clamping, picker
   rows) lives in ../terminal-core.js and is pinned by test/admin-terminal.test.ts.
   ================================================================================================ */
import { $, api, apiJson, esc, toast } from "../util.js";
import { loadXterm } from "../vendor/xterm/xterm-5.5.0/index.js";
import { loadFitAddon } from "../vendor/xterm/addon-fit-0.10.0/index.js";
import { loadUnicode11Addon } from "../vendor/xterm/addon-unicode11-0.8.0/index.js";
import { loadWebLinksAddon } from "../vendor/xterm/addon-web-links-0.11.0/index.js";
import { loadWebglAddon } from "../vendor/xterm/addon-webgl-0.18.0/index.js";
import {
  clampGeometry, frameStatus, nextReconnectDelay, resizeFrame, resizeUrl,
  sessionAlive, sessionLabel, sessionsUrl, streamUrl, targetRows, targetsUrl, ticketUrl,
} from "../terminal-core.js";

/* docs/14 §2: the system monospace stack - no Nerd Font, no web font. The resource
   pipeline is text-only; a font file cannot enter the tree, by design. */
var FONT = 'ui-monospace, SFMono-Regular, Consolas, "Cascadia Mono", monospace';

var packages = null;   // the vendored constructors, loaded once per module lifetime
var targets = null;    // the last /api/terminal/targets reply
var sessions = [];     // the last listing (rows without local state)
var models = [];       // sessions this mount has wired a terminal for
var active = null;     // the session id whose terminal is on stage
var epoch = 0;         // mount generation: loops and sockets from an older mount stop
var seq = 0;           // temp ids for sessions opened but not yet answered
var fitTimer = null;
/* Sessions the user dismissed. The gateway keeps a closed row in the listing until its
   grace window lapses; without this set, every repaint would resurrect a tab the user
   has already closed, and dismissing would look decorative. Cleared of ids the listing
   no longer carries so it cannot grow without bound. */
var dismissed = new Set();

function load() {
  if (!packages) {
    packages = Promise.all([
      loadXterm(), loadFitAddon(), loadUnicode11Addon(), loadWebLinksAddon(), loadWebglAddon(),
    ]).then(function (got) {
      /* Key names are the CONSTRUCTORS wireTerminal news up (got.Terminal, got.FitAddon,
         got.Unicode11Addon, got.WebLinksAddon, got.WebglAddon) - a mismatch here is
         "X is not a constructor" at first Open, which is exactly how it once shipped. */
      return { Terminal: got[0], FitAddon: got[1], Unicode11Addon: got[2], WebLinksAddon: got[3], WebglAddon: got[4] };
    });
  }
  return packages;
}

function sleep(ms) { return new Promise(function (r) { setTimeout(r, ms); }); }

function model(id) { return models.find(function (m) { return m.id === id; }) || null; }

/* The status foot under the surface: a state dot that answers before the words do,
   then one sentence for whichever session is on stage. The tone classes come from the
   status strings this file and the gateway's control frames produce. */
function statusTone(text) {
  if (!text) return "";
  if (/attached/.test(text)) return "ok";
  if (/connecting|opening|reconnecting/.test(text)) return "warn";
  if (/closed|exited|gone|stalled|not running|failed|could not/.test(text)) return "bad";
  return "";
}

function paintStatus() {
  var line = $("term-status");
  if (!line) return;
  var m = model(active);
  var text = m ? m.status : "";
  line.hidden = false;   // the foot always caps the card; an empty strip is still its shape
  line.textContent = "";
  if (!text) return;
  var dot = document.createElement("span");
  dot.className = "term-dot " + statusTone(text);
  line.appendChild(dot);
  line.appendChild(document.createTextNode(text));
}

/* The session tabs. A tab exists for every wired model plus every live listing row the
   user has not opened yet; a closed session keeps its tab (the scrollback is readable)
   until dismissed. */
function paintTabs() {
  var bar = $("term-tabs");
  if (!bar) return;
  var known = models.map(function (m) { return m.id; });
  var extra = sessions.filter(function (s) {
    return s && s.id && known.indexOf(s.id) < 0 && !dismissed.has(s.id);
  });
  var all = models.concat(extra.map(function (s) {
    return { id: s.id, target: s.target, label: s.label, status: "", gone: false };
  }));
  var any = all.length > 0;
  bar.hidden = !any;
  bar.innerHTML = all.map(function (m) {
    var label = esc(sessionLabel(m)) + (m.gone ? " · closed" : "");
    return '<button role="tab" data-act="select" data-id="' + esc(m.id) + '"' +
      ' aria-selected="' + String(m.id === active) + '" title="' + label + '">' +
      '<span class="term-tab-label">' + label + "</span>" +
      ' <span class="term-tab-x" data-act="close" data-id="' + esc(m.id) + '" title="Close session" role="button">\u00d7</span>' +
      "</button>";
  }).join("");
}

function paintStage() {
  var stage = $("term-stage");
  if (!stage) return;
  var empty = $("term-empty");
  if (!model(active)) {
    if (empty) empty.hidden = false;
  } else if (empty) {
    empty.hidden = true;
  }
  Array.prototype.forEach.call(stage.querySelectorAll("[data-term]"), function (holder) {
    holder.hidden = holder.getAttribute("data-term") !== active;
  });
}

/* Create the terminal for one session and append its holder. The holder stays hidden
   until the session is selected - xterm keeps its buffer offscreen, fit measures only
   what is visible. */
function wireTerminal(m) {
  if (m.term) return Promise.resolve();
  // The promise is module-cached, so a rejected load retries naturally on the next wire.
  return load().then(function (got) {
    var term = new got.Terminal({
      fontFamily: FONT,
      fontSize: 13,
      scrollback: 5000,
      allowProposedApi: true,   // terminal.unicode (the Unicode11 table) is a proposed API
      theme: termTheme(),
    });
    var holder = document.createElement("div");
    holder.className = "term-holder";
    holder.setAttribute("data-term", m.id);
    $("term-stage").appendChild(holder);
    term.open(holder);
    var fit = new got.FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new got.Unicode11Addon());
    term.unicode.activeVersion = "11";
    term.loadAddon(new got.WebLinksAddon());
    // WebGL is a pure-JS addon (verified at vendoring time: no wasm anywhere in it), but
    // a machine without WebGL must still get a terminal - the DOM renderer is the
    // fallback, and a lost GL context falls back to it mid-session the same way.
    try {
      var gl = new got.WebglAddon();
      gl.onContextLoss(function () { gl.dispose(); });
      term.loadAddon(gl);
    } catch (e) { /* the DOM renderer stays */ }
    term.onData(function (text) { sendInput(m, text); });
    m.term = term;
    m.fit = fit;
    m.holder = holder;
  });
}

/* A terminal wears its own palette whatever the panel theme is doing — this is ttyd's
   xterm theme, used verbatim because it is the most-copied xterm.js palette and its
   bg/fg/cursor and 16 ANSI colors are tuned as a set (borrowing half of it is how you
   get a terminal that looks almost right). */
function termTheme() {
  return {
    foreground: "#d2d2d2", background: "#2b2b2b", cursor: "#adadad",
    black: "#000000", red: "#d81e00", green: "#5ea702", yellow: "#cfae00",
    blue: "#427ab3", magenta: "#89658e", cyan: "#00a7aa", white: "#dbded8",
    brightBlack: "#686a66", brightRed: "#f54235", brightGreen: "#99e343",
    brightYellow: "#fdeb61", brightBlue: "#84b0d8", brightMagenta: "#bc94b7",
    brightCyan: "#37e6e8", brightWhite: "#f1f1f0",
  };
}

var encoder = null;
function sendInput(m, text) {
  if (!m.ws || m.ws.readyState !== 1) return;   // keys typed into a reconnect gap are dropped
  encoder = encoder || new TextEncoder();
  m.ws.send(encoder.encode(text));
}

/* Connect one session with a ticket. The ticket is spent by the gateway BEFORE the 101,
   so a refused one arrives as onclose with an HTTP story - the reconnect loop then finds
   the session either alive (mint again) or gone (say so). */
function connect(m, ticket) {
  var ws = new WebSocket(streamUrl(m.id, ticket));
  ws.binaryType = "arraybuffer";   // default would wrap every frame in a Blob
  m.ws = ws;
  m.status = "connecting\u2026";
  paintStatus();
  var my = epoch;
  ws.onopen = function () {
    if (my !== epoch) return;
    m.attempt = 0;
    m.status = "attached";
    paintStatus();
    scheduleFit();
  };
  ws.onmessage = function (event) {
    if (my !== epoch) return;
    if (typeof event.data === "string") {
      var frame = null;
      try { frame = JSON.parse(event.data); } catch (e) { return; }
      var story = frameStatus(frame);
      if (story) { m.status = story; paintStatus(); }
    } else if (m.term) {
      m.term.write(new Uint8Array(event.data));
    }
  };
  ws.onclose = function () {
    m.ws = null;
    if (my !== epoch || m.userClosed || m.gone) return;
    void reconnect(m);
  };
}

/* The grace-window half of the page: keep trying while the gateway still lists the
   session, back off between rounds, and stop the moment the story is final. Uses raw
   api() rather than apiJson() because a 503/404 here is an ANSWER, not something to
   toast about on every round. */
async function reconnect(m) {
  if (m.userClosed || m.gone) return;
  var my = epoch;
  m.status = "reconnecting\u2026";
  paintStatus();
  for (;;) {
    await sleep(nextReconnectDelay(m.attempt));
    m.attempt += 1;
    if (my !== epoch || m.userClosed || m.gone) return;

    var listed = await api(sessionsUrl());
    if (my !== epoch) return;
    if (listed.status === 503) { return gone(m, "the terminal plugin is not running"); }
    if (!listed.ok) { continue; }
    var listing = await listed.json().catch(function () { return []; });
    if (!sessionAlive(listing, m.id)) {
      return gone(m, "the session closed while the socket was down (grace window, idle or stall timeout)");
    }

    var minted = await api(ticketUrl(m.id), { method: "POST" });
    if (my !== epoch) return;
    if (minted.status === 404) { return gone(m, "the session closed while the socket was down"); }
    if (minted.status === 503) { return gone(m, "the terminal plugin is not running"); }
    if (!minted.ok) { continue; }
    var body = await minted.json().catch(function () { return {}; });
    if (body.ticket) { connect(m, body.ticket); return; }
  }
}

function gone(m, story) {
  m.gone = true;
  m.status = story;
  paintStatus();
  paintTabs();
}

/* Fit the on-stage terminal and tell the gateway. Two channels on purpose (docs/14
   §8): the resize frame while the socket is up, POST /resize while it is down - the
   panel's window may have changed during exactly that gap. */
function fitNow() {
  fitTimer = null;
  var m = model(active);
  if (!m || !m.fit || !m.term) return;
  var dims = null;
  try { dims = m.fit.proposeDimensions(); } catch (e) { return; }
  if (!dims || !dims.cols || !dims.rows) return;
  var g = clampGeometry(dims.cols, dims.rows);
  m.fit.fit();
  if (m.gone || !m.id || m.id.indexOf("pending-") === 0) return;
  if (g.cols === m.sentCols && g.rows === m.sentRows) return;
  m.sentCols = g.cols;
  m.sentRows = g.rows;
  if (m.ws && m.ws.readyState === 1) {
    m.ws.send(resizeFrame(g.cols, g.rows));
  } else {
    void api(resizeUrl(m.id), { method: "POST", body: JSON.stringify({ cols: g.cols, rows: g.rows }) });
  }
}

function scheduleFit() {
  if (fitTimer) return;
  fitTimer = setTimeout(fitNow, 150);
}

/* Selecting a tab: wire the terminal if it is the first look, stage it, and refit -
   the container was hidden or absent when the terminal was created. */
function select(id) {
  active = id;
  paintTabs();
  paintStage();
  var m = model(id);
  if (!m) {
    var row = sessions.find(function (s) { return s && s.id === id; });
    if (!row) return;
    // label from the listing row: without it the tab falls back to the raw target
    // UUID - exactly what a second page adopting this session used to show.
    m = { id: id, target: row.target, label: row.label, status: "connecting\u2026", attempt: 0, userClosed: false, gone: false, sentCols: 0, sentRows: 0 };
    models.push(m);
    paintTabs();
  }
  void wireTerminal(m).then(function () {
    if (active !== m.id) return;
    paintStage();
    if (!m.ws && !m.gone && !m.userClosed) void resume(m);
    scheduleFit();
    if (m.term) m.term.focus(); // switching tabs types into the one you switched to
  }).catch(function (error) {
    toast(String(error && error.message || error), true);
  });
}

/* Attach to a session that already exists (returning to the page, or a session opened
   in another browser tab): mint a ticket and connect. */
async function resume(m) {
  var my = epoch;
  var minted = await api(ticketUrl(m.id), { method: "POST" });
  if (my !== epoch || m.ws || m.gone || m.userClosed) return;
  if (minted.status === 404 || minted.status === 503) { return gone(m, minted.status === 503 ? "the terminal plugin is not running" : "the session is gone"); }
  if (!minted.ok) { m.status = "could not reach the gateway"; paintStatus(); return; }
  var body = await minted.json().catch(function () { return {}; });
  if (body.ticket) connect(m, body.ticket);
}

/* Open a fresh session: create the terminal first so fit can measure a real container,
   then open with the measured geometry. */
async function openSession() {
  var pick = $("term-target");
  if (!pick || !pick.value) return;
  var my = epoch;
  // The short tab label comes from the picker row ("jdoe-demo"), not the target id -
  // a remote session without this showed its raw connection UUID on the tab.
  var row = targetRows(targets).rows.find(function (r) { return r.id === pick.value; });
  var m = {
    id: "pending-" + (++seq), target: pick.value, label: row ? String(row.label).split(" \u00b7 ")[0] : "",
    status: "opening\u2026",
    attempt: 0, userClosed: false, gone: false, sentCols: 0, sentRows: 0,
  };
  models.push(m);
  active = m.id;
  paintTabs();
  paintStage();
  try {
    await wireTerminal(m);
  } catch (e) {
    /* A vendored package that failed to load must say so - a silently vanishing tab teaches
       the user that Open is decorative. */
    toast("could not load the terminal packages: " + String(e && e.message || e), true);
    models.splice(models.indexOf(m), 1);
    active = null;
    paintTabs();
    paintStage();
    return;
  }
  if (my !== epoch) return;
  paintStage();
  var g = { cols: 80, rows: 24 };
  try {
    var dims = m.fit.proposeDimensions();
    if (dims && dims.cols && dims.rows) g = clampGeometry(dims.cols, dims.rows);
    m.fit.fit();
  } catch (e) { /* the defaults already stand */ }
  m.status = "opening\u2026";
  paintStatus();
  var reply = await apiJson(sessionsUrl(), {
    method: "POST",
    body: JSON.stringify({ target: m.target, cols: g.cols, rows: g.rows }),
  });
  if (my !== epoch) return;
  if (!reply || !reply.id) {   // the toast said why; drop the shell we staged
    if (m.term) m.term.dispose();
    if (m.holder) m.holder.remove();
    models.splice(models.indexOf(m), 1);
    active = models.length ? models[models.length - 1].id : null;
    paintTabs();
    paintStage();
    paintStatus();
    return;
  }
  /* The model's id changes here (pending-N -> the gateway's id); everything keyed on
     the id must follow in the same breath. active is the one that is easy to forget,
     and forgetting it kills the status line AND hides the holder for every fresh
     open: model(active) stops resolving, paintStatus paints "", and paintStage's
     data-term match hides all holders. Found by the browser gate's status trail. */
  var wasActive = active === m.id;
  m.id = String(reply.id);
  if (wasActive) active = m.id;
  m.holder.setAttribute("data-term", m.id);
  paintTabs();
  paintStage();
  if (reply.ticket) connect(m, reply.ticket);
  /* Keyboard focus moves INTO the new terminal. It stayed on the Open button, so the Enter
     a user reaches for to get a prompt opened a second session on the same host instead -
     and a third pressed the per-target cap. Every real terminal focuses the shell it opens. */
  if (m.term && active === m.id) m.term.focus();
}

/* Closing a tab: a live session is DELETEd (the gateway writes the visible reason into
   the stream before it closes); a closed one is only removed locally - the session is
   already gone, a DELETE would just be a 404. */
async function closeSession(id) {
  var m = model(id);
  dismissed.add(id);   // stays dismissed until the listing itself drops the row
  if (m && m.term) { m.term.dispose(); }
  if (m && m.holder) { m.holder.remove(); }
  models = models.filter(function (x) { return x.id !== id; });
  if (active === id) active = models.length ? models[models.length - 1].id : null;
  paintTabs();
  paintStage();
  paintStatus();
  if (!m || m.gone) return;
  m.userClosed = true;
  if (m.ws) { try { m.ws.close(); } catch (e) { /* already gone */ } }
  await apiJson(sessionsUrl() + "/" + encodeURIComponent(id), { method: "DELETE" });
}

/* The page: one bar (tabs left, picker + Open right), the dark surface, the status
   foot. No pane-head, no description paragraph — the nav tab already says Terminal,
   and every real web terminal spends its top row on tabs, not on prose. */
function render() {
  var pane = $("pane");
  if (!pane) return;
  var pick = targetRows(targets);
  pane.innerHTML = '<div class="term-page">' +
    '<div class="term-bar">' +
      '<div class="term-tabs" id="term-tabs" role="tablist" aria-label="Sessions" hidden></div>' +
      '<div class="term-ctl">' +
        (pick.rows.length
          ? '<select id="term-target" class="term-pick" aria-label="Target">' +
            pick.rows.map(function (r) {
              return '<option value="' + esc(r.id) + '">' + esc(r.label) + (r.state ? " (" + esc(r.state) + ")" : "") + "</option>";
            }).join("") + "</select>" +
            '<button class="btn term-new" id="term-new">Open session</button>'
          : '<span class="term-none">' + esc(pick.note) + "</span>") +
      "</div></div>" +
    '<div class="term-stage" id="term-stage">' +
      '<div class="term-empty" id="term-empty" hidden>' +
        '<div class="term-ghost" aria-hidden="true"><span class="term-ghost-dollar">$</span><span class="term-ghost-cursor"></span></div>' +
        '<h2>No session yet</h2>' +
        "<p>Pick a host in the bar above and open one.</p>" +
        "<p>A dropped socket does not end a session \u2014 it waits out the grace window and catches up.</p>" +
      "</div>" +
    "</div>" +
    '<div class="term-foot" id="term-status"></div>' +
    "</div>";

  var button = $("term-new");
  if (button) button.onclick = function () { void openSession(); };
  var tabs = $("term-tabs");
  if (tabs) {
    tabs.onclick = function (event) {
      var closer = event.target.closest('[data-act="close"]');
      if (closer) { void closeSession(closer.getAttribute("data-id")); return; }
      var tab = event.target.closest('[data-act="select"]');
      if (tab) select(tab.getAttribute("data-id"));
    };
  }
  window.addEventListener("resize", scheduleFit);
  paintTabs();
  paintStage();
  if (!active && sessions.length) select(sessions[0].id);
  else if (!active) { var empty = $("term-empty"); if (empty) empty.hidden = false; }
}

async function reload() {
  var t = await apiJson(targetsUrl());
  if (t) targets = t;
  var r = await api(sessionsUrl());
  if (r.status === 503) {
    sessions = [];
  } else if (r.ok) {
    var body = await r.json().catch(function () { return []; });
    sessions = Array.isArray(body) ? body : [];
  }
  // A dismissed id the listing no longer carries will never come back; forget it.
  var listed = new Set(sessions.map(function (s) { return s && s.id; }));
  for (var d of Array.from(dismissed)) if (!listed.has(d)) dismissed.delete(d);
  render();
}

/* The shell has no unmount hook; mounting again is the only signal that the last
   page's terminals are unreachable. Without this, every visit away and back leaked the
   previous mount whole: the WebSockets stayed open (so the gateway's grace clock never
   started - the session read as attached forever) and each xterm kept its canvases and
   its WebGL context for the life of the tab. The gateway sessions themselves stay -
   re-entry adopts them again; only the client-side resources are released here. */
function releaseModels() {
  models.forEach(function (m) {
    if (m.ws) { try { m.ws.close(); } catch (e) { /* already gone */ } }
    if (m.term) { try { m.term.dispose(); } catch (e) { /* already gone */ } }
    if (m.holder) { m.holder.remove(); }
    m.ws = null; m.term = null; m.holder = null;
  });
}

export async function mount() {
  epoch += 1;               // stale callbacks from the last mount die here
  releaseModels();
  models = [];
  active = null;
  dismissed = new Set();   // a fresh mount knows nothing of the last page's tabs
  await reload();
}

export async function refresh() { await reload(); }

/** The shell's slow cadence: republish the listing so tabs pick up bytesOut and the
 *  attached dot, and let a staged-but-unwired listing row stay honest. */
export async function poll() {
  if (!$("term-stage")) return;
  var r = await api(sessionsUrl());
  if (r.status === 503) return;   // the navigation chrome already carries the story
  if (!r.ok) return;
  var body = await r.json().catch(function () { return []; });
  sessions = Array.isArray(body) ? body : [];
  var listed = new Set(sessions.map(function (s) { return s && s.id; }));
  for (var d of Array.from(dismissed)) if (!listed.has(d)) dismissed.delete(d);
  paintTabs();
}

export function countText() {
  return sessions.length ? sessions.length + " live" : "";
}

export function unmount() {
  epoch += 1;
  if (fitTimer) { clearTimeout(fitTimer); fitTimer = null; }
  window.removeEventListener("resize", scheduleFit);
  models.forEach(function (m) {
    if (m.ws) { try { m.ws.close(); } catch (e) { /* already gone */ } }
    if (m.term) { try { m.term.dispose(); } catch (e) { /* already gone */ } }
  });
  models = [];
  active = null;
  sessions = [];
  targets = null;
}
