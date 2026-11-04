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

//! `GET /api/bus` (SPEC §bus.security). The session gate and the Origin rule are
//! applied by the composition before [`accept`]; this module only upgrades.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use bytes::Bytes;

use crate::link::{self, Io, IoMsg};
use crate::Bus;
use crate::MAX_FRAME;

pub fn accept(ws: WebSocketUpgrade, bus: Arc<Bus>) -> Response {
    ws.max_frame_size(MAX_FRAME).max_message_size(MAX_FRAME).on_upgrade(move |socket| async move {
        link::run(bus, Socket(socket)).await;
    })
}

struct Socket(WebSocket);

impl Io for Socket {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<IoMsg>> + Send + '_>> {
        Box::pin(async move {
            match self.0.recv().await? {
                Ok(Message::Binary(bytes)) => Some(IoMsg::Binary(bytes.to_vec())),
                Ok(Message::Text(_)) => Some(IoMsg::Text),
                Ok(Message::Ping(payload)) => Some(IoMsg::Ping(payload.to_vec())),
                Ok(Message::Pong(_)) => Some(IoMsg::Pong(Vec::new())),
                Ok(Message::Close(_)) => Some(IoMsg::Close),
                Err(_) => None,
            }
        })
    }

    fn send(&mut self, msg: IoMsg) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin(async move {
            let message = match msg {
                IoMsg::Binary(bytes) => Message::Binary(Bytes::from(bytes)),
                IoMsg::Ping(payload) => Message::Ping(Bytes::from(payload)),
                IoMsg::Pong(payload) => Message::Pong(Bytes::from(payload)),
                IoMsg::Close => Message::Close(None),
                IoMsg::Text => return false,
            };
            self.0.send(message).await.is_ok()
        })
    }
}
