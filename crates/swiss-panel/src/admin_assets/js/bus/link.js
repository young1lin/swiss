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

/* One WebSocket per page (SPEC §bus.link). The constructor is injectable so a test
   drives real frames through a fake socket and never opens a port. Timings that WELCOME
   carries (heartbeat_ms, max_frame) come from that frame; the constants below are the
   same numbers the server sends, checked against crates/swiss-bus. */

import { create, fromBinary, toBinary } from "./codec.js";
import {
  ByeReason,
  ByeSchema,
  BuildSchema,
  FrameSchema,
  HelloSchema,
  Kind,
  LinkId,
  PingSchema,
  WelcomeSchema,
             
} from "./pb/swiss/bus/v1/bus_pb.js";

export const PROTOCOL = 1;
export const HEARTBEAT_MS = 20_000;
export const MAX_FRAME = 4 * 1024 * 1024;
export const PONG_DEADLINE_MS = 10_000;
export const REQUEST_TIMEOUT_MS = 15_000;
export const RECONNECT_MIN_MS = 500;
export const RECONNECT_MAX_MS = 30_000;
export const RECONNECT_NOTICE_MS = 3_000;

const LOCAL_FAULTS                         = {
  "bus.offline": "the panel is not connected",
  "bus.timeout": "no response in 15 s",
  "bus.tooLarge": "the frame is past the limit",
};

export class BusFault extends Error {
  code        ;
  params                        ;
  constructor(code        , message        , params                        ) {
    super(message);
    this.name = "BusFault";
    this.code = code;
    this.params = params;
  }
}

export function localFault(code        )           {
  return new BusFault(code, LOCAL_FAULTS[code] ?? code, {});
}

;                           
                         
                               
                
                                                                      
                                                                         
 

;                            
                                          
                        
                                   
                                
                                   
               
 

;                             
                                
                                
                      
 

class BrowserSocket                      {
          ws           ;
  constructor(url        ) {
    this.ws = new WebSocket(url);
    this.ws.binaryType = "arraybuffer";
  }
  get binaryType()             { return this.ws.binaryType; }
  set binaryType(value            ) { this.ws.binaryType = value; }
  send(data            )       {
    const copy = new Uint8Array(data.byteLength);
    copy.set(data);
    this.ws.send(copy);
  }
  close()       { this.ws.close(); }
  addEventListener(type        , listener                     )       {
    this.ws.addEventListener(type, listener                 );
  }
  removeEventListener(type        , listener                     )       {
    this.ws.removeEventListener(type, listener                 );
  }
}

function freshPageId()         {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  let hex = "";
  for (const b of bytes) hex += b.toString(16).padStart(2, "0");
  return hex;
}

export const pageId         = freshPageId();

let socketCtor                                 = BrowserSocket;
let rng               = Math.random;
let reload                          = () => {};
let info                         = () => Promise.resolve();
let notice                          = () => {};
let url = "";
let handlers              = { event: () => {}, relay: () => {}, welcome: () => {} };

let sock                   = null;
let generation = 0;
let seq = 0;
let welcomed = false;
let knownBuild = "";
let welcomes = 0;
let heartbeatMs = HEARTBEAT_MS;
let maxFrame = MAX_FRAME;
let attempt = 0;
let reconnectTimer                                           ;
let pingTimer                                           ;
let pongTimer                                           ;
let noticeTimer                                           ;
let noticeShown = false;
let immediate = false;
let protocolReload = false;
let listening = false;

                
                                       
                                  
                                  
 

const pending = new Map              ();

export function bindBus(next             )       { handlers = next; }

export function isWelcomed()          { return welcomed; }

export function installLink(opts            )       {
  socketCtor = opts.socket ?? BrowserSocket;
  rng = opts.random ?? Math.random;
  reload = opts.reload ?? (() => {});
  info = opts.info ?? (() => Promise.resolve());
  notice = opts.notice ?? (() => {});
  url = opts.url ?? defaultUrl();
  resetLink();
  listen();
  connect();
}

function defaultUrl()         {
  if (typeof location === "undefined") return "ws://127.0.0.1/api/bus";
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  return proto + "//" + location.host + "/api/bus";
}

function resetLink()       {
  generation += 1;
  const current = sock;
  sock = null;
  if (current) current.close();
  clearTimeout(reconnectTimer);
  clearTimeout(pingTimer);
  clearTimeout(pongTimer);
  clearTimeout(noticeTimer);
  reconnectTimer = undefined;
  pingTimer = undefined;
  pongTimer = undefined;
  noticeTimer = undefined;
  if (noticeShown) notice(false);
  noticeShown = false;
  welcomed = false;
  knownBuild = "";
  welcomes = 0;
  heartbeatMs = HEARTBEAT_MS;
  maxFrame = MAX_FRAME;
  attempt = 0;
  immediate = false;
  protocolReload = false;
  seq = 0;
  failPending("bus.offline");
}

function listen()       {
  if (listening || typeof window === "undefined") return;
  listening = true;
  window.addEventListener("online", nudge);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") nudge();
  });
}

/** online, or the page becoming visible (SPEC §bus.link): ping if up, connect if not. */
export function nudge()       {
  if (welcomed && sock) sendPing();
  else if (!sock && !protocolReload) {
    clearTimeout(reconnectTimer);
    reconnectTimer = undefined;
    connect();
  }
}

function connect()       {
  if (sock) return;
  const gen = ++generation;
  seq = 0;
  const current = new socketCtor(url);
  sock = current;
  current.binaryType = "arraybuffer";
  let opened = false;
  current.addEventListener("open", () => {
    if (gen !== generation || sock !== current) return;
    opened = true;
    sendHello();
  });
  current.addEventListener("message", (ev       ) => {
    if (gen !== generation || sock !== current) return;
    const bytes = bytesOf(ev);
    if (!bytes) {
      current.close();
      return;
    }
    onBytes(bytes);
  });
  current.addEventListener("close", () => {
    if (gen !== generation) return;
    if (sock === current) sock = null;
    onClose(opened);
  });
}

function bytesOf(ev       )                    {
  if (!("data" in ev)) return null;
  const data          = ev.data;
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (data instanceof Uint8Array) return data;
  return null;
}

function onBytes(bytes            )       {
  let frame       ;
  try { frame = fromBinary(FrameSchema, bytes); }
  catch {
    sock?.close();
    return;
  }
  if (welcomed) armHeartbeat();
  const link = frame.module === 0 && frame.sub === 1;
  if (link && frame.kind === Kind.RESPONSE && frame.id === LinkId.HELLO) {
    onWelcome(frame);
    return;
  }
  if (link && frame.kind === Kind.RESPONSE && frame.id === LinkId.PING) {
    clearTimeout(pongTimer);
    pongTimer = undefined;
    return;
  }
  if (link && frame.kind === Kind.EVENT && frame.id === LinkId.BYE) {
    const bye = fromBinary(ByeSchema, frame.body);
    if (bye.reason === ByeReason.BYE_SLOW) immediate = true;
    if (bye.reason === ByeReason.BYE_PROTOCOL) protocolReload = true;
    return;
  }
  if (link && frame.kind === Kind.EVENT && frame.id === LinkId.BUILD) {
    const build = fromBinary(BuildSchema, frame.body);
    if (build.build !== knownBuild) reload(build.build);
    return;
  }
  if (frame.kind === Kind.RESPONSE) {
    const wait = pending.get(frame.replyTo);
    if (!wait) return;
    clearTimeout(wait.timer);
    pending.delete(frame.replyTo);
    wait.resolve(frame);
    return;
  }
  if (frame.kind === Kind.EVENT) handlers.event(frame);
  else if (frame.kind === Kind.RELAY) handlers.relay(frame);
}

function onWelcome(frame       )       {
  const welcome = fromBinary(WelcomeSchema, frame.body);
  heartbeatMs = Number(welcome.heartbeatMs);
  maxFrame = welcome.maxFrame;
  welcomed = true;
  attempt = 0;
  welcomes += 1;
  if (welcomes > 1 && welcome.build !== knownBuild) reload(welcome.build);
  knownBuild = welcome.build;
  markUp();
  armHeartbeat();
  handlers.welcome();
}

function onClose(opened         )       {
  clearTimeout(pingTimer);
  clearTimeout(pongTimer);
  pingTimer = undefined;
  pongTimer = undefined;
  welcomed = false;
  failPending("bus.offline");
  if (!opened) void info().catch(() => {});
  if (protocolReload) {
    protocolReload = false;
    reload(knownBuild);
    return;
  }
  markDown();
  schedule();
}

function schedule()       {
  let delay        ;
  if (immediate) {
    immediate = false;
    delay = 0;
  } else {
    const base = Math.min(RECONNECT_MAX_MS, RECONNECT_MIN_MS * 2 ** attempt);
    const factor = 1 + (rng() * 2 - 1) * 0.2;
    delay = Math.round(base * factor);
    attempt += 1;
  }
  reconnectTimer = windowOrGlobalTimeout(() => {
    reconnectTimer = undefined;
    connect();
  }, delay);
}

function markDown()       {
  if (noticeTimer || noticeShown) return;
  noticeTimer = windowOrGlobalTimeout(() => {
    noticeTimer = undefined;
    noticeShown = true;
    notice(true);
  }, RECONNECT_NOTICE_MS);
}

function markUp()       {
  clearTimeout(noticeTimer);
  noticeTimer = undefined;
  if (noticeShown) notice(false);
  noticeShown = false;
}

function armHeartbeat()       {
  clearTimeout(pingTimer);
  pingTimer = windowOrGlobalTimeout(() => {
    pingTimer = undefined;
    sendPing();
  }, heartbeatMs);
}

function sendPing()       {
  if (!sock || !welcomed) return;
  const n = nextSeq();
  const body = toBinary(PingSchema, create(PingSchema, { sentMs: BigInt(Date.now()) }));
  emit(0, 1, LinkId.PING, Kind.REQUEST, "", body, n);
  clearTimeout(pongTimer);
  pongTimer = windowOrGlobalTimeout(() => {
    pongTimer = undefined;
    sock?.close();
  }, PONG_DEADLINE_MS);
}

function sendHello()       {
  const n = nextSeq();
  const body = toBinary(HelloSchema, create(HelloSchema, { protocol: PROTOCOL, pageId, build: knownBuild }));
  emit(0, 1, LinkId.HELLO, Kind.REQUEST, "", body, n);
}

function nextSeq()         {
  seq += 1;
  return seq;
}

function emit(module        , sub        , id        , kind        , key        , body            , n        )          {
  if (!sock) return false;
  const bytes = toBinary(FrameSchema, create(FrameSchema, {
    module, sub, id, kind, seq: n, key, body,
  }));
  if (bytes.byteLength > maxFrame) return false;
  sock.send(bytes);
  return true;
}

export function sendFrame(module        , sub        , id        , kind        , key        , body            )          {
  if (!sock || !welcomed) return false;
  return emit(module, sub, id, kind, key, body, nextSeq());
}

export function linkRequest(module        , sub        , id        , key        , body            )                 {
  if (!sock || !welcomed) return Promise.reject(localFault("bus.offline"));
  const n = nextSeq();
  const bytes = toBinary(FrameSchema, create(FrameSchema, {
    module, sub, id, kind: Kind.REQUEST, seq: n, key, body,
  }));
  if (bytes.byteLength > maxFrame) return Promise.reject(localFault("bus.tooLarge"));
  return new Promise((resolve, reject) => {
    const timer = windowOrGlobalTimeout(() => {
      pending.delete(n);
      reject(localFault("bus.timeout"));
    }, REQUEST_TIMEOUT_MS);
    pending.set(n, { timer, resolve, reject });
    if (sock) sock.send(bytes);
  });
}

function failPending(code        )       {
  for (const wait of pending.values()) {
    clearTimeout(wait.timer);
    wait.reject(localFault(code));
  }
  pending.clear();
}

function windowOrGlobalTimeout(fn            , ms        )                                {
  return setTimeout(fn, ms);
}
