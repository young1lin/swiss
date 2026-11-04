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

//! Envelope encode and decode. The hub reads these fields and nothing inside `body`
//! (SPEC §bus.wire), except the bus's own messages, which this module also decodes.

use std::time::{SystemTime, UNIX_EPOCH};

use prost::Message;

use crate::pb::v1::{self, ByeReason, Fault, Frame, Kind};

pub(crate) struct Outgoing {
    pub module: u32,
    pub sub: u32,
    pub id: u32,
    pub kind: i32,
    pub reply_to: u32,
    pub key: String,
    pub body: Vec<u8>,
    pub fault: Option<Fault>,
    pub origin: String,
}

pub(crate) fn encode(seq: u32, out: &Outgoing) -> Vec<u8> {
    Frame {
        module: out.module,
        sub: out.sub,
        id: out.id,
        kind: out.kind,
        seq,
        reply_to: out.reply_to,
        key: out.key.clone(),
        body: out.body.clone(),
        fault: out.fault.clone(),
        origin: out.origin.clone(),
    }
    .encode_to_vec()
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Frame, ()> {
    Frame::decode(bytes).map_err(|_| ())
}

pub(crate) fn kind_of(frame: &Frame) -> Kind {
    Kind::try_from(frame.kind).unwrap_or(Kind::Unspecified)
}

pub(crate) fn fault(code: &str, message: &str) -> Fault {
    Fault { code: code.to_owned(), params: Default::default(), message: message.to_owned() }
}

pub(crate) fn bye_body(reason: ByeReason) -> Vec<u8> {
    v1::Bye { reason: reason as i32 }.encode_to_vec()
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub(crate) fn response(module: u32, sub: u32, id: u32, reply_to: u32, body: Vec<u8>, fault: Option<Fault>) -> Outgoing {
    Outgoing {
        module,
        sub,
        id,
        kind: Kind::Response as i32,
        reply_to,
        key: String::new(),
        body,
        fault,
        origin: String::new(),
    }
}

pub(crate) fn event(module: u32, sub: u32, id: u32, key: &str, origin: &str, body: Vec<u8>) -> Outgoing {
    Outgoing {
        module,
        sub,
        id,
        kind: Kind::Event as i32,
        reply_to: 0,
        key: key.to_owned(),
        body,
        fault: None,
        origin: origin.to_owned(),
    }
}
