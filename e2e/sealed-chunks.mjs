// sealed-chunks.mjs — permanent e2e assertion helper for Swiss's sealed
// history chunks. Verifies, WITHOUT any cipher/LZ4/AES decoding, that the
// stored chunk chain of one turn and its producing call matches the exact
// text the public API resolved: piece hashes re-derived from raw lexemes,
// Unicode scalar counts and UTF-8 byte budgets checked against the stored
// columns, sealed-block envelope SHAPE checked (never echoed), and a raw
// whole-file scan proving a marker appears nowhere in plaintext.
//
// Permanent repo e2e helper, consumed by llm-chat.test.mjs; pure assertions,
// no gateway, no runtime. Node builtins only: node:sqlite (opened read-only,
// lazily), node:crypto,
// node:assert/strict, node:fs.

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

const CHUNK_CAP = 65536;

// The shared known-answer constants of the llm-chat long third answer: the
// fake provider's script entry and the privacy expectations are ONE constant.
export const LONG_TAIL = "TAIL-8e77-2c91";
export const LONG_ANSWER = "ANS3-zu03 " + "p".repeat(2600) + " " + LONG_TAIL;

// ---------------------------------------------------------------------------
// Raw-lexeme JSON walker.
//
// The resolved body text the Rust side returns is already compact and
// ASCII-key-sorted (serde_json Value::to_string), but this helper never
// trusts that: it re-parses the raw text, keeps every value's EXACT source
// span (so float spelling like 1.0 and escape spelling like \"a\u0041b\" are
// preserved byte-for-byte), and rebuilds folded objects by sorting DECODED
// keys by code point. JSON.parse runs first as full validation.
// ---------------------------------------------------------------------------

class Node_ {
  constructor(kind, start, end, decoded, entries) {
    this.kind = kind; // "value" | "object" | "array"
    this.start = start;
    this.end = end;
    this.decoded = decoded; // JS value (for keys and scalar checks)
    this.entries = entries; // object: [{ keyRaw, keyDecoded, value: Node_ }]
    this.items = entries; // array: [Node_]
  }
  raw(text) {
    return text.slice(this.start, this.end);
  }
}

function parseRaw(text) {
  JSON.parse(text); // validation: malformed input throws before we walk
  let i = 0;
  const ws = () => {
    while (i < text.length && " \t\n\r".includes(text[i])) i++;
  };
  const value = () => {
    ws();
    const start = i;
    const c = text[i];
    if (c === "{") {
      i++;
      const entries = [];
      ws();
      if (text[i] === "}") {
        i++;
      } else {
        for (;;) {
          ws();
          const keyStart = i;
          stringToken();
          const keyRaw = text.slice(keyStart, i);
          const keyDecoded = JSON.parse(keyRaw);
          ws();
          assert.equal(text[i], ":", "object colon");
          i++;
          entries.push({ keyRaw, keyDecoded, value: value() });
          ws();
          if (text[i] === ",") {
            i++;
            continue;
          }
          assert.equal(text[i], "}", "object close");
          i++;
          break;
        }
      }
      return new Node_("object", start, i, null, entries);
    }
    if (c === "[") {
      i++;
      const items = [];
      ws();
      if (text[i] === "]") {
        i++;
      } else {
        for (;;) {
          items.push(value());
          ws();
          if (text[i] === ",") {
            i++;
            continue;
          }
          assert.equal(text[i], "]", "array close");
          i++;
          break;
        }
      }
      return new Node_("array", start, i, null, items);
    }
    if (c === '"') {
      stringToken();
      return new Node_("value", start, i, JSON.parse(text.slice(start, i)), null);
    }
    // Number / true / false / null: consume the literal lexeme verbatim.
    while (i < text.length && !",}] \t\n\r".includes(text[i])) i++;
    const raw = text.slice(start, i);
    assert(raw.length > 0, "empty literal lexeme");
    return new Node_("value", start, i, JSON.parse(raw), null);
  };
  const stringToken = () => {
    assert.equal(text[i], '"', "string open");
    i++;
    while (text[i] !== '"') {
      if (text[i] === "\\") i++;
      i++;
    }
    i++;
  };
  const root = value();
  ws();
  assert.equal(i, text.length, "trailing bytes after the JSON document");
  return root;
}

// Code-point (Unicode scalar) sort of decoded keys — Rust's BTreeMap order.
const keyLess = (a, b) => {
  const A = Array.from(a);
  const B = Array.from(b);
  for (let k = 0; k < Math.min(A.length, B.length); k++) {
    // Array.from yields lone code points, so codePointAt(0) is the scalar.
    if (A[k] !== B[k]) return A[k].codePointAt(0) < B[k].codePointAt(0);
  }
  return A.length < B.length;
};

// Fold entries [{keyRaw, keyDecoded, valueRaw}] into compact text, keys
// sorted by decoded code points, values spliced as EXACT raw lexemes.
const foldObject = (entries) => {
  const sorted = [...entries].sort((x, y) => (keyLess(x.keyDecoded, y.keyDecoded) ? -1 : keyLess(y.keyDecoded, x.keyDecoded) ? 1 : 0));
  return "{" + sorted.map((e) => e.keyRaw + ":" + e.valueRaw).join(",") + "}";
};

const utf8Bytes = (s) => Buffer.byteLength(s, "utf8");
const scalarCount = (s) => Array.from(s).length;
const sha256hex = (s) => createHash("sha256").update(s, "utf8").digest("hex");

// ---------------------------------------------------------------------------
// Piece + envelope derivation from the RESOLVED (v1-shape) call body text,
// mirroring history.rs split_body: wire piece = {endpoint?,headers?,options?};
// system/tools scalars and each messages[i] become their own pieces; the
// stored envelope replaces them with hash strings and stamps top-level "v":2.
// ---------------------------------------------------------------------------

function derivePieces(resolvedCallBodyText) {
  const root = parseRaw(resolvedCallBodyText);
  assert.equal(root.kind, "object", "the resolved call body is a JSON object");
  const requestEntry = root.entries.find((e) => e.keyDecoded === "request");
  assert(requestEntry, "the resolved call body carries a request object");
  assert.equal(requestEntry.value.kind, "object", "request is an object");
  const text = resolvedCallBodyText;
  const req = requestEntry.value.entries;
  const pieces = [];
  const replaced = []; // request entries after piece substitution
  // Wire fold: the three keys, whichever present, in ASCII order, raw values.
  const wireKeys = ["endpoint", "headers", "options"].filter((k) => req.some((e) => e.keyDecoded === k));
  let wireHash = null;
  if (wireKeys.length > 0) {
    const wireText = foldObject(
      wireKeys.map((k) => {
        const e = req.find((x) => x.keyDecoded === k);
        return { keyRaw: e.keyRaw, keyDecoded: k, valueRaw: e.value.raw(text) };
      })
    );
    wireHash = sha256hex(wireText);
    pieces.push({ hash: wireHash, text: wireText });
  }
  for (const e of req) {
    if (wireKeys.includes(e.keyDecoded)) continue;
    if (e.keyDecoded === "system" || e.keyDecoded === "tools") {
      if (e.value.kind === "value" && e.value.decoded === null) {
        replaced.push({ keyRaw: e.keyRaw, keyDecoded: e.keyDecoded, valueRaw: e.value.raw(text) });
        continue; // a null scalar stays inline, never a piece
      }
      const pieceText = e.value.raw(text);
      const hash = sha256hex(pieceText);
      pieces.push({ hash, text: pieceText });
      replaced.push({ keyRaw: e.keyRaw, keyDecoded: e.keyDecoded, valueRaw: JSON.stringify(hash) });
      continue;
    }
    if (e.keyDecoded === "messages" && e.value.kind === "array") {
      const hashes = e.value.items.map((m) => {
        const pieceText = m.raw(text);
        const hash = sha256hex(pieceText);
        pieces.push({ hash, text: pieceText });
        return JSON.stringify(hash);
      });
      replaced.push({ keyRaw: e.keyRaw, keyDecoded: e.keyDecoded, valueRaw: "[" + hashes.join(",") + "]" });
      continue;
    }
    replaced.push({ keyRaw: e.keyRaw, keyDecoded: e.keyDecoded, valueRaw: e.value.raw(text) });
  }
  // The wire entry itself: its hash string, folded in with the rest (the
  // fold sorts, so push order does not matter).
  if (wireHash !== null) {
    replaced.push({ keyRaw: '"wire"', keyDecoded: "wire", valueRaw: JSON.stringify(wireHash) });
  }
  assert(pieces.length > 0, "a chunked call body must reference at least one piece");
  // Rebuild the envelope: same top-level entries, request replaced, "v":2.
  const topEntries = root.entries
    .filter((e) => e.keyDecoded !== "request" && e.keyDecoded !== "v")
    .map((e) => ({ keyRaw: e.keyRaw, keyDecoded: e.keyDecoded, valueRaw: e.value.raw(text) }));
  topEntries.push({ keyRaw: '"request"', keyDecoded: "request", valueRaw: foldObject(replaced) });
  topEntries.push({ keyRaw: '"v"', keyDecoded: "v", valueRaw: "2" });
  const envelopeText = foldObject(topEntries);
  return { envelopeText, pieces, wireHash };
}

// ---------------------------------------------------------------------------
// Stored-block contract, source-faithful (swiss-core envelope.rs Sealed +
// swiss-host seal.rs seal_block/open_block): a SEALED row's block bytes are
// serde_json::to_vec(&Sealed) - the FROZEN seven-field shape
// {lmg,alg,keySource,salt,iv,tag,ct}, camelCase on the wire, lmg === 1,
// every other field a string, ct non-empty. `is_sealed` is a Rust PREDICATE
// (object + lmg===1 + alg/ct strings), never a serialized field - it is
// never required here. This guard is the SEALED-ENGAGEMENT witness: every
// checked block MUST be sealed; non-JSON bytes (the degraded/plain coded
// class open_block tolerates) FAIL loudly here - classification without
// engagement is not the proof this run owes. A JSON object with any OTHER
// key set also fails. Field VALUES are never compared and never echoed -
// failures report key sets, positions and lengths only. Shape alone is
// still not cryptographic proof; the coupled API-decode assertions carry
// the authenticity story.
// ---------------------------------------------------------------------------

const SEALED_KEYS = ["alg", "ct", "iv", "keySource", "lmg", "salt", "tag"];

// BLOB normalization: node:sqlite hands BLOB columns back as Uint8Array,
// never trust String(blob) (it would render comma-joined numbers). Accept
// only byte views, normalize through Buffer.from, require non-empty bytes.
const blockBytes = (block, where) => {
  assert.ok(
    Buffer.isBuffer(block) || block instanceof Uint8Array || block instanceof ArrayBuffer,
    where + ": the block column is a byte view (BLOB), not " + typeof block
  );
  const bytes = Buffer.from(block);
  assert.ok(bytes.length > 0, where + ": the block holds non-empty bytes");
  return bytes;
};

const assertSealedBlock = (bytes, where) => {
  let parsed;
  try {
    parsed = JSON.parse(bytes.toString("utf8"));
  } catch {
    assert.fail(where + ": the block is NOT a sealed envelope - plain/degraded coded bytes fail the sealed-engagement guard");
  }
  assert.equal(typeof parsed, "object", where + ": a sealed block is an object");
  assert.ok(parsed !== null, where + ": a sealed block is not null");
  assert.equal(
    JSON.stringify(Object.keys(parsed).sort()),
    JSON.stringify(SEALED_KEYS),
    where + ": the block carries exactly the frozen seven-field Sealed shape (lmg,alg,keySource,salt,iv,tag,ct) - is_sealed is a predicate, not a wire field"
  );
  assert.equal(parsed.lmg, 1, where + ": lmg is the frozen marker value 1");
  for (const k of ["alg", "keySource", "salt", "iv", "tag", "ct"]) {
    assert.equal(typeof parsed[k], "string", where + ": " + k + " is a string (type only, value never echoed)");
  }
  assert.ok(parsed.ct.length > 0, where + ": ct is non-empty (length only)");
};

const assertChunkChain = (rows, expectedBytes, label) => {
  const classes = { sealed: 0, plain: 0 };
  assert.ok(rows.length > 0, label + ": at least one chunk part");
  const byteSets = [];
  rows.forEach((r, idx) => {
    assert.equal(r.part, idx, label + ": parts are contiguous from 0");
    assert.ok(r.codec === 0 || r.codec === 1, label + " part " + idx + ": codec is 0 or 1");
    assert.ok(Number.isInteger(r.plain) && r.plain > 0, label + " part " + idx + ": plain is a positive integer");
    assert.ok(r.plain <= CHUNK_CAP, label + " part " + idx + ": plain within the 64 KiB chunk cap");
    const bytes = blockBytes(r.block, label + " part " + idx);
    assertSealedBlock(bytes, label + " part " + idx);
    classes.sealed += 1;
    byteSets.push(bytes);
  });
  assert.equal(
    rows.reduce((sum, r) => sum + r.plain, 0),
    expectedBytes,
    label + ": the chunk plains sum to the exact UTF-8 byte length of the expected text"
  );
  assert.equal(classes.plain, 0, label + ": sealed engagement - every checked block is a sealed envelope, plain count is zero");
  return { classes, byteSets };
};

// ---------------------------------------------------------------------------
// Public entry point.
// ---------------------------------------------------------------------------

export async function assertStoredChunks({
  dbPath,
  runId,
  turnSeq,
  expectedTurnText,
  callId,
  resolvedCallBodyText,
  tailMarker,
}) {
  assert.equal(typeof dbPath, "string", "dbPath: path string");
  assert.ok(Number.isInteger(runId) && runId > 0, "runId: positive integer");
  assert.ok(Number.isInteger(turnSeq) && turnSeq >= 0, "turnSeq: non-negative integer");
  assert.ok(Number.isInteger(callId) && callId > 0, "callId: positive integer");
  assert.equal(typeof expectedTurnText, "string", "expectedTurnText: the exact turn body text");
  assert.equal(typeof resolvedCallBodyText, "string", "resolvedCallBodyText: the exact resolved call body text");
  assert.equal(typeof tailMarker, "string", "tailMarker: marker string (required by this contract)");

  // Derivation first: any shape surprise fails loudly before the DB opens.
  const { envelopeText, pieces } = derivePieces(resolvedCallBodyText);

  let turnChain = null;
  let callChain = null;

  const { DatabaseSync } = await import("node:sqlite");
  const db = new DatabaseSync(dbPath, { readOnly: true }); // SELECTs only
  try {
    // SAME turn: the run's seq-th turn, chunked.
    const turn = db
      .prepare("SELECT id, body_flag FROM turn WHERE run_id = ? AND seq = ?")
      .get(runId, turnSeq);
    assert.ok(turn, "turn (" + runId + "," + turnSeq + "): the row exists");
    assert.equal(turn.body_flag, 1, "turn (" + runId + "," + turnSeq + "): body_flag marks a chunked body");
    const turnId = turn.id;

    // Producing call: the call belongs to the same run, chunked.
    const call = db.prepare("SELECT run_id, body_flag FROM call WHERE id = ?").get(callId);
    assert.ok(call, "call " + callId + ": the row exists");
    assert.equal(call.run_id, runId, "call " + callId + " is filed under run " + runId);
    assert.equal(call.body_flag, 1, "call " + callId + ": body_flag marks a chunked body");

    // The turn's stored chain vs the exact expected turn text.
    const turnRows = db
      .prepare("SELECT part, codec, plain, block FROM turn_body WHERE turn_id = ? ORDER BY part")
      .all(turnId);
    turnChain = assertChunkChain(turnRows, utf8Bytes(expectedTurnText), "turn_body " + turnId);

    // The call's stored chain vs the DERIVED v2 envelope (piece-folded).
    const callRows = db
      .prepare("SELECT part, codec, plain, block FROM call_body WHERE call_id = ? ORDER BY part")
      .all(callId);
    callChain = assertChunkChain(callRows, utf8Bytes(envelopeText), "call_body " + callId);

    // Every referenced piece must verify — direct lookups, no count fallback.
    for (const piece of pieces) {
      const meta = db.prepare("SELECT chars FROM piece WHERE hash = ?").get(piece.hash);
      assert.ok(meta, "piece " + piece.hash + ": the hash row exists (referenced pieces are never optional)");
      assert.equal(
        meta.chars,
        scalarCount(piece.text),
        "piece " + piece.hash + ": the chars column is the Unicode SCALAR count of the piece text"
      );
      const rows = db
        .prepare("SELECT part, codec, plain, block FROM piece_body WHERE hash = ? ORDER BY part")
        .all(piece.hash);
      const pieceChain = assertChunkChain(rows, utf8Bytes(piece.text), "piece_body " + piece.hash);
      const marker = Buffer.from(tailMarker, "utf8");
      if (pieceChain.byteSets.some((b) => b.includes(marker))) {
        assert.fail("piece_body " + piece.hash + ": the tail marker leaked into a sealed block envelope");
      }
    }
    const marker = Buffer.from(tailMarker, "utf8");
    for (const chain of [turnChain, callChain]) {
      if (chain.byteSets.some((b) => b.includes(marker))) {
        assert.fail("a sealed block envelope carries the tail marker");
      }
    }
  } finally {
    db.close();
  }

  // Whole-file raw scan: the marker appears nowhere in plaintext. The main DB
  // is mandatory; -wal/-shm are scanned when present and skipped ONLY on a
  // real ENOENT — any other read error fails loudly.
  const scanned = [];
  for (const name of [dbPath, dbPath + "-wal", dbPath + "-shm"]) {
    let bytes;
    try {
      bytes = readFileSync(name);
    } catch (err) {
      if (name === dbPath) throw err; // the main DB is mandatory
      assert.equal(err.code, "ENOENT", name + ": the only skippable absence is a real ENOENT");
      continue;
    }
    scanned.push(name);
    const hit = bytes.indexOf(Buffer.from(tailMarker, "utf8"));
    assert.equal(hit, -1, name + ": the tail marker must not persist anywhere in plaintext (byte offset " + hit + ")");
  }

  return {
    ok: true,
    piecesVerified: pieces.length,
    envelopeUtf8Bytes: utf8Bytes(envelopeText),
    turnUtf8Bytes: utf8Bytes(expectedTurnText),
    turnBlockClasses: turnChain.classes,
    callBlockClasses: callChain.classes,
    filesScanned: scanned.length,
  };
}

// ---------------------------------------------------------------------------
// Pure in-memory smoke: the walker and the derivation only — no DB, no
// runtime, no files. Run with: node e2e/sealed-chunks.mjs
// ---------------------------------------------------------------------------

export function smoke() {
  // 1. Raw lexeme preservation: float spelling and escape spelling survive.
  const raw = '{"b":"a\\u0041b","a":1.0,"s":"x𝕊y"}';
  const root = parseRaw(raw);
  assert.equal(root.kind, "object", "smoke: root object");
  const a = root.entries.find((e) => e.keyDecoded === "a");
  assert.equal(a.value.raw(raw), "1.0", "smoke: float lexeme 1.0 preserved, not 1");
  const b = root.entries.find((e) => e.keyDecoded === "b");
  assert.equal(b.value.raw(raw), '"a\\u0041b"', "smoke: the raw backslash-u escape spelling survives verbatim");
  assert.equal(b.value.decoded, "aAb", "smoke: the escaped string still decodes");

  // 2. Unicode scalar vs UTF-8 byte accounting on a surrogate pair.
  assert.equal(scalarCount("x𝕊y"), 3, "smoke: one scalar per surrogate pair");
  assert.equal(utf8Bytes("x𝕊y"), 6, "smoke: 4 bytes for the astral char, 1 each ASCII");

  // 3. Fold sorts DECODED keys by code point, values verbatim.
  const folded = foldObject([
    { keyRaw: '"b"', keyDecoded: "b", valueRaw: "2" },
    { keyRaw: '"a"', keyDecoded: "a", valueRaw: '{"y":1,"x":2}' },
  ]);
  assert.equal(folded, '{"a":{"y":1,"x":2},"b":2}', "smoke: outer fold sorts; inner span stays verbatim");

  // 4. Full derivation round-trip on a synthetic resolved body.
  const msg = '{"role":"user","content":"x𝕊y"}';
  const resolved =
    '{"request":{"endpoint":"https://h.test/v1","headers":{"x":"\u0040"},"options":{"t":1.0},' +
    '"system":"be brief","tools":[],"messages":[' + msg + '],"prompt":"p"},"response":{"ok":true}}';
  const { envelopeText, pieces } = derivePieces(resolved);
  // The wire piece folds endpoint/headers/options in ASCII order from raw spans.
  assert.ok(
    pieces.some((p) => p.text === '{"endpoint":"https://h.test/v1","headers":{"x":"\u0040"},"options":{"t":1.0}}'),
    "smoke: wire piece folds raw spans, escape and float spelling intact"
  );
  assert.ok(pieces.some((p) => p.text === '"be brief"'), "smoke: the system scalar is its own piece");
  assert.ok(pieces.some((p) => p.text === "[]"), "smoke: the tools scalar is its own piece");
  assert.ok(pieces.some((p) => p.text === msg), "smoke: each message is its own piece");
  for (const p of pieces) {
    assert.equal(p.hash, sha256hex(p.text), "smoke: hash is sha256 of the piece text's UTF-8 bytes");
  }
  const env = JSON.parse(envelopeText);
  assert.equal(env.v, 2, "smoke: the envelope stamps v=2");
  assert.equal(typeof env.request.wire, "string", "smoke: wire becomes its hash string");
  assert.equal(env.request.messages.length, 1, "smoke: messages become hash strings");
  assert.equal(env.response.ok, true, "smoke: untouched top-level entries ride verbatim");
  assert.ok(utf8Bytes(envelopeText) > 0 && !envelopeText.includes("be brief"), "smoke: no piece text leaks inline");
  const chars = pieces.find((p) => p.text === msg);
  assert.equal(scalarCount(chars.text), scalarCount(msg), "smoke: scalar accounting is on the exact piece text");

  // 5. Stored-block contract against the REAL frozen shape (envelope.rs
  //    Sealed: {lmg,alg,keySource,salt,iv,tag,ct}; is_sealed is a predicate,
  //    not a field). Every checked block must ENGAGE the seal.
  const sealedEnvelope = JSON.stringify({
    lmg: 1,
    alg: "AES-256-GCM",
    keySource: "machineid",
    salt: "c2FsdA==",
    iv: "aXY=",
    tag: "dGFn",
    ct: "Y3Q=",
  });
  // Buffer BLOB: accepted.
  assertSealedBlock(Buffer.from(sealedEnvelope, "utf8"), "smoke buffer");
  // Uint8Array BLOB - what node:sqlite actually returns: accepted through the
  // same normalization, byte-for-byte (never String(blob)).
  const u8 = Uint8Array.from(Buffer.from(sealedEnvelope, "utf8"));
  assertSealedBlock(blockBytes(u8, "smoke u8"), "smoke u8");
  assert.ok(blockBytes(Buffer.from(sealedEnvelope, "utf8"), "smoke buf").equals(blockBytes(u8, "smoke u8")), "smoke: Buffer and Uint8Array normalize identically");
  // A non-byte-view block (a bare string) fails the type guard.
  assert.throws(() => blockBytes(sealedEnvelope, "smoke type"), /byte view/, "smoke: a non-BLOB block fails the byte-view guard");
  // Plain/degraded non-JSON coded bytes FAIL the sealed-engagement guard.
  assert.throws(
    () => assertSealedBlock(Buffer.from([0x1f, 0x8b, 0x70, 0x6c, 0x61, 0x69, 0x6e]), "smoke plain"),
    /NOT a sealed envelope/,
    "smoke: plain/degraded coded bytes are rejected, not classified away"
  );
  // The OLD invented shape is rejected.
  assert.throws(
    () => assertSealedBlock(Buffer.from(JSON.stringify({ alg: "a", ct: "c", is_sealed: true, lmg: 1 }), "utf8"), "smoke old"),
    /seven-field/,
    "smoke: the OLD invented shape is rejected"
  );
  assert.throws(
    () => assertSealedBlock(Buffer.from(JSON.stringify({ lmg: 0, alg: "a", keySource: "k", salt: "s", iv: "i", tag: "t", ct: "c" }), "utf8"), "smoke lmg"),
    /marker value 1/,
    "smoke: a wrong lmg is rejected"
  );
  assert.throws(
    () => assertSealedBlock(Buffer.from(JSON.stringify({ lmg: 1, alg: "a", keySource: "k", salt: "s", iv: "i", ct: "c" }), "utf8"), "smoke missing"),
    /seven-field/,
    "smoke: a missing field (tag) is rejected"
  );
  assert.throws(
    () => assertSealedBlock(Buffer.from(JSON.stringify({ lmg: 1, alg: "a", keySource: "k", salt: "s", iv: "i", tag: 7, ct: "c" }), "utf8"), "smoke type"),
    /tag is a string/,
    "smoke: a non-string field is rejected"
  );
  return "smoke ok: lexemes, scalars, fold order, derivation, hashes, sealed engagement";
}

if (process.argv[1] && process.argv[1].endsWith("sealed-chunks.mjs") && !process.env.SCG_NO_SMOKE) {
  console.log(smoke());
}
