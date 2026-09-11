/* The terminal view's pure half (docs/14 §8): URL building, reconnect pacing, the
   close-frame stories, geometry clamping, the target-picker rows, and the Windows-Terminal
   key/mouse actions (docs/15 §1). Plain data in,
   plain data out — the contract with the gateway's Rust side is pinned by the tests in
   test/admin-terminal.test.ts, not by clicking. The DOM/xterm/WS wiring stays in
   views/terminal.js; everything a test needs to trust lives here. */

var PTY_MIN = 1;
var PTY_MAX = 1000;   // the bounds lmg-host enforces on both axes — a 0-column PTY is
                      // undefined behaviour on the far side, so the panel clamps BEFORE
                      // the gateway has to refuse (docs/14 §8)

export function targetsUrl() { return "/api/terminal/targets"; }
export function sessionsUrl() { return "/api/terminal/sessions"; }
export function sessionUrl(id) { return sessionsUrl() + "/" + encodeURIComponent(id); }
export function ticketUrl(id) { return sessionUrl(id) + "/ticket"; }
export function resizeUrl(id) { return sessionUrl(id) + "/resize"; }
export function streamUrl(id, ticket) {
  /* Absolute, always: the WebSocket constructor rejects a relative URL with a SyntaxError
     before any connection is attempted. The page origin decides ws/wss; outside a browser
     (node tests) there is no location and the bare path comes back, which keeps this pure. */
  var base = "";
  if (typeof location !== "undefined" && location && location.host) {
    base = (location.protocol === "https:" ? "wss://" : "ws://") + location.host;
  }
  return base + sessionUrl(id) + "/stream?ticket=" + encodeURIComponent(ticket);
}

/** Reconnect pacing after a socket drops. Quick first retry (a lid-close round trip is
 *  short), then doubling to a cap — inside the server's 60 s grace the socket keeps
 *  trying without hammering the mint route. attempt is 0-based. */
export function nextReconnectDelay(attempt) {
  if (!(attempt >= 0)) attempt = 0;
  return Math.min(500 * Math.pow(2, attempt), 5000);
}

/** What the status line says for a server text frame. null = not a control frame we
 *  narrate (binary is the terminal's own bytes; they need no caption). */
export function frameStatus(frame) {
  if (!frame || typeof frame !== "object") return null;
  if (frame.t === "stalled") return "stalled — the client is not accepting output";
  if (frame.t === "error") return frame.message ? String(frame.message) : "closed";
  if (frame.t === "exit") {
    return frame.code == null ? "the shell exited" : "the shell exited with code " + frame.code;
  }
  return null;
}

/** True while the session is still in the gateway's listing — the listing is the truth:
 *  a session that left it is past its grace window, timed out, or was deleted, and no
 *  ticket will ever bring it back (docs/14 §6.7). */
export function sessionAlive(listing, id) {
  return Array.isArray(listing) && listing.some(function (s) { return s && s.id === id; });
}

/** Clamp the fit addon's proposal to the PTY bounds before it is SENT, so an honest
 *  resize never becomes a 400. Unknown/absent values fall back to the classics. */
export function clampGeometry(cols, rows) {
  function axis(v, fallback) {
    var n = Math.round(Number(v));
    if (!isFinite(n)) n = fallback;
    return Math.min(Math.max(n, PTY_MIN), PTY_MAX);
  }
  return { cols: axis(cols, 80), rows: axis(rows, 24) };
}

/** The resize control frame as the wire wants it: one small JSON object, the only text
 *  the client is allowed to mean anything with (docs/14 §8). */
export function resizeFrame(cols, rows) {
  var g = clampGeometry(cols, rows);
  return JSON.stringify({ t: "resize", cols: g.cols, rows: g.rows });
}

/** Rows for the target picker, in order: local (if the config says so), then every
 *  remote target the shell seat reports. A remote side that is absent explains ITSELF
 *  with the reason the gateway sent — "the tunnels plugin is disabled", never a bare
 *  empty list (docs/14 §4). Returns { rows, note } — note is the one-line story under
 *  the picker when there is nothing remote to pick. */
export function targetRows(reply) {
  var rows = [];
  var note = "";
  var r = reply || {};
  if (r.local && r.local.enabled) {
    rows.push({ id: "local", label: "local · " + (r.local.shell || "shell") });
  }
  var remote = r.remote || {};
  var targets = Array.isArray(remote.targets) ? remote.targets : [];
  targets.forEach(function (t) {
    if (!t || !t.id) return;
    var where = t.host ? t.host + (t.port && t.port !== 22 ? ":" + t.port : "") : t.id;
    var who = t.username ? t.username + "@" : "";
    rows.push({
      id: String(t.id),
      label: (t.label || t.id) + " · " + who + where,
      state: t.state || "",
    });
  });
  if (!rows.length) {
    note = remote.presence === "absent" && remote.reason
      ? String(remote.reason)
      : "no terminal targets — connect a tunnel first, or enable the local shell in the plugin config";
  }
  return { rows: rows, note: note };
}

/** The picker's label for a session tab: short, monospace-friendly, stable. The listing
 *  rows carry the connection's label ("jdoe-demo"); without it a remote tab would show
 *  the raw connection UUID, which is noise, not a name. */
export function sessionLabel(session) {
  if (!session) return "?";
  if (session.label) return String(session.label);
  var t = session.target || "?";
  return t === "local" ? "local" : t;
}

/** What one keyboard event means inside the terminal, Windows Terminal style (docs/15
 *  §1): "paste" | "copy" | "sigint" | null, where null means "not ours — xterm keeps the
 *  event". Takes a duck-typed { key, code, ctrlKey, shiftKey, altKey, metaKey, type }
 *  rather than a KeyboardEvent so a test can press any combination without a DOM, and
 *  judges keydown ONLY: xterm hands keypress and keyup to the custom handler too, and
 *  deciding on those would fire every action twice. Ctrl+C is the load-bearing
 *  asymmetry — with a selection it copies (and must NOT reach the shell as ^C), without
 *  one it stays the interrupt a flooding program is counting on. */
export function keyAction(ev, hasSelection) {
  if (!ev || ev.type !== "keydown") return null;
  if (ev.metaKey || ev.altKey) return null;   // mac paste and the browser's own menu combos
  var key = String(ev.key || "").toLowerCase();
  if (ev.ctrlKey) {
    if (key === "v") return "paste";                                // Ctrl+V, Ctrl+Shift+V
    if (key === "c") {
      if (ev.shiftKey) return hasSelection ? "copy" : null;         // Ctrl+Shift+C
      return hasSelection ? "copy" : "sigint";                      // Ctrl+C
    }
    if (key === "insert") return hasSelection ? "copy" : null;      // Ctrl+Insert
    return null;
  }
  if (ev.shiftKey && key === "insert") return "paste";              // Shift+Insert
  return null;
}

/** What a right-click on the terminal surface means: paste with no selection, copy one
 *  away, and Shift+right-click keeps the browser's context menu as the escape hatch. Only
 *  button === 2 is judged — the wiring subscribes to contextmenu, whose button is the one
 *  that opened it. */
export function mouseAction(ev, hasSelection) {
  if (!ev || ev.button !== 2) return null;
  if (ev.shiftKey) return "menu";
  return hasSelection ? "copy" : "paste";
}
