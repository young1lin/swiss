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

/* The same golden frames the Rust suite reads (SPEC §bus.codegen). Each one is decoded,
   compared field by field, and re-encoded to the same bytes. */

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { fromBinary, toBinary } from "../src/bus/codec.js";
import {
  ByeReason,
  ByeSchema,
  FrameSchema,
  HelloSchema,
  Kind,
  PingSchema,
  PongSchema,
  TopicsSchema,
  WelcomeSchema,
  type Frame,
} from "../src/bus/pb/swiss/bus/v1/bus_pb.js";
import {
  ConsoleListSchema,
  ConsolePutSchema,
  ConsoleSavedSchema,
  SaveMode,
} from "../src/bus/pb/swiss/bus/v1/data_pb.js";
import { PrefsSchema } from "../src/bus/pb/swiss/bus/v1/panel_pb.js";
import { HEARTBEAT_MS, MAX_FRAME, PONG_DEADLINE_MS, PROTOCOL, RECONNECT_MAX_MS, RECONNECT_MIN_MS, RECONNECT_NOTICE_MS, REQUEST_TIMEOUT_MS } from "../src/bus/link.js";

const PING_HEX = "1001180220012807420708c0ab93af9a34";
const PONG_HEX = "10011802200228093007420e08c0ab93af9a3410c3ab93af9a34";

interface Golden {
  name: string;
  hex: string;
  bodyType: string;
  body: Record<string, unknown>;
  fault?: { code: string; message: string; params: Record<string, string> };
  module: number;
  sub: number;
  id: number;
  kind: string;
  seq: number;
  replyTo?: number;
  key?: string;
  origin?: string;
}

function unhex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

function kindName(kind: number): string {
  if (kind === Kind.REQUEST) return "REQUEST";
  if (kind === Kind.RESPONSE) return "RESPONSE";
  if (kind === Kind.EVENT) return "EVENT";
  if (kind === Kind.RELAY) return "RELAY";
  return "UNSPECIFIED";
}

function assertBody(entry: Golden, frame: Frame): void {
  if (entry.bodyType === "Ping") {
    const ping = fromBinary(PingSchema, frame.body);
    expect(ping.sentMs, entry.name).toBe(BigInt(entry.body.sentMs as number));
  } else if (entry.bodyType === "Pong") {
    const pong = fromBinary(PongSchema, frame.body);
    expect(pong.sentMs, entry.name).toBe(BigInt(entry.body.sentMs as number));
    expect(pong.serverMs, entry.name).toBe(BigInt(entry.body.serverMs as number));
  } else if (entry.bodyType === "Hello") {
    const hello = fromBinary(HelloSchema, frame.body);
    expect(hello.protocol, entry.name).toBe(entry.body.protocol);
    expect(hello.pageId, entry.name).toBe(entry.body.pageId);
    expect(hello.build, entry.name).toBe(entry.body.build);
  } else if (entry.bodyType === "Welcome") {
    const welcome = fromBinary(WelcomeSchema, frame.body);
    expect(welcome.protocol, entry.name).toBe(entry.body.protocol);
    expect(welcome.connId, entry.name).toBe(entry.body.connId);
    expect(welcome.build, entry.name).toBe(entry.body.build);
    expect(welcome.heartbeatMs, entry.name).toBe(BigInt(entry.body.heartbeatMs as number));
    expect(welcome.maxFrame, entry.name).toBe(entry.body.maxFrame);
    expect(welcome.serverMs, entry.name).toBe(BigInt(entry.body.serverMs as number));
  } else if (entry.bodyType === "Bye") {
    const bye = fromBinary(ByeSchema, frame.body);
    expect(bye.reason, entry.name).toBe(ByeReason.BYE_SLOW);
  } else if (entry.bodyType === "Topics") {
    const topics = fromBinary(TopicsSchema, frame.body);
    expect(topics.topics, entry.name).toHaveLength(1);
    expect(topics.topics[0].module, entry.name).toBe(6);
    expect(topics.topics[0].sub, entry.name).toBe(1);
    expect(topics.topics[0].key, entry.name).toBe("alpha");
  } else if (entry.bodyType === "ConsolePut") {
    const put = fromBinary(ConsolePutSchema, frame.body);
    expect(put.conn, entry.name).toBe("alpha");
    expect(put.mode, entry.name).toBe(SaveMode.UPDATE);
    const record = put.console;
    expect(record, entry.name).toBeDefined();
    if (!record) return;
    expect(record.id, entry.name).toBe("c1");
    expect(record.tab, entry.name).toBe("scratch");
    expect(record.agent, entry.name).toBe(true);
    expect(record.agentTab, entry.name).toBe("agent-1");
    expect(record.open, entry.name).toBe(true);
    expect(record.sql, entry.name).toBe("select 1");
    expect(record.revision, entry.name).toBe(3n);
    expect(record.createdAt, entry.name).toBe(1_700_000_000_000n);
    expect(record.updatedAt, entry.name).toBe(1_700_000_000_100n);
    expect(record.closedAt, entry.name).toBe(1_700_000_000_000n);
  } else if (entry.bodyType === "ConsoleList") {
    const list = fromBinary(ConsoleListSchema, frame.body);
    expect(list.next, entry.name).toBe("c1");
    expect(list.consoles, entry.name).toHaveLength(1);
    expect(list.consoles[0].id, entry.name).toBe("c1");
  } else if (entry.bodyType === "ConsoleSaved") {
    const saved = fromBinary(ConsoleSavedSchema, frame.body);
    expect(saved.console?.id, entry.name).toBe("c1");
  } else if (entry.bodyType === "Prefs") {
    const prefs = fromBinary(PrefsSchema, frame.body);
    expect(prefs.lang, entry.name).toBe("zh");
    expect(prefs.theme, entry.name).toBeUndefined();
    expect(prefs.codeScheme, entry.name).toBeUndefined();
  } else if (entry.bodyType === "") {
    expect(frame.body.length, entry.name).toBe(0);
  } else {
    throw new Error(entry.name + ": unknown body " + entry.bodyType);
  }
}

describe("golden frames (SPEC §bus.codegen)", () => {
  const text = readFileSync(new URL("../../../swiss-bus/testdata/golden.json", import.meta.url), "utf8");
  const entries = JSON.parse(text) as Golden[];

  it("decodes every frame to its fields and re-encodes to its bytes", () => {
    expect(entries.length).toBeGreaterThanOrEqual(11);
    for (const entry of entries) {
      if (entry.name === "ping") expect(entry.hex).toBe(PING_HEX);
      if (entry.name === "pong") expect(entry.hex).toBe(PONG_HEX);
      const bytes = unhex(entry.hex);
      const frame = fromBinary(FrameSchema, bytes);
      expect(frame.module, entry.name).toBe(entry.module);
      expect(frame.sub, entry.name).toBe(entry.sub);
      expect(frame.id, entry.name).toBe(entry.id);
      expect(kindName(frame.kind), entry.name).toBe(entry.kind);
      expect(frame.seq, entry.name).toBe(entry.seq);
      expect(frame.replyTo, entry.name).toBe(entry.replyTo ?? 0);
      expect(frame.key, entry.name).toBe(entry.key ?? "");
      expect(frame.origin, entry.name).toBe(entry.origin ?? "");
      if (entry.fault) {
        expect(frame.fault?.code, entry.name).toBe(entry.fault.code);
        expect(frame.fault?.message, entry.name).toBe(entry.fault.message);
        expect(frame.fault?.params, entry.name).toEqual(entry.fault.params);
      } else {
        expect(frame.fault, entry.name).toBeUndefined();
      }
      assertBody(entry, frame);
      expect(hexOf(toBinary(FrameSchema, frame)), entry.name).toBe(entry.hex);
    }
  });
});

describe("bus timings match the Rust constants (SPEC §bus.link)", () => {
  const rust = readFileSync(new URL("../../../swiss-bus/src/lib.rs", import.meta.url), "utf8");

  it("uses the same limits the server names", () => {
    expect(rust).toContain("PROTOCOL: u32 = 1");
    expect(PROTOCOL).toBe(1);
    expect(rust).toContain("HEARTBEAT_MS: u64 = 20_000");
    expect(HEARTBEAT_MS).toBe(20_000);
    expect(rust).toContain("MAX_FRAME: usize = 4 * 1024 * 1024");
    expect(MAX_FRAME).toBe(4 * 1024 * 1024);
    expect(rust).toContain("PONG_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10)");
    expect(PONG_DEADLINE_MS).toBe(10_000);
    expect(rust).toContain("REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15)");
    expect(REQUEST_TIMEOUT_MS).toBe(15_000);
    expect(rust).toContain("RECONNECT_MIN: std::time::Duration = std::time::Duration::from_millis(500)");
    expect(RECONNECT_MIN_MS).toBe(500);
    expect(rust).toContain("RECONNECT_MAX: std::time::Duration = std::time::Duration::from_secs(30)");
    expect(RECONNECT_MAX_MS).toBe(30_000);
    expect(rust).toContain("RECONNECT_NOTICE: std::time::Duration = std::time::Duration::from_secs(3)");
    expect(RECONNECT_NOTICE_MS).toBe(3_000);
  });
});
