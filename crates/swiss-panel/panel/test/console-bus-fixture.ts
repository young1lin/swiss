/* The real link and codecs, with an in-memory peer (SPEC §bus.requests). No mocked busRequest. */
import { create, fromBinary, toBinary } from "../src/bus/codec.js";
import { installBus, type BusSocket } from "../src/bus/bus.js";
import { FaultSchema, FrameSchema, Kind, LinkId, Module, TopicsSchema, WelcomeSchema, type Frame } from "../src/bus/pb/swiss/bus/v1/bus_pb.js";
import { ConsoleGoneSchema, ConsoleListSchema, ConsolePutSchema, ConsoleRemoveSchema, ConsoleRemovedSchema,
  ConsoleSavedSchema, ConsolesClearedSchema, ConsolesId, ConsolesOfSchema, ConsolesPageSchema, DataSub, SaveMode,
} from "../src/bus/pb/swiss/bus/v1/data_pb.js";
import { PanelSub, PrefsId, PrefsSchema } from "../src/bus/pb/swiss/bus/v1/panel_pb.js";
import { consoleFromWire } from "../src/data-console-store.js";
import type { ConsoleRecord } from "../src/data-console-sync.js";

export interface ConsoleCall { method: string; url: string; topic?: string; body: Record<string, unknown> | null }
export interface Reply { body?: Record<string, unknown>; fault?: { code: string; message: string } }
export class ConsolePeer implements BusSocket {
  static current: ConsolePeer;
  binaryType: BinaryType = "arraybuffer";
  private listeners = new Map<string, Array<(ev: Event) => void>>();
  private seq = 1;
  sent: Frame[] = [];
  calls: ConsoleCall[] = [];
  subscriptions: string[] = [];
  answer: (call: ConsoleCall) => Reply | Promise<Reply> = () => ({ body: { consoles: [] } });
  constructor() { ConsolePeer.current = this; }
  addEventListener(type: string, listener: (ev: Event) => void): void {
    this.listeners.set(type, [...(this.listeners.get(type) || []), listener]);
  }
  removeEventListener(type: string, listener: (ev: Event) => void): void {
    this.listeners.set(type, (this.listeners.get(type) || []).filter(fn => fn !== listener));
  }
  private emit(type: string, ev: Event): void { for (const fn of this.listeners.get(type) || []) fn(ev); }
  close(): void { this.emit("close", new Event("close")); }
  open(build = "test"): void {
    this.emit("open", new Event("open"));
    this.receive({ module: 0, sub: 1, id: LinkId.HELLO, kind: Kind.RESPONSE, replyTo: 1,
      body: toBinary(WelcomeSchema, create(WelcomeSchema, { protocol: 1, connId: "test", build, heartbeatMs: 20000n, maxFrame: 4194304 })) });
  }
  private receive(fields: Partial<Omit<Frame, "$typeName">>): void {
    const bytes = toBinary(FrameSchema, create(FrameSchema, { seq: this.seq++, ...fields }));
    this.emit("message", new MessageEvent("message", { data: bytes.buffer }));
  }
  send(bytes: Uint8Array): void {
    const frame = fromBinary(FrameSchema, bytes);
    this.sent.push(frame);
    if (frame.module === 0) {
      if (frame.sub === 2) {
        const topics = fromBinary(TopicsSchema, frame.body);
        this.subscriptions.push(...topics.topics.map(t => t.key));
        queueMicrotask(() => this.respond(frame, new Uint8Array()));
      }
      return;
    }
    if (frame.module !== Module.DATA) return;
    let call: ConsoleCall;
    switch (frame.id) {
      case ConsolesId.CONSOLES_LIST: {
        const body = fromBinary(ConsolesPageSchema, frame.body);
        call = { method: "LIST", url: "", topic: "DATA.CONSOLES", body }; break;
      }
      case ConsolesId.CONSOLES_PUT: {
        const body = fromBinary(ConsolePutSchema, frame.body);
        call = { method: "PUT", url: "", topic: "DATA.CONSOLES", body: { ...body,
          console: body.console ? consoleFromWire(body.console) : undefined,
          mode: body.mode === SaveMode.CREATE ? "create" : body.mode === SaveMode.UPDATE ? "update" : "auto" } }; break;
      }
      case ConsolesId.CONSOLES_REMOVE:
        call = { method: "REMOVE", url: "", topic: "DATA.CONSOLES", body: fromBinary(ConsoleRemoveSchema, frame.body) }; break;
      default:
        call = { method: "CLEAR_CLOSED", url: "", topic: "DATA.CONSOLES", body: fromBinary(ConsolesOfSchema, frame.body) };
    }
    this.calls.push(call);
    void Promise.resolve().then(() => this.answer(call)).then(reply => {
      if (reply.fault) { this.receive({ ...frame, kind: Kind.RESPONSE, replyTo: frame.seq, body: new Uint8Array(),
        fault: create(FaultSchema, reply.fault) }); return; }
      const body = reply.body || {};
      switch (frame.id) {
        case ConsolesId.CONSOLES_LIST: {
          const consoles = (body.consoles || []) as ConsoleRecord[];
          this.respond(frame, toBinary(ConsoleListSchema, create(ConsoleListSchema, {
            consoles: consoles.map(c => ({ ...c, revision: BigInt(c.revision), createdAt: BigInt(c.createdAt), updatedAt: BigInt(c.updatedAt), agentTab: c.agentTab ?? undefined, closedAt: c.closedAt == null ? undefined : BigInt(c.closedAt) })), next: String(body.next || ""),
          }))); break;
        }
        case ConsolesId.CONSOLES_PUT: {
          const console = body.console as ConsoleRecord | undefined;
          this.respond(frame, toBinary(ConsoleSavedSchema, create(ConsoleSavedSchema, { console: console ? {
            ...console, revision: BigInt(console.revision), createdAt: BigInt(console.createdAt), updatedAt: BigInt(console.updatedAt), agentTab: console.agentTab ?? undefined, closedAt: console.closedAt == null ? undefined : BigInt(console.closedAt),
          } : undefined }))); break;
        }
        case ConsolesId.CONSOLES_REMOVE:
          this.respond(frame, toBinary(ConsoleRemovedSchema, create(ConsoleRemovedSchema, { id: String(call.body?.id || ""), removed: body.removed === true }))); break;
        default:
          this.respond(frame, toBinary(ConsolesClearedSchema, create(ConsolesClearedSchema, { cleared: Number(body.cleared || 0) })));
      }
    });
  }
  private respond(request: Frame, body: Uint8Array): void {
    this.receive({ module: request.module, sub: request.sub, id: request.id, kind: Kind.RESPONSE, replyTo: request.seq, body });
  }
  changed(conn: string, console: ConsoleRecord, origin = "other-page"): void {
    this.receive({ module: Module.DATA, sub: DataSub.CONSOLES, id: ConsolesId.CONSOLES_CHANGED,
      kind: Kind.EVENT, key: conn, origin, body: toBinary(ConsoleSavedSchema, create(ConsoleSavedSchema, { console: {
        ...console, revision: BigInt(console.revision), createdAt: BigInt(console.createdAt), updatedAt: BigInt(console.updatedAt), agentTab: console.agentTab ?? undefined, closedAt: console.closedAt == null ? undefined : BigInt(console.closedAt),
      } })) });
  }
  prefs(choice: { lang?: string; theme?: string; codeScheme?: string }): void {
    this.receive({ module: Module.PANEL, sub: PanelSub.PREFS, id: PrefsId.PREFS_CHANGED, kind: Kind.RELAY,
      origin: "other-page", body: toBinary(PrefsSchema, create(PrefsSchema, choice)) });
  }
  removed(conn: string, id: string): void {
    this.receive({ module: Module.DATA, sub: DataSub.CONSOLES, id: ConsolesId.CONSOLES_REMOVED,
      kind: Kind.EVENT, key: conn, body: toBinary(ConsoleGoneSchema, create(ConsoleGoneSchema, { id })) });
  }
  cleared(conn: string): void {
    this.receive({ module: Module.DATA, sub: DataSub.CONSOLES, id: ConsolesId.CONSOLES_CLEARED,
      kind: Kind.EVENT, key: conn, body: toBinary(ConsolesClearedSchema, create(ConsolesClearedSchema, { cleared: 1 })) });
  }
}
export function consolePeer(): ConsolePeer {
  installBus({ socket: ConsolePeer, url: "ws://127.0.0.1/api/bus", random: () => 0.5 });
  const peer = ConsolePeer.current;
  peer.open();
  return peer;
}
