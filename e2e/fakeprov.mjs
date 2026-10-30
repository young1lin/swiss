// The deterministic fake LLM provider for the e2e suite: a loopback OpenAI-wire
// server built on node:http alone (no dependency, no real endpoint, no cost, no
// secrets - the keyless loopback provider is a first-class case in SPEC llm.providers).
//
// It speaks exactly the wire swiss itself pins in crates/swiss-llm/src/provider.rs
// tests, so genai's adapter reads it like any OpenAI-compatible endpoint:
//   POST {base}/chat/completions - one SSE body: delta frames (text, reasoning via
//     reasoning_content, or a tool call), a finish frame carrying usage, then [DONE].
//     Behaviors are scripted per model, consumed in order: answer, reasoning+answer,
//     tool call, HTTP error (optionally echoing the bearer it received - the leak a
//     provider must never pass through), mid-stream abort, hang-then-release (the
//     gate that lets a test hold a round trip open deterministically), a burst of
//     N single-piece deltas (the no-sleep flood), and partial-then-hold (deltas
//     the user sees before a cancel; the rest waits for release).
//   GET  {base}/models           - the provider's model listing.
//   POST {base}/embeddings       - fixed vectors, index order kept.
//
// The journal records every request (method, path, authorization, parsed body) so a
// test asserts the EXACT request chronology swiss sends - messages, tools, model -
// never a digest of it.
//
// Selftest (loopback only; queued for explicit grant, never part of the suite):
//   node e2e/fakeprov.mjs --selftest
// The selftest is staged (a print before every awaited call), bounded by a 30s
// watchdog, and cleans up only its own fetches, gates and server - a failure
// must terminate, never hang.
import http from "node:http";

// The newline as a value, not a literal: this file is authored through tooling where
// backslash escapes are fragile, and the repo has been burned by real newlines inside
// JS string literals before (see .agents/rules/panel-proof-of-life.md).
const NL = String.fromCharCode(10);

function jsonHeaders() {
  return { "content-type": "application/json" };
}
function sseHeaders(length) {
  return { "content-type": "text/event-stream", "content-length": String(length), "connection": "close" };
}
function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    req.on("error", reject);
  });
}

// One OpenAI-style stream chunk.
function frame(model, delta, finish) {
  return {
    id: "fake-1", object: "chat.completion.chunk", model,
    choices: [{ index: 0, delta, finish_reason: finish == null ? null : finish }],
  };
}
// The closing chunk carries the usage (the shape provider.rs asserts).
function usageFrame(model, finish, tokens) {
  const f = frame(model, {}, finish);
  f.usage = { prompt_tokens: tokens[0], completion_tokens: tokens[1], total_tokens: tokens[0] + tokens[1] };
  return f;
}
// The answer as two deltas, so the consumer's typewriter path is exercised.
function splitText(text) {
  const s = String(text == null ? "" : text);
  if (s.length < 2) return [s];
  const at = Math.ceil(s.length / 2);
  return [s.slice(0, at), s.slice(at)];
}
function framesFor(item, model) {
  const frames = [];
  if (item.reasoning) frames.push(frame(model, { reasoning_content: item.reasoning }));
  if (item.mode === "burst") {
    // A no-sleep flood: N single-piece deltas, each individually verifiable, to
    // expose a relay that silently skips frames when its buffer fills.
    const count = item.count || 160;
    for (let i = 0; i < count; i++) frames.push(frame(model, { content: "b" + i + ";" }));
    frames.push(usageFrame(model, "stop", [count, count]));
    return frames;
  }
  if (item.mode === "tool") {
    frames.push(frame(model, { tool_calls: [{
      index: 0, id: item.callId || "call-fake-1", type: "function",
      function: { name: item.name, arguments: typeof item.args === "string" ? item.args : JSON.stringify(item.args || {}) },
    }] }));
    frames.push(usageFrame(model, "tool_calls", item.tokens || [24, 6]));
    return frames;
  }
  for (const piece of splitText(item.text)) frames.push(frame(model, { content: piece }));
  frames.push(usageFrame(model, "stop", item.tokens || [12, 4]));
  return frames;
}
function sseBody(frames) {
  return framesBody(frames) + "data: [DONE]" + NL + NL;
}
// The data frames without the terminal [DONE] (a held stream writes a prefix,
// and the terminal marker must only ride the release).
function framesBody(frames) {
  let out = "";
  for (const f of frames) out += "data: " + JSON.stringify(f) + NL + NL;
  return out;
}

// The fixed 8-dimensional vector every embeddings call answers (R1 keeps full
// sealed inputs and vectors, so tests can assert the exact numbers came back).
const VECTOR = [0.5, -0.25, 0.125, -0.0625, 0.03125, -0.015625, 0.0078125, -0.00390625];

export async function fakeProvider(options = {}) {
  const models = options.models || ["fake-chat", "fake-embed"];
  const requireBearer = !!options.requireBearer;
  const bearer = options.bearer || "";
  const entries = [];        // the journal, in arrival order
  const scriptQueue = [];    // FIFO for unscripted-per-model requests
  const perModel = new Map(); // model -> FIFO (how interleaved agents stay deterministic)
  const hangs = [];          // held round trips, released by the test
  let n = 0;

  function script(model, ...items) {
    if (model == null) { scriptQueue.push(...items); return; }
    let q = perModel.get(model);
    if (!q) { q = []; perModel.set(model, q); }
    q.push(...items);
  }
  function takeScript(model) {
    const q = perModel.get(model);
    if (q && q.length) return q.shift();
    return scriptQueue.shift();
  }

  async function handle(req, res) {
    const raw = await readBody(req);
    let parsed = null;
    try { parsed = JSON.parse(raw); } catch { parsed = null; }
    const entry = { i: n++, at: Date.now(), method: req.method, path: req.url, authorization: req.headers.authorization || null, body: parsed };
    entries.push(entry);

    if (req.method === "GET" && req.url.endsWith("/models")) {
      res.writeHead(200, jsonHeaders());
      res.end(JSON.stringify({ object: "list", data: models.map((id) => ({ id })) }));
      return;
    }
    if (req.url.endsWith("/embeddings")) {
      res.writeHead(200, jsonHeaders());
      res.end(JSON.stringify({ object: "list", model: "fake-embed", data: [{ object: "embedding", index: 0, embedding: VECTOR }], usage: { prompt_tokens: 3, total_tokens: 3 } }));
      return;
    }
    if (req.url.endsWith("/chat/completions")) {
      if (requireBearer && entry.authorization !== "Bearer " + bearer) {
        res.writeHead(401, jsonHeaders());
        res.end(JSON.stringify({ error: { message: "the fake expected its bearer header" } }));
        return;
      }
      const model = (parsed && parsed.model) || "fake-chat";
      const item = takeScript(model) || { mode: "answer", text: "unscripted:" + model };
      if (item.mode === "hang" || item.mode === "partial") {
        // hang: nothing is sent until released. partial: a prefix of deltas is
        // written at once (the text the user sees before a cancel); the rest
        // waits for release. Either way a cancelled round trip closes the
        // socket, the gate goes away, and the journal remembers the abort.
        let prefix = null;
        let rest = null;
        if (item.mode === "partial") {
          const frames = framesFor({ mode: "answer", text: item.text || "partial" }, model);
          const cut = Math.min(item.first || 2, frames.length);
          prefix = framesBody(frames.slice(0, cut));
          rest = framesBody(frames.slice(cut)) + "data: [DONE]" + NL + NL;
        } else {
          rest = sseBody(framesFor(item.then || { mode: "answer", text: "late" }, model));
        }
        const gate = { entry, res, prefix, rest };
        if (prefix) {
          res.writeHead(200, { "content-type": "text/event-stream" });
          res.write(prefix);
        }
        hangs.push(gate);
        req.socket.on("close", () => {
          const at = hangs.indexOf(gate);
          if (at >= 0) hangs.splice(at, 1);
          entry.aborted = true;
        });
        return;
      }
      if (item.mode === "error") {
        // echoBearer simulates the provider leak swiss must scrub: the received
        // Authorization value embedded in an error body that comes back.
        let message = item.message || "the fake failed on purpose";
        if (item.echoBearer && entry.authorization) message += " (echo " + entry.authorization + ")";
        res.writeHead(item.status || 500, jsonHeaders());
        res.end(JSON.stringify({ error: { message } }));
        return;
      }
      const body = sseBody(framesFor(item, model));
      if (item.mode === "abort") {
        res.writeHead(200, { "content-type": "text/event-stream" });
        res.write(body.slice(0, body.indexOf("data: [DONE]")));
        res.destroy();
        entry.aborted = true;
        return;
      }
      res.writeHead(200, sseHeaders(Buffer.byteLength(body)));
      res.end(body);
      return;
    }
    res.writeHead(404, jsonHeaders());
    res.end("{}");
  }

  const server = http.createServer((req, res) => {
    handle(req, res).catch(() => { try { res.writeHead(500, jsonHeaders()); res.end("{}"); } catch { /* already gone */ } });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;

  return {
    url: "http://127.0.0.1:" + port,
    port,
    async close() {
      for (const gate of hangs.splice(0)) { try { gate.res.destroy(); } catch { /* already closed */ } }
      if (typeof server.closeAllConnections === "function") server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
    },
    // Script the next behaviors; model null is the shared queue, a model name is
    // that model's private queue (interleaving stays deterministic per agent).
    script,
    // The journal, scoped: every request, or only chat round trips.
    requests: () => entries.slice(),
    chats: () => entries.filter((e) => e.path.endsWith("/chat/completions")),
    hangCount: () => hangs.length,
    releaseAll() {
      for (const gate of hangs.splice(0)) {
        try {
          if (gate.prefix) gate.res.end(gate.rest);
          else {
            gate.res.writeHead(200, sseHeaders(Buffer.byteLength(gate.rest)));
            gate.res.end(gate.rest);
          }
        } catch { /* the consumer went away; the journal already marked it */ }
      }
    },
  };
}

// Parse one SSE body the way a consumer must: data frames JSON-parsed, [DONE]
// recorded separately; callers concatenate payloads themselves. Raw-text
// containment is the wrong semantics - the provider DELIBERATELY splits
// answers across delta frames.
function parseSse(text) {
  const frames = [];
  let sawDone = false;
  for (const block of text.split(NL + NL)) {
    for (const line of block.split(NL)) {
      if (line.indexOf("data:") !== 0) continue;
      const payload = line.slice(5).trim();
      if (payload === "[DONE]") { sawDone = true; continue; }
      try { frames.push(JSON.parse(payload)); } catch { frames.push(null); }
    }
  }
  return { frames, sawDone };
}

// Direct-run selftest: loopback only, no gateway, no browser. Queued for grant.
// The hold case is the delicate one, and the deadlock is proven, not guessed:
// awaiting the held round trip's Response BEFORE release can never settle -
// the plain-hang handler writes no headers until released - so the selftest
// starts the fetch UNawaited, polls the journal until the gate is held,
// records that the Response is still pending as evidence, and only then
// releases.
if (process.argv[1] && process.argv[1].endsWith("fakeprov.mjs") && process.argv.includes("--selftest")) {
  const assert = (await import("node:assert/strict")).default;
  const stage = (s) => console.log("selftest stage: " + s);
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const fake = await fakeProvider({ models: ["m1"] });
  // Every fetch the selftest makes rides an AbortController it owns, so the
  // finally block (and the watchdog) can unstick any await without touching
  // anything outside this process.
  const controllers = [];
  const post = (content) => {
    const ctl = new AbortController();
    controllers.push(ctl);
    return fetch(fake.url + "/v1/chat/completions", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ model: "m1", messages: [{ role: "user", content }] }),
      signal: ctl.signal,
    });
  };
  const modelsGet = () => {
    const ctl = new AbortController();
    controllers.push(ctl);
    return fetch(fake.url + "/v1/models", { signal: ctl.signal });
  };
  // A 30s budget on the whole run: a stuck await terminates loudly.
  const watchdog = setTimeout(() => {
    console.error("selftest watchdog: 30s budget exhausted; aborting own fetches and exiting");
    for (const c of controllers.splice(0)) { try { c.abort(); } catch { /* already settled */ } }
    try { fake.releaseAll(); } catch { /* nothing held */ }
    setTimeout(() => process.exit(3), 2000);
  }, 30000);

  try {
    fake.script(null,
      { mode: "answer", text: "hello there" },
      { mode: "hang", then: { mode: "answer", text: "late answer" } },
      { mode: "burst", count: 200 });
    const contentOf = (s) => s.frames
      .filter((f) => f && f.choices && f.choices[0].delta && typeof f.choices[0].delta.content === "string")
      .map((f) => f.choices[0].delta.content).join("");
    const finishOf = (s) => s.frames.filter((f) => f && f.choices && f.choices[0].finish_reason != null).pop();

    stage("answer round trip (immediate response)");
    const r1 = await post("q");
    assert.equal(r1.status, 200);
    const s1 = parseSse(await r1.text());
    // The answer DELIBERATELY arrives as two deltas ("hello " + "there"): the
    // consumer must concatenate parsed payloads, never expect contiguous text.
    assert.equal(contentOf(s1), "hello there", "the content deltas concatenate to the exact greeting");
    const finish1 = finishOf(s1);
    assert.ok(finish1, "a finish frame exists");
    assert.equal(finish1.choices[0].finish_reason, "stop");
    assert.deepEqual(finish1.usage, { prompt_tokens: 12, completion_tokens: 4, total_tokens: 16 }, "the finish frame carries exact usage");
    assert.equal(s1.sawDone, true, "the stream ends with [DONE]");

    stage("hold round trip started (fetch NOT awaited yet)");
    const heldP = post("hold");
    // Readiness from the journal, not a fixed sleep: the server has the
    // request and the gate is held.
    const gateDeadline = Date.now() + 5000;
    for (;;) {
      if (fake.chats().length >= 2 && fake.hangCount() === 1) break;
      if (Date.now() > gateDeadline) throw new Error("the hold request never reached its gate; chats=" + fake.chats().length + " hangs=" + fake.hangCount());
      await sleep(50);
    }
    stage("gate held; probing that the Response stays pending before release");
    const settledSide = heldP.then(() => "settled", () => "settled");
    const pending = await Promise.race([settledSide, sleep(500).then(() => "pending")]);
    assert.equal(pending, "pending", "the held round trip sends no headers before release (the awaited-fetch deadlock, recorded at the readiness stage)");
    stage("releasing the gate");
    fake.releaseAll();
    const r2 = await heldP;
    assert.equal(r2.status, 200);
    const s2 = parseSse(await r2.text());
    assert.equal(contentOf(s2), "late answer", "release answers the held round trip, exactly");

    stage("burst round trip (200 deltas)");
    const r3 = await post("burst");
    const s3 = parseSse(await r3.text());
    let burstWant = "";
    for (let i = 0; i < 200; i++) burstWant += "b" + i + ";";
    assert.equal(contentOf(s3), burstWant, "the burst mode emits every piece, in order");

    stage("journal and models endpoint");
    assert.equal(fake.chats().length, 3, "the journal saw every request");
    assert.deepEqual(fake.chats()[0].body.messages[0].content, "q", "the journal keeps the parsed body");
    const models = await (await modelsGet()).json();
    assert.equal(models.data[0].id, "m1");
  } catch (err) {
    console.error("selftest failed: " + (err && err.stack ? err.stack : err));
    process.exitCode = 1;
  } finally {
    clearTimeout(watchdog);
    // Abort our own fetches (settled ones ignore it), release anything held,
    // then close: the process must terminate without any outside help.
    for (const c of controllers.splice(0)) { try { c.abort(); } catch { /* already settled */ } }
    try { fake.releaseAll(); } catch { /* nothing held */ }
    stage("cleanup: closing the fake server");
    await fake.close();
  }
  if (process.exitCode) process.exit(process.exitCode);
  console.log("fakeprov selftest: ok");
  process.exit(0);
}
