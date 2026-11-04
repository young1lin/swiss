// Contract tests for the llm continuing-conversation wire (contract v3).
//
// These encode the FINAL wire and pin it as the backend lands it: they fail on
// real defects and must never be weakened to pass. Continuations carry their
// run id explicitly - an absent runId means a NEW run. Every scenario exercises
// its flow end to end - real streams,
// real history reads, real tool rounds - and asserts exact request chronology from
// the fake provider's journal, never a digest.
//
// Contract pinned here (parent handoff):
//   - the first SSE frame is the 'run' event {runId, agent, provider?, model,
//     continued}; 'done' carries the same runId plus ok/ms/turns/tokens or
//     error/cancelled.
//   - open-conversation state is read from the EXISTING runs list:
//     GET /api/llm/history/runs?agent=<encoded>&open=true&page=0&pageSize=1 -
//     there is NO /agents/test/open route (deleted from the design).
//   - POST /api/llm/agents/test/cancel answers {ok:true, cancelled:boolean};
//     cancel, await registry idle, then close (the panel New sequence) works.
//   - POST /api/llm/history/runs/{id}/close answers {ok:true}, 409 while the run
//     is live (and a second live stream answers 409 with a JSON code); old
//     history is retained for audit, the context resets.
//   - limits REJECT, never truncate.
//   - conversation calls carry an explicit run linkage.
//   - reasoning display is GENERIC here: a local alias gets no DeepSeek resend
//     promise (the conservative policy gates that on the exact official host;
//     Rust units cover the true branch against a mock private endpoint). The
//     e2e pins only that thinking streams visibly and files durably.
//   - bodies seal past the preview window: llm.db keeps a ~2 KiB plaintext
//     preview per row BY DESIGN; a marker beyond it must be absent from the
//     raw llm.db bytes (WAL and SHM sidecars included) while the unsealed
//     API body still carries it.
//   - a 160-delta burst must arrive whole: exact count, exact concatenation,
//     no silent frame skips in the relay (a bounded-32 skip is a bug, not a cap).
//   - honest cancellation: done{cancelled:true} always closes the stream, the
//     partial text the user saw stays in the record, a completed tool round
//     survives a mid-loop cancel, and the conversation continues with its
//     context intact.
//
// Runs only on the verification overlay (see e2e/README.md): 19998, the
// target-test exe, a dedicated fresh SWISS_E2E_HOME (helpers refuse anything
// else at import time). Never 19999. Node built-ins only.
import assert from "node:assert/strict";
import { existsSync, writeFileSync } from "node:fs";
import { join } from "node:path";

// A fake bearer for a loopback fake (no real secret anywhere): the row holds only
// the reference; set BEFORE helpers captures the environment for child processes.
process.env.SWISS_E2E_FAKE_KEY = "e2e-fake-bearer-6a1c-4f2e";
const helpers = await import("./helpers.mjs");
const { PORT, BASE, HOME, api, boot, stop, withGateway } = helpers;
// The mock key must reach the gateway explicitly (helpers snapshots the child
// env at import): a known-fake value only, never a real credential.
helpers.ENV.SWISS_E2E_FAKE_KEY = process.env.SWISS_E2E_FAKE_KEY;
// The builtin agent's read/list tools are pinned to the USER'S home root
// (USERPROFILE, then HOME, then "." - the same three-step paths::home_dir
// uses), which is SEPARATE from the gateway's state home. For THIS fixture
// only, both roots resolve to the SAME validated dedicated SWISS_E2E_HOME, so
// the tool round reads the note this test wrote. This is a fixture pin, not a
// production SWISS_HOME=root contract. helpers.ENV is what every child gateway
// (including each restart boot) inherits; the runner's own process.env keys
// are left untouched.
const ENV_PIN = ["USERPROFILE", "HOME"];
const ENV_RESTORE = ENV_PIN.map((key) => ({ key, had: key in helpers.ENV, was: helpers.ENV[key] }));
for (const key of ENV_PIN) helpers.ENV[key] = HOME;
for (const key of ENV_PIN) assert.equal(helpers.ENV[key], HOME, "the child env pins " + key + " to the dedicated e2e home");
const { fakeProvider } = await import("./fakeprov.mjs");
// The sealed-chunk engagement guard plus the shared known-answer constants of
// the long third answer: the fake script entry and the privacy block use ONE
// constant pair, defined once on the guard side.
const { assertStoredChunks, LONG_TAIL, LONG_ANSWER } = await import("./sealed-chunks.mjs");

const NL = String.fromCharCode(10);
const STREAM_PATH = "/api/llm/agents/test/stream";
const CANCEL_PATH = "/api/llm/agents/test/cancel";
const RUN_ID_FIELD = "runId"; // call rows carry an explicit run linkage; one constant if the wire spelling moves
const PROMPT_LIMIT = 2000;   // the route's own ceiling; limits reject, never truncate

// One fake for the whole file: its URL rides in the home's row, so it must stay
// listening across every gateway stop/start (the restart scenarios depend on it).
const fake = await fakeProvider({ models: ["fake-echo", "fake-tool", "fake-a", "fake-b", "fake-chat", "fake-reason", "fake-burst", "fake-cxltool", "fake-embed"] });

// ---------------------------------------------------------------------------
// Admin HTTP through the one-time ticket (no key ever handled here).
// ---------------------------------------------------------------------------
let adminCookie = null;
async function ticket() {
  if (adminCookie) return adminCookie;
  for (let i = 0; i < 5 && !adminCookie; i++) {
    const r = api("POST", "/api/session/ticket", "{}");
    if (r.status === 0 && r.body && r.body.url) {
      const spent = await fetch(r.body.url, { redirect: "manual" });
      const jar = typeof spent.headers.getSetCookie === "function" ? spent.headers.getSetCookie() : [spent.headers.get("set-cookie") || ""];
      const hit = jar.find((c) => c && c.includes("swiss_session"));
      if (hit) adminCookie = hit.split(";")[0];
    }
    if (!adminCookie) await new Promise((resolve) => setTimeout(resolve, 400));
  }
  if (!adminCookie) throw new Error("could not spend a session ticket for admin fetches");
  return adminCookie;
}
async function adminJson(method, path, body) {
  for (let attempt = 0; ; attempt++) {
    const jar = await ticket();
    const r = await fetch(BASE + path, {
      method,
      // The cookie proves browser provenance only with the Origin header of
      // this exact host:port; a cookie POST without it 403s by design.
      headers: { "content-type": "application/json", cookie: jar, origin: BASE },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    let parsed = null;
    try { parsed = await r.json(); } catch { parsed = null; }
    // A gateway restart rotates the session key: re-mint once and retry.
    if (r.status !== 401 || attempt >= 1) return { status: r.status, body: parsed };
    adminCookie = null;
  }
}
async function fetchStreamRaw(agent, prompt, runId) {
  for (let attempt = 0; ; attempt++) {
    const jar = await ticket();
    // The body names the run it continues: an absent runId means a NEW run,
    // and the gateway refuses a no-id ask while a continuable run is open
    // (409 open-run-exists) - the implicit join is deliberately gone.
    const payload = { agent };
    if (prompt !== undefined) payload.prompt = prompt;
    if (runId !== undefined && runId !== null) payload.runId = runId;
    const r = await fetch(BASE + STREAM_PATH, {
      method: "POST",
      // Same browser-provenance rule as adminJson: the cookie needs its
      // Origin header (exact host:port) or the gate refuses the POST.
      headers: { "content-type": "application/json", cookie: jar, origin: BASE },
      body: JSON.stringify(payload),
    });
    if (r.status !== 401 || attempt >= 1) return r;
    adminCookie = null; // a restart rotated the session key
  }
}

// ---------------------------------------------------------------------------
// SSE consumption: frames are "event:"/"data:" pairs; every event is kept.
// ---------------------------------------------------------------------------
async function drainSse(res, events) {
  const reader = res.body.getReader();
  const dec = new TextDecoder();
  let buf = "";
  for (;;) {
    const step = await reader.read();
    if (step.done) break;
    buf += dec.decode(step.value, { stream: true });
    for (;;) {
      const cut = buf.indexOf(NL + NL);
      if (cut < 0) break;
      const raw = buf.slice(0, cut);
      buf = buf.slice(cut + 2);
      let name = "message";
      let data = "";
      for (const line of raw.split(NL)) {
        if (line.indexOf("event:") === 0) name = line.slice(6).trim();
        else if (line.indexOf("data:") === 0) data += line.slice(5).trim();
      }
      if (data) {
        let payload = null;
        try { payload = JSON.parse(data); } catch { payload = null; }
        events.push({ event: name, data: payload });
      }
    }
  }
}
async function send(agent, prompt, runId) {
  const events = [];
  const res = await fetchStreamRaw(agent, prompt, runId);
  if (res.status !== 200) {
    // A non-200 answer carries its JSON code/error into the diagnosis (the
    // credential material on this wire is known-fake only, never echoed raw).
    const diag = await res.clone().json().catch(() => null);
    const why = diag && (diag.code || diag.error) ? " (code " + diag.code + " / " + diag.error + ")" : "";
    assert.equal(res.status, 200, "stream answered for " + agent + why);
  }
  await drainSse(res, events);
  return events;
}
function startStream(agent, prompt, runId) {
  const events = [];
  const done = (async () => {
    const res = await fetchStreamRaw(agent, prompt, runId);
    if (res.status !== 200) {
      const diag = await res.clone().json().catch(() => null);
      const why = diag && (diag.code || diag.error) ? " (code " + diag.code + " / " + diag.error + ")" : "";
      assert.equal(res.status, 200, "stream answered for " + agent + why);
    }
    await drainSse(res, events);
    return events;
  })();
  return { events, done };
}
async function waitEvent(events, name, timeoutMs) {
  const deadline = Date.now() + (timeoutMs || 20000);
  for (;;) {
    const hit = events.find((e) => e.event === name);
    if (hit) return hit;
    if (Date.now() > deadline) {
      throw new Error("no " + name + " event within " + (timeoutMs || 20000) + "ms; saw: " + events.map((e) => e.event).join(","));
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}
function runFrame(events) {
  assert.ok(events.length && events[0].event === "run" && events[0].data, "the first frame is the run event");
  return events[0].data;
}
function doneFrame(events) {
  const hit = events.filter((e) => e.event === "done").pop();
  assert.ok(hit && hit.data, "a done event closed the stream");
  return hit.data;
}

// ---------------------------------------------------------------------------
// Row edits through the host config API (GET for the revision, CAS PUT).
// ---------------------------------------------------------------------------
function putRow(change) {
  const cur = api("GET", "/api/plugins/llm/config");
  assert.equal(cur.status, 0, "read the llm row: " + cur.stderr);
  const config = (cur.body && cur.body.config) || {};
  change(config);
  const put = api("PUT", "/api/plugins/llm/config", JSON.stringify({ revision: cur.body.revision, config }));
  assert.equal(put.status, 0, "apply the llm row: " + put.stderr);
  if (put.body && put.body.error) throw new Error("row not applied: " + put.body.error);
  return put.body;
}
async function openRuns(agent) {
  const open = await adminJson("GET", "/api/llm/history/runs?agent=" + encodeURIComponent(agent) + "&open=true&page=0&pageSize=1");
  // The strict gate: a not-yet-serving plugin tree answers the structured
  // 503, and that body has no runs array - fail with the RECORDED numeric
  // status instead of letting a caller crash indexing undefined.
  assert.equal(open.status, 200, "the open listing answered (status " + open.status + ")");
  assert.ok(open.body && Array.isArray(open.body.runs), "the open listing is a real runs array");
  return open;
}
async function dialogue(id) {
  const r = await adminJson("GET", "/api/llm/history/runs/" + id + "/dialogue");
  assert.equal(r.status, 200, "dialogue for run " + id);
  return r.body;
}
async function callsOfRun(id) {
  const r = await adminJson("GET", "/api/llm/history/runs/" + id + "/calls");
  assert.equal(r.status, 200, "calls for run " + id);
  return r.body;
}
async function callsPage(query) {
  const r = await adminJson("GET", "/api/llm/history/calls" + (query ? "?" + query : ""));
  assert.equal(r.status, 200, "call history " + query);
  return r.body;
}

// ---------------------------------------------------------------------------
// Journal helpers: exact chronology, exact once-ness.
// ---------------------------------------------------------------------------
function chatsOf(model) {
  return fake.chats().filter((e) => e.body && e.body.model === model);
}
function userTexts(body) {
  return (body.messages || []).filter((m) => m.role === "user").map((m) => String(m.content));
}
function assistantTexts(body) {
  return (body.messages || []).filter((m) => m.role === "assistant").map((m) => String(m.content == null ? "" : m.content) + " " + JSON.stringify(m.tool_calls || null));
}
function onceIn(text, marker, what) {
  const count = text.split(marker).length - 1;
  assert.equal(count, 1, what + " appears exactly once: " + marker + " (saw " + count + ")");
}
function echoReq(marker) {
  const hit = chatsOf("fake-echo").find((e) => {
    const users = userTexts(e.body);
    return users.length > 0 && users[users.length - 1].includes(marker);
  });
  assert.ok(hit, "a fake-echo round trip whose last user turn is " + marker);
  return hit.body;
}

// ===========================================================================
// Scenario A: one gateway - conversation, busy/cancel, interleaving, tools,
// generic reasoning, auth mask, embedding bodies.
// ===========================================================================
const S1 = "S1-9a41"; const S2 = "S2-8b52"; const S3 = "S3-7c63";
const HOLD = "HOLD-d507";
const AFTER = "AFTER-2b7e";
const R1 = "R1-a177"; const R2 = "R2-b288"; const R3 = "R3-c399";
const BUSY = "BUSY-6c11"; const NEW = "NEW-c408"; const CXL2 = "CXL2-91af"; const FINAL = "FINAL-5e21";
let echoRunId = null;

try {
  await withGateway(async () => {
    putRow((config) => {
      config.providers = { fake: {
        style: "openai", baseUrl: fake.url + "/v1",
        models: [
          { id: "fake-echo" }, { id: "fake-tool" }, { id: "fake-a" }, { id: "fake-b" },
          { id: "fake-chat" }, { id: "fake-reason" }, { id: "fake-burst" }, { id: "fake-cxltool" }, { id: "fake-embed", kind: "embedding" },
        ],
      } };
      config.agents = {
        echo: { provider: "fake", model: "fake-echo" },
        toolsy: { provider: "fake", model: "fake-tool" },
        aa: { provider: "fake", model: "fake-a" },
        bb: { provider: "fake", model: "fake-b" },
        thinker: { provider: "fake", model: "fake-reason" },
        bursty: { provider: "fake", model: "fake-burst" },
        cxltool: { provider: "fake", model: "fake-cxltool" },
      };
      config.embedding = { provider: "fake", model: "fake-embed" };
    });
    assert.ok(existsSync(HOME), "the e2e home exists");

    // The whole scripted life of the echo conversation, in consumption order:
    // three turns, a cancelled hold, the post-cancel turn, three restart turns,
    // a busy hold, the fresh run after close, a cancelled hold, and the final
    // fresh turn. The fake answers each request with the next item.
    fake.script("fake-echo",
      { mode: "answer", text: "ANS1-zu01" }, { mode: "answer", text: "ANS2-zu02" }, { mode: "answer", text: LONG_ANSWER },
      { mode: "partial", text: "PART-7c1f visible-before-cancel", first: 1 }, { mode: "answer", text: "ANS-AFTER" },
      { mode: "answer", text: "ANS-R1" }, { mode: "answer", text: "ANS-R2" }, { mode: "answer", text: "ANS-R3" },
      { mode: "hang", then: { mode: "answer", text: "busy-done" } }, { mode: "answer", text: "ANS-NEW" },
      { mode: "hang", then: { mode: "answer", text: "cxl2-never" } }, { mode: "answer", text: "ANS-FINAL" });

    // -- the continuing conversation: three user turns in ONE context ---------
    const e1 = await send("echo", S1);
    const first = runFrame(e1);
    assert.ok(Number.isInteger(first.runId), "run.runId is a stable integer id");
    assert.equal(first.agent, "echo");
    assert.equal(first.model, "fake-echo");
    assert.equal(first.continued, false, "the first send starts a fresh context");
    echoRunId = first.runId;
    const d1 = doneFrame(e1);
    assert.equal(d1.runId, echoRunId, "done carries the same runId");
    assert.equal(d1.ok, true);

    const open1 = await openRuns("echo");
    assert.equal(open1.status, 200);
    assert.equal(open1.body.runs.length, 1, "one open conversation for echo");
    assert.equal(open1.body.runs[0].id, echoRunId);
    for (const key of ["id", "at", "atEnd", "agent", "runtime", "provider", "model", "ok", "ms", "turns"]) {
      assert.ok(key in open1.body.runs[0], "the open-runs row carries " + key);
    }

    // A no-id ask while this conversation is open must refuse with the exact
    // permanent 409 code - absent runId means NEW, and a new run cannot fork
    // past an open one. The refused ask never reaches the provider.
    const noIdBefore = chatsOf("fake-echo").length;
    const noId = await fetchStreamRaw("echo", "no-id-probe-3f0a");
    assert.equal(noId.status, 409, "a no-id send while a run is open answers 409");
    const noIdBody = await noId.json().catch(() => null);
    assert.equal(noIdBody && noIdBody.code, "open-run-exists", "the 409 carries the precise code");
    assert.equal(chatsOf("fake-echo").length, noIdBefore, "the refused no-id ask never reached the provider");

    const e2 = await send("echo", S2, echoRunId);
    assert.equal(runFrame(e2).runId, echoRunId, "the second send continues the same run");
    assert.equal(runFrame(e2).continued, true, "continued is set from the second send on");
    const e3 = await send("echo", S3, echoRunId);
    assert.equal(runFrame(e3).runId, echoRunId);

    // exact request chronology from the journal: each request carries every
    // completed turn, once, in order - the heart of the >=3-user-turns contract.
    const conv = chatsOf("fake-echo").slice(0, 3);
    assert.equal(conv.length, 3, "three provider round trips for three turns");
    assert.equal(userTexts(conv[0].body).length, 1);
    onceIn(userTexts(conv[0].body)[0], S1, "turn 1 user text");
    assert.equal(conv[0].body.messages[0].role, "system", "the system prompt leads every request");
    assert.equal(userTexts(conv[1].body).length, 2);
    onceIn(userTexts(conv[1].body)[0], S1, "turn 1 rides turn 2");
    onceIn(userTexts(conv[1].body)[1], S2, "turn 2 user text");
    onceIn(assistantTexts(conv[1].body).join(" | "), "ANS1-zu01", "turn 1 answer rides turn 2");
    assert.equal(userTexts(conv[2].body).length, 3);
    onceIn(userTexts(conv[2].body)[0], S1, "S1 rides turn 3");
    onceIn(userTexts(conv[2].body)[1], S2, "S2 rides turn 3");
    onceIn(userTexts(conv[2].body)[2], S3, "S3 user text");
    onceIn(assistantTexts(conv[2].body).join(" | "), "ANS2-zu02", "turn 2 answer rides turn 3");

    const dg = await dialogue(echoRunId);
    const userTurns = dg.turns.filter((t) => t.role === "user").map((t) => t.text);
    assert.equal(userTurns.length, 3, "the dialogue holds three user turns");
    onceIn(userTurns.join(" | "), S1, "dialogue S1");
    onceIn(userTurns.join(" | "), S3, "dialogue S3");

    // every conversation call is linked to its run, explicitly.
    const runCalls = (await callsOfRun(echoRunId)).calls;
    assert.ok(Array.isArray(runCalls) && runCalls.length >= 3, "the run lists its round trips");
    for (const c of runCalls) {
      assert.equal(c[RUN_ID_FIELD], echoRunId, "a conversation call carries its run linkage");
    }

    // the sealed-body boundary, proven on the long third answer: its head sits
    // in the turn's plaintext preview BY DESIGN; its tail (past the preview
    // window) exists only in the sealed chunks - absent from raw llm.db bytes,
    // present through the unseal route, and ENGAGED: the stored chunk chains
    // re-derive from the exact public-API texts (see sealed-chunks.mjs).
    const turnsList = await adminJson("GET", "/api/llm/history/runs/" + echoRunId + "/turns");
    const finals = (turnsList.body.turns || []).filter((t) => t.role === "final");
    const lastFinal = finals[finals.length - 1];
    assert.ok(lastFinal, "the run lists its final turn");
    assert.ok((lastFinal.preview || "").includes("ANS3-zu03"), "the preview carries the head of the long answer (the by-design plaintext boundary)");
    assert.ok(!(lastFinal.preview || "").includes(LONG_TAIL), "the preview cuts before the tail");
    const tailBody = await adminJson("GET", "/api/llm/history/turns/" + echoRunId + "/" + lastFinal.seq + "/body");
    assert.equal(tailBody.status, 200, "the turn body route answers 200");
    assert.equal(tailBody.body.bodyGone, false, "the turn body is present (bodyGone false)");
    assert.equal(tailBody.body.body, LONG_ANSWER, "the sealed turn body unseals to the EXACT full long answer");
    assert.equal(tailBody.body.runId, echoRunId, "the turn body echoes its run");
    assert.equal(tailBody.body.seq, lastFinal.seq, "the turn body echoes its seq");
    // Find the PRODUCING call by its public body - never by list order or a
    // preview guess: the run's call whose response texts are exactly the long
    // answer and whose request prompt is S3.
    let producer = null;
    for (const c of (await callsOfRun(echoRunId)).calls) {
      const cb = await adminJson("GET", "/api/llm/history/calls/" + c.id + "/body");
      if (cb.status !== 200 || !cb.body || cb.body.bodyGone !== false) continue;
      const parsed = JSON.parse(cb.body.body);
      const texts = parsed.response && parsed.response.texts;
      const prompt = parsed.request && parsed.request.prompt;
      if (JSON.stringify(texts) === JSON.stringify([LONG_ANSWER]) && prompt === S3) {
        producer = { id: c.id, resolvedCallBodyText: cb.body.body };
        break;
      }
    }
    assert.ok(producer, "the producing call is found by its public body (exact texts, exact prompt)");
    // Sealed-chunk engagement: chunk chains re-derived from the exact public
    // texts, piece hashes, scalar/byte budgets, strict seven-field sealed
    // envelopes, and the whole-file plaintext scan (db + WAL/SHM on ENOENT-only
    // skips) - all inside the guard.
    const guard = await assertStoredChunks({
      dbPath: join(HOME, "logs", "llm.db"),
      runId: echoRunId,
      turnSeq: lastFinal.seq,
      expectedTurnText: LONG_ANSWER,
      callId: producer.id,
      resolvedCallBodyText: producer.resolvedCallBodyText,
      tailMarker: LONG_TAIL,
    });
    // Boolean/numeric engagement summary only - no ciphertext, no key material.
    console.log("[sealed-chunks] pieces=" + guard.piecesVerified + " turnBytes=" + guard.turnUtf8Bytes + " callBytes=" + guard.envelopeUtf8Bytes + " turnClasses=" + JSON.stringify(guard.turnBlockClasses) + " callClasses=" + JSON.stringify(guard.callBlockClasses) + " filesScanned=" + guard.filesScanned);
    // a body that is not there fails honestly - never an empty success.
    const missingBody = await adminJson("GET", "/api/llm/history/turns/" + echoRunId + "/9999/body");
    assert.equal(missingBody.status, 404, "a missing turn body answers 404");

    // -- busy: a live stream rejects the second one 409, and close waits -------
    const held = startStream("echo", HOLD, echoRunId);
    const heldRun = await waitEvent(held.events, "run");
    assert.equal(heldRun.data.runId, echoRunId, "the held turn is the same open conversation");
    const second = await fetchStreamRaw("echo", "second-while-live", echoRunId);
    assert.equal(second.status, 409, "a second live stream for the same conversation answers 409");
    const secondBody = await second.json().catch(() => null);
    assert.ok(secondBody && (secondBody.code || secondBody.error), "the 409 carries a precise JSON code");
    const busyClose = await adminJson("POST", "/api/llm/history/runs/" + echoRunId + "/close");
    assert.equal(busyClose.status, 409, "close while live answers 409");

    // -- cancel: reply shape, idle, and the context stays whole ---------------
    const cxl = await adminJson("POST", CANCEL_PATH, { runId: echoRunId });
    assert.equal(cxl.status, 200);
    assert.deepEqual({ ok: cxl.body.ok, cancelled: cxl.body.cancelled }, { ok: true, cancelled: true }, "cancel mid-flight");
    // Cancel MUST close the stream with an honest terminal frame - there is
    // no release fallback here on purpose: a stream that only ends when the
    // fake gives up is a failure, not a pass.
    await waitEvent(held.events, "done", 60000);
    const heldDone = doneFrame(held.events);
    assert.equal(heldDone.runId, echoRunId);
    assert.equal(heldDone.cancelled, true, "done says cancelled");
    // Honest cancellation keeps what the user already saw: the partial
    // deltas streamed before the cancel stay in the run's record.
    const dgCxl = await dialogue(echoRunId);
    assert.ok(dgCxl.turns.some((t) => (t.text || "").includes("PART-7c1f vis")), "the cancelled turn's partial visible text stays in the record");
    const cxlAgain = await adminJson("POST", CANCEL_PATH, { runId: echoRunId });
    assert.deepEqual({ ok: cxlAgain.body.ok, cancelled: cxlAgain.body.cancelled }, { ok: true, cancelled: false }, "cancel after settle is idempotent");

    // the context after a cancel: the cancelled round kept its observed prefix
    // and its user request - a terminal settled round, retained canonically -
    // and no fabricated full answer rides the next request.
    const after = await send("echo", AFTER, echoRunId);
    assert.equal(runFrame(after).runId, echoRunId, "the conversation survives the cancel");
    assert.equal(runFrame(after).continued, true);
    const afterReq = echoReq(AFTER);
    const afterUsers = userTexts(afterReq);
    assert.equal(afterUsers.length, 5, "five user turns: S1..S3, the cancelled hold's request, plus the post-cancel send");
    onceIn(afterUsers[0], S1, "S1 after cancel");
    onceIn(afterUsers[1], S2, "S2 after cancel");
    onceIn(afterUsers[2], S3, "S3 after cancel");
    onceIn(afterUsers[3], HOLD, "the cancelled hold's user request rides canonically");
    onceIn(afterUsers[4], AFTER, "the post-cancel turn");
    onceIn(assistantTexts(afterReq).join(" | "), "PART-7c1f vis", "the observed cancelled prefix stays in assistant content");
    assert.ok(!JSON.stringify(afterReq).includes("ible-before-cancel"), "the cancelled round trip left no completed answer in the context");

    // -- interleaving: two agents, direct provider and embedding calls --------
    fake.script("fake-a", { mode: "answer", text: "AA1-x" }, { mode: "answer", text: "AA2-x" });
    fake.script("fake-b", { mode: "answer", text: "BB1-y" }, { mode: "answer", text: "BB2-y" });
    fake.script("fake-chat", { mode: "answer", text: "DIRECT-z" });
    const a1 = await send("aa", "IA1-m1");
    const runA = runFrame(a1).runId;
    const b1 = await send("bb", "IB1-n1");
    const runB = runFrame(b1).runId;
    assert.notEqual(runA, runB, "two agents hold two conversations");
    // The direct ask names its model explicitly: the route otherwise picks the
    // provider's FIRST chat model (fake-echo here), which would both add an
    // echo arrival and steal the restart queue's next scripted answer.
    const direct = await adminJson("POST", "/api/llm/providers/test", { provider: "fake", model: "fake-chat", prompt: "direct-d1" });
    assert.equal(direct.status, 200);
    assert.equal(direct.body.ok, true, "the direct provider test answers: " + JSON.stringify(direct.body));
    assert.equal(direct.body.model, "fake-chat", "the direct ask answered as the model it named");
    assert.equal(direct.body.text, "DIRECT-z", "the direct ask consumed its own scripted answer");
    const directAsks = chatsOf("fake-chat");
    assert.equal(directAsks.length, 1, "exactly one fake-chat ask: the direct test");
    assert.ok(directAsks[0].body && Array.isArray(directAsks[0].body.messages), "the direct ask is journaled with its real messages array");
    assert.equal(directAsks[0].body.messages.length, 1, "the direct ask carries no system message, only the prompt");
    assert.equal(directAsks[0].body.messages[0].role, "user", "the one message is the user turn");
    assert.equal(String(directAsks[0].body.messages[0].content), "direct-d1", "the direct prompt is exact");
    const embed = await adminJson("POST", "/api/llm/embedding/test", { text: "E2E-EMBED-e6a1" });
    assert.equal(embed.status, 200);
    assert.equal(embed.body.ok, true, "the embedding test answers: " + JSON.stringify(embed.body));
    const a2 = await send("aa", "IA2-m2", runA);
    assert.equal(runFrame(a2).runId, runA);
    const b2 = await send("bb", "IB2-n2", runB);
    assert.equal(runFrame(b2).runId, runB);

    const callsA = (await callsOfRun(runA)).calls;
    assert.equal(callsA.length, 2, "run A holds exactly its two round trips");
    for (const c of callsA) {
      assert.equal(c.model, "fake-a", "no cross-agent call leaked into run A");
      assert.equal(c[RUN_ID_FIELD], runA, "interleaved calls stay linked to their run");
    }
    const callsB = (await callsOfRun(runB)).calls;
    assert.equal(callsB.length, 2);
    for (const c of callsB) assert.equal(c.model, "fake-b");
    const idA = new Set(callsA.map((c) => c.id));
    const idB = new Set(callsB.map((c) => c.id));
    const all = (await callsPage("page=0&pageSize=100")).calls || [];
    const strays = all.filter((c) => c.kind === "chat" && c.agent == null && (idA.has(c.id) || idB.has(c.id)));
    assert.deepEqual(strays, [], "the direct provider call belongs to no conversation");
    const directRows = all.filter((c) => c.kind === "chat" && c.agent == null && c.model === "fake-chat");
    assert.equal(directRows.length, 1, "exactly one direct call row, owned by no agent");
    assert.ok(RUN_ID_FIELD in directRows[0], "the direct row carries the run linkage field");
    assert.equal(directRows[0][RUN_ID_FIELD], null, "the direct call's run linkage is explicitly null");
    const dgA = await dialogue(runA);
    onceIn(dgA.turns.filter((t) => t.role === "user").map((t) => t.text).join(" | "), "IA1-m1", "A dialogue keeps its own turns");
    assert.ok(!dgA.turns.some((t) => (t.text || "").includes("IB1-n1")), "B's turn never appears in A's dialogue");
    const openA = await openRuns("aa");
    assert.equal(openA.body.runs.length, 1);
    assert.equal(openA.body.runs[0].id, runA);
    const openB = await openRuns("bb");
    assert.equal(openB.body.runs[0].id, runB);

    // -- a real tool round: the read tool's USER-home root is this same
    // pinned dedicated test home, so the note below is inside the root. ------
    const NOTE = "e2e llm tool marker 51c3";
    writeFileSync(join(HOME, "note.txt"), NOTE);
    // A distinct, explicitly scripted tool-call id: every id oracle below
    // compares against THIS constant, never two derived ids against each other.
    const TOOL_CALL_ID = "call-e2e-read-01";
    fake.script("fake-tool", { mode: "tool", name: "read", args: { path: "note.txt" }, callId: TOOL_CALL_ID }, { mode: "answer", text: "TOOLDONE-97" });
    const toolRun = await send("toolsy", "TOOL-3a9f");
    const toolRunId = runFrame(toolRun).runId;
    assert.equal(doneFrame(toolRun).ok, true, "the tool round completes");
    const toolReqs = chatsOf("fake-tool");
    assert.equal(toolReqs.length, 2, "the loop asked twice: the call, then the tool result");
    const secondAsk = toolReqs[1].body.messages;
    const toolCallMsg = secondAsk.filter((m) => m.role === "assistant" && m.tool_calls && m.tool_calls.length);
    assert.equal(toolCallMsg.length, 1, "the second request replays the assistant tool call");
    assert.equal(toolCallMsg[0].tool_calls[0].function.name, "read");
    onceIn(toolCallMsg[0].tool_calls[0].function.arguments, "note.txt", "the tool call arguments");
    const toolResultMsg = secondAsk.filter((m) => m.role === "tool");
    assert.equal(toolResultMsg.length, 1);
    assert.equal(String(toolResultMsg[0].content), NOTE, "the tool result carries the exact file bytes");
    assert.equal(toolCallMsg[0].tool_calls[0].id, TOOL_CALL_ID, "the replayed tool call keeps the original scripted id (no fabricated replay id)");
    assert.equal(String(toolResultMsg[0].tool_call_id), TOOL_CALL_ID, "the tool result references that exact scripted id");
    const callAt = secondAsk.indexOf(toolCallMsg[0]);
    assert.ok(callAt >= 0, "the tool call message is in the replayed context");
    assert.equal(secondAsk[callAt + 1], toolResultMsg[0], "the tool result directly follows its assistant tool call");
    const dgTool = await dialogue(toolRunId);
    assert.ok(dgTool.turns.some((t) => t.role === "tool" && (t.text || "").includes(NOTE)), "the dialogue shows the tool turn");

    // -- a burst of 160 deltas, no sleeps: every frame must arrive ------------
    // A relay buffer that silently skips frames at 32 was acknowledged; this
    // pins the fix: exact count, exact concatenation, whole in the record.
    const BURST = 160;
    fake.script("fake-burst", { mode: "burst", count: BURST });
    let burstWant = "";
    for (let i = 0; i < BURST; i++) burstWant += "b" + i + ";";
    const be = await send("bursty", "BURST-d926");
    const burstRunId = runFrame(be).runId;
    const deltas = be.filter((e) => e.event === "delta" && e.data && e.data.kind === "text");
    assert.equal(deltas.length, BURST, "every burst delta arrived (silent skips fail)");
    assert.equal(deltas.map((e) => e.data.text).join(""), burstWant, "the burst concatenation is exact");
    assert.equal(doneFrame(be).ok, true);
    const dgBurst = await dialogue(burstRunId);
    assert.ok(dgBurst.turns.some((t) => (t.text || "").includes(burstWant.slice(0, 32)) && (t.text || "").includes(burstWant.slice(-32))), "the burst answer is recorded whole");

    // -- honest cancel while the round trip after a tool call hangs ------------
    // A punctuation-bearing Unicode id: the JSON round trip must preserve it
    // exactly, on the assistant call and on the tool result that answers it.
    const TOOL_CALL_ID_CXL = "call-工具-回环-②";
    fake.script("fake-cxltool",
      { mode: "tool", name: "read", args: { path: "note.txt" }, callId: TOOL_CALL_ID_CXL },
      { mode: "hang", then: { mode: "answer", text: "cxltool-never" } },
      { mode: "answer", text: "CXLTOOL-RECOVERED" },
      { mode: "answer", text: "CXLTOOL-NEWRUN-ANS" });
    const ct = startStream("cxltool", "CXLTOOL-4b8e");
    const ctRun = (await waitEvent(ct.events, "run")).data.runId;
    await waitEvent(ct.events, "toolResult", 30000); // the tool round completes; the next ask hangs
    // The arrival gate: the cancel must land DURING the second provider ask.
    // Readiness comes from the fake's journal (every request is journaled at
    // arrival, before the hang gate holds) - the toolResult UI event alone is
    // not proof the second ask reached the provider.
    const ctArriveDeadline = Date.now() + 10000;
    for (;;) {
      if (chatsOf("fake-cxltool").length === 2 && fake.hangCount() === 1) break;
      if (Date.now() > ctArriveDeadline) throw new Error("the hanging ask never arrived: chats=" + chatsOf("fake-cxltool").length + " hangs=" + fake.hangCount());
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    onceIn(userTexts(chatsOf("fake-cxltool")[1].body).join(" | "), "CXLTOOL-4b8e", "the held ask is this conversation's own prompt");
    const ctCancel = await adminJson("POST", CANCEL_PATH, { runId: ctRun });
    assert.deepEqual({ ok: ctCancel.body.ok, cancelled: ctCancel.body.cancelled }, { ok: true, cancelled: true }, "cancel during the hanging round trip after a tool");
    await waitEvent(ct.events, "done", 60000);
    assert.equal(doneFrame(ct.events).cancelled, true, "the tool-loop stream closes honestly");
    const dgCt = await dialogue(ctRun);
    assert.ok(dgCt.turns.some((t) => t.role === "tool" && (t.text || "").includes(NOTE)), "the completed tool round is kept");
    // The abort gate: the cancelled ask's socket really closed - the journal
    // remembers the abort and no gate stays held - before any recovery ask.
    const ctAbortDeadline = Date.now() + 10000;
    for (;;) {
      const held = chatsOf("fake-cxltool");
      if (held[1] && held[1].aborted === true && fake.hangCount() === 0) break;
      if (Date.now() > ctAbortDeadline) throw new Error("the cancelled ask never aborted: aborted=" + (held[1] && held[1].aborted) + " hangs=" + fake.hangCount());
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    // The contract (SPEC llm.history): a Stop during the model wait after a tool
    // round files that wait as a cancelled round, so the conversation stays
    // continuable. A no-id send still refuses beside the open run without
    // reaching the provider - the journal count is the proof.
    const ctNoId = await fetchStreamRaw("cxltool", "no-id-while-open-c3");
    assert.equal(ctNoId.status, 409, "a no-id send while the run is open answers 409");
    const ctNoIdBody = await ctNoId.json().catch(() => null);
    assert.equal(ctNoIdBody && ctNoIdBody.code, "open-run-exists", "the no-id 409 carries the exact open-run-exists code");
    assert.equal(chatsOf("fake-cxltool").length, 2, "the refused no-id send never reached the provider");
    // The canonical wire the cancel interrupted: the second ask THAT REALLY
    // ARRIVED carries the tool round byte-exactly - the Unicode id on both the
    // assistant call and the tool result that answers it.
    const ctHang = chatsOf("fake-cxltool")[1];
    assert.ok(ctHang && ctHang.body, "the hanging ask is in the journal with its body");
    const ctCallMsg = ctHang.body.messages.filter((m) => m.role === "assistant" && m.tool_calls && m.tool_calls.length);
    assert.equal(ctCallMsg.length, 1, "one assistant tool call rides the interrupted ask");
    assert.equal(ctCallMsg[0].tool_calls[0].id, TOOL_CALL_ID_CXL, "the Unicode tool-call id survives the round trip exactly");
    onceIn(ctCallMsg[0].tool_calls[0].function.arguments, "note.txt", "the tool call arguments are unchanged");
    const ctToolMsg = ctHang.body.messages.filter((m) => m.role === "tool");
    assert.equal(ctToolMsg.length, 1, "one tool result rides the interrupted ask");
    assert.equal(String(ctToolMsg[0].content), NOTE, "the tool result carries the exact file bytes");
    assert.equal(String(ctToolMsg[0].tool_call_id), TOOL_CALL_ID_CXL, "the tool result references the Unicode id exactly");
    assert.ok(!JSON.stringify(ctHang.body).includes("cxltool-never"), "the cancelled round trip left no answer");
    // The named send continues the SAME run: the stopped turn and its settled
    // tool round replay once each, the new turn follows, nothing is invented.
    const ctNext = await send("cxltool", "CXLTOOL-AFTER-51ca", ctRun);
    const ctNextRun = runFrame(ctNext);
    assert.equal(ctNextRun.runId, ctRun, "the named send continues the stopped run");
    assert.equal(ctNextRun.continued, true, "the send is a continuation");
    assert.equal(doneFrame(ctNext).ok, true, "the continued round trip completes");
    assert.equal(chatsOf("fake-cxltool").length, 3, "exactly three provider asks: the refusal consumed nothing");
    const ctNextReq = chatsOf("fake-cxltool")[2];
    const ctNextUsers = userTexts(ctNextReq.body).join(" | ");
    onceIn(ctNextUsers, "CXLTOOL-4b8e", "the stopped turn's prompt");
    onceIn(ctNextUsers, "CXLTOOL-AFTER-51ca", "the new turn");
    const ctNextTool = ctNextReq.body.messages.filter((m) => m.role === "tool");
    assert.equal(ctNextTool.length, 1, "the settled tool round replays once");
    assert.equal(String(ctNextTool[0].tool_call_id), TOOL_CALL_ID_CXL, "the replayed result keeps the Unicode id");
    assert.equal(String(ctNextTool[0].content), NOTE, "the replayed result keeps the exact file bytes");
    assert.ok(!JSON.stringify(ctNextReq.body).includes("cxltool-never"), "the cancelled answer never appears");
    const dgNext = await dialogue(ctRun);
    assert.ok(dgNext.turns.some((t) => (t.text || "").includes("CXLTOOL-RECOVERED")), "the continued answer is recorded");
    // The operator's explicit close ends it (SPEC llm.history): ONE close, no
    // retry - the continuation settled, so a busy 409 here would be a defect.
    const ctClose = await adminJson("POST", "/api/llm/history/runs/" + ctRun + "/close");
    assert.equal(ctClose.status, 200, "the close after a settled continuation answers 200");
    assert.deepEqual(ctClose.body, { ok: true }, "close reports ok");
    // The fresh no-id conversation: no stale context rides.
    const ctFresh = await send("cxltool", "CXLTOOL-FRESH-77d0");
    const ctFreshRun = runFrame(ctFresh);
    assert.notEqual(ctFreshRun.runId, ctRun, "the fresh send opens a distinct run");
    assert.equal(ctFreshRun.continued, false, "the fresh run is not a continuation");
    assert.equal(doneFrame(ctFresh).ok, true, "the fresh round trip completes");
    assert.equal(chatsOf("fake-cxltool").length, 4, "exactly four provider asks");
    const ctFreshReq = chatsOf("fake-cxltool")[3];
    const ctFreshUsers = userTexts(ctFreshReq.body);
    assert.equal(ctFreshUsers.length, 1, "the fresh ask carries exactly one user turn");
    onceIn(ctFreshUsers[0], "CXLTOOL-FRESH-77d0", "the fresh turn");
    assert.ok(!ctFreshReq.body.messages.some((m) => m.role === "tool"), "no tool message rides the fresh ask");
    const ctFreshJson = JSON.stringify(ctFreshReq.body);
    assert.ok(!ctFreshJson.includes(NOTE), "no stale tool bytes ride the fresh ask");
    assert.ok(!ctFreshJson.includes(TOOL_CALL_ID_CXL), "no stale tool-call id rides the fresh ask");
    assert.ok(!ctFreshJson.includes("CXLTOOL-4b8e"), "no pre-close prompt rides the fresh ask");
    assert.ok(!ctFreshJson.includes("cxltool-never"), "the cancelled answer never appears");
    const dgFresh = await dialogue(ctFreshRun.runId);
    assert.ok(dgFresh.turns.some((t) => (t.text || "").includes("CXLTOOL-NEWRUN-ANS")), "the fresh answer is recorded");

    // -- generic reasoning: it streams, displays, and files durably ------------
    // NOT the DeepSeek resend guarantee: the conservative policy gates reasoning
    // resend on the exact official api.deepseek.com host, so that branch is
    // proven by Rust units against a mock private endpoint. A local alias makes
    // no such promise; this case pins only what holds for any reasoning model.
    fake.script("fake-reason",
      { mode: "answer", reasoning: "THINK-8f02", text: "TH-ANS-1" },
      { mode: "answer", text: "TH-ANS-2" });
    const t1 = await send("thinker", "TH1-q1");
    assert.equal(doneFrame(t1).ok, true);
    assert.ok(t1.some((e) => e.event === "delta" && e.data && e.data.kind === "thinking" && String(e.data.text || "").length > 0),
      "the reasoning streams as visible thinking deltas");
    const thinkRunId = runFrame(t1).runId;
    const dgThink = await dialogue(thinkRunId);
    assert.ok(dgThink.turns.some((t) => (t.text || "").includes("THINK-8f02") || (t.preview || "").includes("THINK-8f02")),
      "the run's record keeps the reasoning durably");
    const t2 = await send("thinker", "TH2-q2", thinkRunId);
    assert.equal(doneFrame(t2).ok, true);
    assert.equal(chatsOf("fake-reason").length, 2);

    // -- auth mask: the reference never resolves into anything stored --------
    const fake2 = await fakeProvider({ models: ["fake-key-chat"], requireBearer: true, bearer: process.env.SWISS_E2E_FAKE_KEY });
    try {
      putRow((config) => {
        config.providers.envref = {
          style: "openai", baseUrl: fake2.url + "/v1", apiKey: "$" + "{SWISS_E2E_FAKE_KEY}",
          models: [{ id: "fake-key-chat" }],
        };
      });
      fake2.script("fake-key-chat", { mode: "answer", text: "KEYED-ok" }, { mode: "error", message: "the fake failed on purpose", echoBearer: true });
      const okTest = await adminJson("POST", "/api/llm/providers/test", { provider: "envref" });
      assert.equal(okTest.body.ok, true, "the env-ref provider authenticates: " + JSON.stringify(okTest.body));
      assert.equal(fake2.chats()[0].authorization, "Bearer " + process.env.SWISS_E2E_FAKE_KEY, "the reference resolved only onto the wire");
      const leakTest = await adminJson("POST", "/api/llm/providers/test", { provider: "envref" });
      assert.equal(leakTest.body.ok, false, "the error case is an answer, not a route failure");
      assert.ok(!JSON.stringify(leakTest.body).includes(process.env.SWISS_E2E_FAKE_KEY), "the echoed bearer was scrubbed from the reply");
      const status = await adminJson("GET", "/api/llm/status");
      assert.ok(!JSON.stringify(status.body).includes(process.env.SWISS_E2E_FAKE_KEY), "status never shows the resolved key");
      const recent = (await callsPage("page=0&pageSize=20")).calls || [];
      const failed = recent.find((c) => c.ok === false && c.provider === "envref");
      if (failed && failed.hasBody) {
        const bodyRead = await adminJson("GET", "/api/llm/history/calls/" + failed.id + "/body");
        assert.ok(!JSON.stringify(bodyRead.body).includes(process.env.SWISS_E2E_FAKE_KEY), "the stored call body stays masked");
      }
    } finally {
      await fake2.close();
    }

    // -- limits reject, never truncate ---------------------------------------
    const tooLong = "x".repeat(PROMPT_LIMIT + 1);
    // The echo journal census: exactly the five legitimate agent asks, in
    // order, each carrying exactly its growing user history - and nothing
    // leaked (no probe prompt, no direct test, no over-long text) anywhere.
    const echoAsks = chatsOf("fake-echo");
    assert.equal(echoAsks.length, 5, "exactly five fake-echo asks: S1, S2, S3, the cancelled HOLD, and AFTER");
    const censusMarkers = [S1, S2, S3, HOLD, AFTER];
    echoAsks.forEach((ask, i) => {
      const users = userTexts(ask.body);
      assert.equal(users.length, i + 1, "echo ask " + i + " carries exactly " + (i + 1) + " user turns");
      onceIn(users[i], censusMarkers[i], "echo ask " + i + "'s newest turn is " + censusMarkers[i]);
    });
    assert.equal(echoAsks[3].aborted, true, "the cancelled HOLD ask is journaled as aborted");
    const allEchoUsers = echoAsks.map((a) => userTexts(a.body).join(" | ")).join(" | ");
    assert.ok(!allEchoUsers.includes("direct-d1"), "the direct test never rode the echo model");
    assert.ok(!allEchoUsers.includes("second-while-live"), "the busy-lane probe never reached the provider");
    assert.ok(!allEchoUsers.includes(tooLong), "the over-long prompt is in no ask yet");
    const beforeReject = echoAsks.length;
    const over = await fetchStreamRaw("echo", tooLong);
    assert.equal(over.status, 400, "an over-limit prompt is rejected before any ask");
    assert.equal(chatsOf("fake-echo").length, beforeReject, "the rejected prompt never reached the provider");

    // -- R1: the embedding call keeps full sealed inputs and vectors ----------
    const embedCalls = (await callsPage("kind=embedding&page=0&pageSize=10")).calls || [];
    const embedRow = embedCalls.find((c) => (c.preview || "").includes("E2E-EMBED-e6a1")) || embedCalls[0];
    assert.ok(embedRow, "the embedding test filed a call");
    assert.equal(embedRow.hasBody, true, "embedding bodies are kept (R1)");
    const embedBody = await adminJson("GET", "/api/llm/history/calls/" + embedRow.id + "/body");
    const embedText = JSON.stringify(embedBody.body);
    assert.ok(embedText.includes("E2E-EMBED-e6a1"), "the stored embedding body keeps the input text (R1)");
    assert.ok(embedText.includes("0.125"), "the stored embedding body keeps the vectors (R1)");
  }, { plugin: "llm", ready: "/api/llm/status" });

  // =========================================================================
  // Scenario B: restart resume - the open conversation survives full gateway
  // stop/starts on the same home, and its chronology keeps growing.
  // =========================================================================
  try {
    // Boot inside the try: a boot that spawns but never turns healthy must
    // still reach stop(), and the plugin must be SERVING before the first
    // llm request - /health answers before plugins start (server boot order),
    // so waitReady on the plugin's own status route is the real gate.
    await boot({ fresh: false });
    const llmUp = await helpers.waitReady("/api/llm/status");
    assert.equal(llmUp.status, 0, "the llm plugin serves its status route after restart (status " + llmUp.status + ")");
    const open = await openRuns("echo");
    assert.equal(open.body.runs.length, 1, "the conversation is still open after restart");
    assert.equal(open.body.runs[0].id, echoRunId);
    await send("echo", R1, echoRunId);
    await send("echo", R2, echoRunId);
  } finally {
    stop();
  }
  try {
    // Same contract as the first restart boot: owned child on failure, and
    // the llm plugin serving before the first history read.
    await boot({ fresh: false });
    const llmUp2 = await helpers.waitReady("/api/llm/status");
    assert.equal(llmUp2.status, 0, "the llm plugin serves its status route after the second restart (status " + llmUp2.status + ")");
    const open = await openRuns("echo");
    assert.equal(open.body.runs.length, 1, "exactly one open run survives the second restart");
    assert.equal(open.body.runs[0].id, echoRunId, "still the same run after a second restart");
    const dg = await dialogue(echoRunId);
    const users = dg.turns.filter((t) => t.role === "user").map((t) => t.text).join(" | ");
    onceIn(users, S1, "S1 survived two restarts");
    onceIn(users, R1, "R1 survived the restart");
    const e3 = await send("echo", R3, echoRunId);
    assert.equal(runFrame(e3).runId, echoRunId);
    assert.equal(runFrame(e3).continued, true);
    const r3Req = echoReq(R3);
    const r3Users = userTexts(r3Req);
    assert.equal(r3Users.length, 8, "eight user turns ride the post-restart request");
    const r3Expected = [S1, S2, S3, HOLD, AFTER, R1, R2, R3];
    r3Expected.forEach((marker, i) => onceIn(r3Users[i], marker, "post-restart slot " + i + " is " + marker));
  } finally {
    stop();
  }

  // =========================================================================
  // Scenario C: close/reset - busy 409, audit retention, a wholly fresh
  // chronology, and the cancel -> await idle -> close (New) sequence.
  // =========================================================================
  try {
    // Same contract once more: owned child on failure, plugin serving first.
    await boot({ fresh: false });
    const llmUp3 = await helpers.waitReady("/api/llm/status");
    assert.equal(llmUp3.status, 0, "the llm plugin serves its status route after the third restart (status " + llmUp3.status + ")");
    const held = startStream("echo", BUSY, echoRunId);
    const heldRun = await waitEvent(held.events, "run");
    assert.equal(heldRun.data.runId, echoRunId);
    const busy = await adminJson("POST", "/api/llm/history/runs/" + echoRunId + "/close");
    assert.equal(busy.status, 409, "close stays 409 while the run is live");
    const live = await fetchStreamRaw("echo", "second-while-live-c", echoRunId);
    assert.equal(live.status, 409, "a second live stream for the same conversation answers 409");
    fake.releaseAll();
    await held.done;
    const closed = await adminJson("POST", "/api/llm/history/runs/" + echoRunId + "/close");
    assert.deepEqual(closed.body, { ok: true }, "close after idle answers ok");
    const openAfter = await openRuns("echo");
    assert.deepEqual(openAfter.body.runs, [], "no open conversation after close");
    const allRuns = await adminJson("GET", "/api/llm/history/runs?page=0&pageSize=100");
    assert.ok((allRuns.body.runs || []).some((r) => r.id === echoRunId), "the closed run is retained for audit");
    const dgClosed = await dialogue(echoRunId);
    assert.ok(dgClosed.turns.filter((t) => t.role === "user").length >= 6, "the closed run's dialogue is still readable");

    // reset: the next send is a wholly fresh context - no old sentinel rides.
    const fresh = await send("echo", NEW);
    const freshRun = runFrame(fresh);
    assert.ok(freshRun.runId !== echoRunId, "a new run id after close");
    assert.equal(freshRun.continued, false);
    const freshUsers = userTexts(echoReq(NEW));
    assert.equal(freshUsers.length, 1, "the fresh request carries exactly one user turn");
    onceIn(freshUsers[0], NEW, "the fresh turn");
    assert.ok(!JSON.stringify(echoReq(NEW)).includes(S1), "no pre-close sentinel rides the fresh request");

    // cancel -> await registry idle -> close (the panel New sequence).
    const cxlStream = startStream("echo", CXL2, freshRun.runId);
    const cxlRun = await waitEvent(cxlStream.events, "run");
    const runId2 = cxlRun.data.runId;
    assert.equal(runId2, freshRun.runId, "the fresh conversation continues");
    const cxl2 = await adminJson("POST", CANCEL_PATH, { runId: runId2 });
    assert.deepEqual({ ok: cxl2.body.ok, cancelled: cxl2.body.cancelled }, { ok: true, cancelled: true }, "cancel mid-flight");
    await waitEvent(cxlStream.events, "done", 60000);
    assert.equal(doneFrame(cxlStream.events).cancelled, true, "the second cancel closes the stream honestly too");
    let closed2 = null;
    const deadline = Date.now() + 10000;
    for (;;) {
      closed2 = await adminJson("POST", "/api/llm/history/runs/" + runId2 + "/close");
      if (closed2.status === 200 || Date.now() > deadline) break; // await registry idle, then close
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    assert.deepEqual(closed2.body, { ok: true }, "close succeeds once the registry is idle after cancel");
    const openEnd = await openRuns("echo");
    assert.deepEqual(openEnd.body.runs, [], "nothing left open");
    const dg2 = await dialogue(runId2);
    onceIn(dg2.turns.filter((t) => t.role === "user").map((t) => t.text).join(" | "), NEW, "the closed run keeps its audited turn");

    // and a wholly fresh multi-turn state after that.
    const end = await send("echo", FINAL);
    const endRun = runFrame(end);
    assert.ok(endRun.runId !== runId2 && endRun.runId !== echoRunId, "a third fresh run");
    assert.equal(userTexts(echoReq(FINAL)).length, 1);
    onceIn(userTexts(echoReq(FINAL))[0], FINAL, "the final fresh turn");
  } finally {
    stop();
  }

  console.log("llm-chat: ok");
} finally {
  try {
    await fake.close();
  } finally {
    // Restore the two pinned child-env keys to their exact prior presence and
    // value even when fake.close() throws: a key that was absent is deleted
    // again, one that was present gets its original value back.
    for (const r of ENV_RESTORE) {
      if (!r.had) delete helpers.ENV[r.key];
      else helpers.ENV[r.key] = r.was;
    }
  }
}
