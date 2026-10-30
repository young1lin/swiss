// Real-browser walk for the llm Test dialogue (contract v3), pinning the earned
// contract surface. Every step is a real pointer click or real text input; every
// assertion is a visible outcome (text on screen, a control actually disabled, a
// sheet actually open) - never an element-existence proof. The transcript is
// asserted semantically through the tc-col turns: exact user/final role labels
// (llm.history.role.*), exact body texts, ordered, with positive rects - the
// library Run/resume metadata rows (llm.test.summary) are excluded by ROLE label.
// A passive PH4 trace observes the Fresh click window: allowlisted network
// method/path/status plus known-control clicks - observations only, never root
// attribution, never headers/cookies/ticket/key/body/config/query/text.
//
// CANCEL_SEL is the panel owner's FINAL id for the stop control (fe4b).
import assert from "node:assert/strict";

const { api } = await import("./helpers.mjs");
const { withPanel } = await import("./browser.mjs");
const { fakeProvider } = await import("./fakeprov.mjs");

const URL_BASE = "http://127.0.0.1:" + (process.env.SWISS_E2E_PORT || "18999");
const CANCEL_SEL = "#laTestStop";

function putRow(change) {
  const cur = api("GET", "/api/plugins/llm/config");
  assert.equal(cur.status, 0, "read the llm row: " + cur.stderr);
  const config = (cur.body && cur.body.config) || {};
  change(config);
  const put = api("PUT", "/api/plugins/llm/config", JSON.stringify({ revision: cur.body.revision, config }));
  assert.equal(put.status, 0, "apply the llm row: " + put.stderr);
  if (put.body && put.body.error) throw new Error("row not applied: " + put.body.error);
}

// The dialog should hand the prompt box back empty after a send; if it does not
// (a behavior gap worth failing on its own), clear it so the next turn's input
// is exactly this turn's text.
async function promptCleared(b) {
  const empty = await b.eval("document.querySelector('#laTestPrompt').value === ''");
  if (!empty) await b.eval("(() => { document.querySelector('#laTestPrompt').value = ''; return true; })()");
  return empty;
}

// Semantic transcript reads: the user/final turns the user actually said and
// saw, in order, through the tc-col rows - the panel's own role labels
// (llm.history.role.user / .final; metadata rows carry llm.test.summary and are
// excluded by role, never by markup changes).
const ROLE_USER = "user";
const ROLE_FINAL = "final";
async function semanticTurns(b) {
  const src = "(() => { const rows = Array.from(document.querySelectorAll('#laTestOut .tc-turn'));"
    + " const sem = rows.filter((r) => { const role = ((r.querySelector('.tc-role') || {}).textContent || '').trim();"
    + " return role === '" + ROLE_USER + "' || role === '" + ROLE_FINAL + "'; });"
    + " return sem.map((r) => ({ role: ((r.querySelector('.tc-role') || {}).textContent || '').trim(),"
    + " text: (((r.querySelector('.tc-md') || r.querySelector('.tc-text') || {}).textContent) || '').trim(),"
    + " w: Math.round(r.getBoundingClientRect().width), h: Math.round(r.getBoundingClientRect().height) })); })()";
  return b.eval(src);
}
function assertSem(list, want, what) {
  assert.deepEqual(list.map((t) => t.role + ":" + t.text), want, what);
  for (const t of list) {
    assert.ok(t.w > 0 && t.h > 0, what + ": every semantic row has a positive bounding rect");
  }
}

// ---------------------------------------------------------------------------
// PH4 passive trace, inline (prep-reviewed): observations around the Fresh
// click. Network: method + sanitized pathname + numeric status ONLY, for the
// five allowlisted same-origin routes (query dropped); strict tab-session
// equality; armed after the ticket navigation has landed. Clicks: one passive
// capture listener echoing ONLY the known fake controls (or the fixed label
// 'other'); never simulated, never arbitrary ids/text/coordinates. Counts are
// observations - root attribution is never drawn here.
// ---------------------------------------------------------------------------
const TRACE_ALLOW = [
  [/^\/api\/llm\/history\/runs$/, "discovery"],
  [/^\/api\/llm\/history\/runs\/[0-9]+\/dialogue$/, "dialogue"],
  [/^\/api\/llm\/history\/runs\/[0-9]+\/close$/, "close"],
  [/^\/api\/llm\/agents\/test\/cancel$/, "cancel"],
  [/^\/api\/llm\/agents\/test\/stream$/, "stream"],
];
function traceInstall(b) {
  const tab = b.session;
  const net = [];
  const statusIdx = new Map();
  const sanitize = (url) => {
    let u;
    try { u = new URL(url); } catch { return null; }
    if (u.origin !== URL_BASE) return null;
    for (const [re, kind] of TRACE_ALLOW) {
      if (re.test(u.pathname)) return { kind, path: u.pathname };
    }
    return null;
  };
  b.on("Network.requestWillBeSent", (p, sid) => {
    if (sid !== tab) return;
    const req = p.request || {};
    const hit = sanitize(req.url);
    if (!hit) return;
    statusIdx.set(p.requestId, net.length);
    net.push({ seq: net.length, kind: hit.kind, method: req.method, path: hit.path, status: 0 });
  });
  b.on("Network.responseReceived", (p, sid) => {
    if (sid !== tab) return;
    const idx = statusIdx.get(p.requestId);
    if (idx === undefined) return;
    net[idx].status = (p.response && p.response.status) || 0;
  });
  return {
    async arm() {
      await b.send("Network.enable", {}, tab);
      await b.eval("(() => { window.__ph4Clicks = []; window.__ph4Seq = 0;" +
        " const KNOWN = ['#laTestRun','#laTestFresh','#laTestStop','#laTestClose','#laTestPrompt'];" +
        " document.addEventListener('click', (ev) => {" +
        " const t = ev.target; let target = 'other';" +
        " for (const sel of KNOWN) { if (t.closest(sel)) { target = sel; break; } }" +
        " if (target === 'other' && t.closest('[data-latest=\"echo\"]')) target = 'data-latest:echo';" +
        " window.__ph4Clicks.push({ seq: window.__ph4Seq++, target });" +
        " }, { capture: true, passive: true }); return true; })()");
    },
    netMark: () => net.length,
    clickMark: () => b.eval("window.__ph4Clicks.length"),
    async snapshot(markNet, markClick) {
      // Deep copy each event: a later status update must never mutate a frozen
      // snapshot (slice() alone would share the objects).
      const frozen = net.map((e) => ({ ...e }));
      const clicks = await b.eval("window.__ph4Clicks");
      return { net: frozen, clicks, freshNetMark: markNet, freshClickMark: markClick };
    },
  };
}
function traceLines(s) {
  const out = [];
  for (const e of s.net) {
    out.push("PH4NET seq=" + e.seq + " " + e.kind + " " + e.method + " " + e.path + " status=" + (e.status || "-"));
  }
  for (const c of s.clicks) {
    out.push("PH4CLICK seq=" + c.seq + " " + c.target + (c.seq === s.freshClickMark ? " <<FRESH" : ""));
  }
  const closePosts = s.net.filter((e) => e.kind === "close" && e.method === "POST");
  const postMark = s.net.filter((e) => e.seq >= s.freshNetMark);
  out.push("PH4SUMMARY closePosts=" + closePosts.length +
    " closeStatus=" + (closePosts.length ? closePosts[0].status : "-") +
    " postMarkDiscovery=" + postMark.filter((e) => e.kind === "discovery").length +
    " postMarkDialogue=" + postMark.filter((e) => e.kind === "dialogue").length +
    " freshClickSeq=" + s.freshClickMark +
    " clickTotal=" + s.clicks.length);
  return out;
}

await withPanel(async (b) => {
  const fake = await fakeProvider({ models: ["fake-echo"] });
  // PH4 trace state, declared before the try so the finally can print it.
  const trace = traceInstall(b);
  let snap = null;
  let traceTaken = false;
  let freshNetMark = -1;
  let freshClickMark = -1;
  try {
    putRow((config) => {
      config.providers = { fake: {
        style: "openai", baseUrl: fake.url + "/v1",
        models: [{ id: "fake-echo" }],
      } };
      config.agents = { echo: { provider: "fake", model: "fake-echo" } };
    });
    fake.script("fake-echo",
      { mode: "answer", text: "B-ANS1-t1" },
      { mode: "answer", text: "B-ANS2-t2" },
      { mode: "hang", then: { mode: "answer", text: "B-late" } },
      { mode: "answer", text: "B-ANS4-t4" });

    // A fresh page load now that the row exists.
    await b.navigate(URL_BASE + "/?walk=llmchat#llm-agents");
    await b.waitFor("(() => document.querySelectorAll('[data-latest]').length > 0)()");

    // PH4: arm once the page document is final (the ticket navigation landed
    // long before; no reload happens from here), so the passive recorder
    // survives the walk.
    await trace.arm();

    // Open the Test dialog with a real click. #laTestOut is a lazy transcript
    // slot created on first paint of an answer, so it is probed after the
    // first exchange below - the visible sheet carries the dialog at open.
    await b.click('[data-latest="echo"]');
    await b.visibleOk("#sheet");
    await b.visibleOk("#laTestPrompt");
    await b.visibleOk("#laTestRun");
    await b.visibleOk("#laTestClose");

    // Turn 1: type, click Ask with a real pointer, watch the answer land.
    await b.type("#laTestPrompt", "B-S1-turn1");
    await b.click("#laTestRun");
    await b.waitFor("(() => document.querySelector('#laTestOut').textContent.includes('B-ANS1-t1'))()");
    await b.visibleOk("#laTestOut");
    assertSem(await semanticTurns(b), [ROLE_USER + ":B-S1-turn1", ROLE_FINAL + ":B-ANS1-t1"], "turn 1 renders a user turn and a final answer, in order");
    const cleared1 = await promptCleared(b);

    // Turn 2 on the same open conversation: the column accumulates.
    await b.type("#laTestPrompt", "B-S2-turn2");
    await b.click("#laTestRun");
    await b.waitFor("(() => document.querySelector('#laTestOut').textContent.includes('B-ANS2-t2'))()");
    assertSem(await semanticTurns(b), [ROLE_USER + ":B-S1-turn1", ROLE_FINAL + ":B-ANS1-t1", ROLE_USER + ":B-S2-turn2", ROLE_FINAL + ":B-ANS2-t2"], "turn 2 accumulates the same conversation in order");
    await b.visibleOk("#laTestFresh");

    // While a turn is live: the Ask button is busy, New is disabled (contract),
    // and the cancel control is visible and stops the round trip.
    await b.type("#laTestPrompt", "B-S3-turn3");
    await b.click("#laTestRun");
    await b.waitFor("(() => !!document.querySelector('#laTestRun').disabled)()");
    const live = await b.eval("(() => { const fresh = document.querySelector('#laTestFresh'); return { freshDisabled: fresh ? !!fresh.disabled : null }; })()");
    assert.equal(live.freshDisabled, true, "New is disabled while a turn is live");
    await b.visibleOk(CANCEL_SEL);
    // Journal-gated before the REAL Stop click: the disabled button is synchronous
    // (onRun sets it before the provider round trip exists), so the arrival proof is
    // the fake's journal - exactly three chats, one held gate, turn 3's prompt on it.
    const hangDeadline = Date.now() + 8000;
    for (;;) {
      if (fake.chats().length === 3 && fake.hangCount() === 1) break;
      if (Date.now() > hangDeadline) throw new Error("S3 hang never arrived; chats=" + fake.chats().length + " hangs=" + fake.hangCount());
      await new Promise((r) => setTimeout(r, 50));
    }
    const held = fake.chats()[2];
    const heldTurn = held && held.body && Array.isArray(held.body.messages) ? held.body.messages[held.body.messages.length - 1] : null;
    assert.ok(heldTurn && heldTurn.role === "user" && String(heldTurn.content) === "B-S3-turn3", "the held round trip carries turn 3's prompt");
    await b.click(CANCEL_SEL);
    await b.waitFor("(() => { const run = document.querySelector('#laTestRun'); const fresh = document.querySelector('#laTestFresh'); return !run.disabled && fresh && !fresh.disabled; })()");
    assert.equal(await promptCleared(b), cleared1, "the prompt box behaves the same after cancel as after a send");
    // The abort must REACH the provider before New is touched: the gate goes away
    // and the journal marks the third entry aborted (fakeprov's socket-close hook).
    const abortDeadline = Date.now() + 8000;
    for (;;) {
      if (fake.hangCount() === 0 && fake.chats()[2] && fake.chats()[2].aborted === true) break;
      if (Date.now() > abortDeadline) throw new Error("S3 abort never reached the provider; hangs=" + fake.hangCount() + " aborted=" + (fake.chats()[2] ? String(!!fake.chats()[2].aborted) : "no-entry"));
      await new Promise((r) => setTimeout(r, 50));
    }

    // New asks the server to close the open run and clears the column; the
    // next send starts a wholly fresh state.
    // PH4 marks: click count (an awaited CDP read) first, then the net length,
    // both immediately before the REAL pointer click - two sequential
    // observations around it, not an atomic same-tick CDP guarantee.
    freshClickMark = await trace.clickMark();
    freshNetMark = trace.netMark();
    traceTaken = true;
    try {
      await b.click("#laTestFresh");
      await b.waitFor("(() => Array.from(document.querySelectorAll('#laTestOut .tc-turn')).every((r) => { const role = ((r.querySelector('.tc-role') || {}).textContent || '').trim(); return role !== '" + ROLE_USER + "' && role !== '" + ROLE_FINAL + "'; }))()");
      assertSem(await semanticTurns(b), [], "New clears every semantic turn");
    } finally {
      // Freeze even when the New wait/assert fails, while the document is
      // still alive; a snapshot failure must never mask the original assert.
      try {
        snap = await trace.snapshot(freshNetMark, freshClickMark);
      } catch {
        snap = null;
      }
    }
    await b.type("#laTestPrompt", "B-S4-turn4");
    await b.click("#laTestRun");
    await b.waitFor("(() => document.querySelector('#laTestOut').textContent.includes('B-ANS4-t4'))()");
    assertSem(await semanticTurns(b), [ROLE_USER + ":B-S4-turn4", ROLE_FINAL + ":B-ANS4-t4"], "the fresh state starts at one exchange");
    // Fixed-count observable: the fake saw exactly four provider chats and the
    // fourth carries turn 4's prompt - the hang was consumed by S3, not by S4.
    assert.equal(fake.chats().length, 4, "exactly four provider chats after S4");
    // Gate C v2 (PH6): the fresh send's provider wire, pinned to the server's
    // first-turn contract (llm_api.rs :755-761 prompt + NLNL + test_addendum;
    // exact join pinned server-side at :1688). Drift here fails, never adapts.
    const freshWire = fake.chats()[3];
    assert.ok(freshWire && freshWire.body && Array.isArray(freshWire.body.messages) && freshWire.body.messages.length > 0,
      "the fourth chat carries a real messages array");
    const roles = freshWire.body.messages.map((m) => (m && m.role) || "");
    assert.equal(roles[roles.length - 1], "user", "the fresh ask's terminal message is the user turn");
    assert.equal(roles.filter((r) => r === "user").length, 1, "the fresh conversation carries exactly one user turn");
    // Frozen mirror of test_addendum(["read","list"]) - llm_api.rs :385-393; the
    // fixture's echo takes the whole OFFER (llm_tools.rs :42, agent.rs :391).
    const T4_ADDENDUM = "## This test\nread opens one file under the user's home directory (an absolute path inside it, or one relative to it) and list names one directory's entries there with type, size and modified time. Check with them rather than guess.";
    const tail = freshWire.body.messages[freshWire.body.messages.length - 1];
    assert.equal(String(tail && tail.content), "B-S4-turn4\n\n" + T4_ADDENDUM,
      "the fresh ask's user message is exactly turn 4's prompt + the pinned first-turn addendum");
    let t4Occurrences = 0;
    for (const m of freshWire.body.messages) {
      const text = String((m && m.content) || "");
      t4Occurrences += text.split("B-S4-turn4").length - 1;
      for (const stale of ["B-S1-turn1", "B-ANS1-t1", "B-S2-turn2", "B-ANS2-t2", "B-S3-turn3"]) {
        assert.ok(!text.includes(stale), "the fresh ask carries no stale turn text: " + stale);
      }
    }
    assert.equal(t4Occurrences, 1, "turn 4's prompt text appears exactly once on the fresh wire");

    // A full reload of the page restores the last open conversation.
    await b.navigate(URL_BASE + "/?walk=llmchat2#llm-agents");
    await b.waitFor("(() => document.querySelectorAll('[data-latest]').length > 0)()");
    await b.click('[data-latest="echo"]');
    await b.waitFor("(() => document.querySelector('#laTestOut').textContent.includes('B-ANS4-t4'))()");

    // The history page shows the conversation; the run sheet opens visibly and
    // carries the same words the user saw.
    await b.navigate(URL_BASE + "/#llm-history");
    await b.waitFor("(() => document.querySelectorAll('[data-hrun]').length > 0)()");
    await b.click("[data-hrun]");
    await b.visibleOk("#lhRunCopy");
    await b.waitFor("(() => (document.querySelector('#sheet') || { textContent: '' }).textContent.includes('B-ANS4-t4'))()");
  } finally {
    // PH4: emit the frozen observation window (or the fixed unavailable line)
    // before closing the fake - additive diagnosis, never masking a failure.
    if (snap) {
      for (const line of traceLines(snap)) console.log(line);
    } else if (traceTaken) {
      console.log("PH4TRACE unavailable: the snapshot failed before freeze");
    }
    // Fixed-label numeric journal state before the fake closes - counts and aborted
    // flags only; never bodies, headers, URLs, keys or prompt values.
    const jc = fake.chats();
    console.log("PH4JOURNAL chats=" + jc.length + " hangs=" + fake.hangCount() + " abortedFlags=" + jc.map((e) => e.aborted === true ? "1" : "0").join(""));
    await fake.close();
  }
}, { plugin: "llm", view: "#llm-agents" });

console.log("panel-llm-chat: ok");
