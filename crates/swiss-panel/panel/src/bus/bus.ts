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

/* The API a view uses (SPEC §bus.topics, SPEC §bus.requests). The client counts
   subscribers, so two views of one topic subscribe once; the server does not. After
   every WELCOME the page subscribes again and each resync listener re-reads. */

import { create, fromBinary, toBinary } from "./codec.js";
import type { DescMessage } from "../vendor/protobuf/2.16.0/descriptors.js";
import type { MessageShape } from "../vendor/protobuf/2.16.0/types.js";
import { Kind, TopicId, TopicsSchema, type Frame } from "./pb/swiss/bus/v1/bus_pb.js";
import {
  bindBus,
  BusFault,
  installLink,
  isWelcomed,
  linkRequest,
  pageId,
  sendFrame,
  type BusOptions,
  type BusSocket,
} from "./link.js";

export { BusFault, pageId };
export type { BusOptions, BusSocket };
export {
  HEARTBEAT_MS,
  MAX_FRAME,
  PONG_DEADLINE_MS,
  PROTOCOL,
  RECONNECT_MAX_MS,
  RECONNECT_MIN_MS,
  RECONNECT_NOTICE_MS,
  REQUEST_TIMEOUT_MS,
} from "./link.js";

export interface BusRoute<Desc extends DescMessage> {
  module: number;
  sub: number;
  id: number;
  schema: Desc;
}

export interface BusTopic {
  module: number;
  sub: number;
  key: string;
}

interface Slot {
  topic: BusTopic;
  n: number;
  ids: Map<number, Array<(frame: Frame) => void>>;
}

const topics = new Map<string, Slot>();
const resync: Array<() => void> = [];

bindBus({
  event: (frame) => deliver(frame),
  relay: (frame) => deliver(frame),
  welcome: () => {
    const held: BusTopic[] = [];
    for (const slot of topics.values()) if (slot.n > 0) held.push(slot.topic);
    if (held.length) sendTopics(TopicId.SUBSCRIBE, held);
    for (const fn of resync) fn();
  },
});

function topicKey(topic: BusTopic): string {
  return topic.module + "\0" + topic.sub + "\0" + topic.key;
}

function deliver(frame: Frame): void {
  const slot = topics.get(topicKey({ module: frame.module, sub: frame.sub, key: frame.key }));
  const list = slot ? slot.ids.get(frame.id) : undefined;
  if (!list) return;
  for (const fn of list) fn(frame);
}

export function installBus(opts: BusOptions = {}): void {
  topics.clear();
  resync.length = 0;
  installLink(opts);
}

export function busRequest<Q extends DescMessage, R extends DescMessage>(
  route: BusRoute<Q>,
  key: string,
  req: MessageShape<Q>,
  reply: R,
): Promise<MessageShape<R>> {
  return linkRequest(route.module, route.sub, route.id, key, toBinary(route.schema, req)).then((frame) => {
    if (frame.fault) {
      const params: Record<string, string> = {};
      for (const [k, v] of Object.entries(frame.fault.params)) if (typeof v === "string") params[k] = v;
      return Promise.reject(new BusFault(frame.fault.code, frame.fault.message, params));
    }
    return fromBinary(reply, frame.body);
  });
}

export function busOn<Desc extends DescMessage>(
  topic: BusTopic,
  id: number,
  body: Desc,
  fn: (msg: MessageShape<Desc>, origin: string) => void,
): () => void {
  const key = topicKey(topic);
  let slot = topics.get(key);
  if (!slot) {
    slot = { topic, n: 0, ids: new Map() };
    topics.set(key, slot);
    if (isWelcomed()) sendTopics(TopicId.SUBSCRIBE, [topic]);
  }
  slot.n += 1;
  const list = slot.ids.get(id) ?? [];
  const mine = (frame: Frame): void => { fn(fromBinary(body, frame.body), frame.origin); };
  list.push(mine);
  slot.ids.set(id, list);
  return () => {
    const current = topics.get(key);
    if (!current) return;
    const found = current.ids.get(id);
    if (found) {
      const at = found.indexOf(mine);
      if (at >= 0) found.splice(at, 1);
    }
    current.n -= 1;
    if (current.n === 0) {
      topics.delete(key);
      if (isWelcomed()) sendTopics(TopicId.UNSUBSCRIBE, [topic]);
    }
  };
}

export function busRelay<Desc extends DescMessage>(route: BusRoute<Desc>, key: string, msg: MessageShape<Desc>): void {
  sendFrame(route.module, route.sub, route.id, Kind.RELAY, key, toBinary(route.schema, msg));
}

export function busOnResync(fn: () => void): () => void {
  resync.push(fn);
  return () => {
    const at = resync.indexOf(fn);
    if (at >= 0) resync.splice(at, 1);
  };
}

function sendTopics(id: number, list: BusTopic[]): void {
  const body = toBinary(TopicsSchema, create(TopicsSchema, {
    topics: list.map((topic) => ({ module: topic.module, sub: topic.sub, key: topic.key })),
  }));
  sendFrame(0, 2, id, Kind.REQUEST, "", body);
}
