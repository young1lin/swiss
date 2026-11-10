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

//! The event bus (SPEC §bus). One WebSocket per page, one protobuf `Frame` per message.
//! The codecs in [`pb`] are generated; this crate does not name a subsystem.

mod codec;
mod hub;
mod link;
mod notifier;
mod route;

pub mod pb;

pub use hub::{Answer, Bus, Call, Handler, Registration, Stamp};
pub use notifier::Notifier;
pub use link::{run, Io, IoMsg};
pub use route::accept;

#[cfg(test)]
mod tests;

/// The only protocol this build speaks (SPEC §bus.link).
pub const PROTOCOL: u32 = 1;

/// Frame size, either direction (SPEC §bus.wire).
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

/// Links per gateway. The newest past this is refused with BYE `LIMIT`.
pub const MAX_LINKS: usize = 32;

/// Topics one link may hold. A SUBSCRIBE that would pass it is refused whole.
pub const MAX_TOPICS: usize = 512;

/// Outbound queue depth. Past either cap the link ends with BYE `SLOW`.
pub const MAX_QUEUE_FRAMES: usize = 256;
pub const MAX_QUEUE_BYTES: usize = 8 * 1024 * 1024;

/// How long a new link may wait for HELLO.
pub const HELLO_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Carried in WELCOME. The page pings when this long passes with nothing from the server.
pub const HEARTBEAT_MS: u64 = 20_000;

/// How long the page waits for PONG.
pub const PONG_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// WebSocket protocol ping interval. Any inbound frame or pong resets [`IDLE_LIMIT`].
pub const PROTOCOL_PING: std::time::Duration = std::time::Duration::from_secs(30);

/// Silence after which the server ends the link with BYE `IDLE`.
pub const IDLE_LIMIT: std::time::Duration = std::time::Duration::from_secs(75);

/// How long the page waits for a RESPONSE. Raised locally; the server does not time a handler out.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Reconnect backoff, reset only by WELCOME (SPEC §bus.link). The page owns the wait.
pub const RECONNECT_MIN: std::time::Duration = std::time::Duration::from_millis(500);
pub const RECONNECT_MAX: std::time::Duration = std::time::Duration::from_secs(30);

/// How long the page is down before the chrome draws the reconnecting line.
pub const RECONNECT_NOTICE: std::time::Duration = std::time::Duration::from_secs(3);

/// A debug build re-reads the panel stamp on this interval and publishes BUILD when it changes.
pub const STAMP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(6);

/// At most one throttled notice per topic and key per this long (SPEC §bus.topics).
/// The first poke in a quiet gap publishes at once; the rest of the gap fold into one
/// trailing publish at its end.
pub const NOTICE_GAP: std::time::Duration = std::time::Duration::from_millis(250);

/// One LIST page stays under this, which is inside the frame limit (SPEC §bus.requests).
pub const LIST_PAGE_MAX: usize = 2 * 1024 * 1024;

pub const UNAVAILABLE: &str = "bus.unavailable";
pub const UNKNOWN: &str = "bus.unknown";
pub const BAD_BODY: &str = "bus.badBody";
pub const NOT_RELAYABLE: &str = "bus.notRelayable";
pub const LIMIT: &str = "bus.limit";

/// The command a stale generated codec must print (SPEC §bus.codegen). `-j 2` keeps the
/// generator off the rest of the machine.
pub const REGENERATE: &str = "cargo run -j 2 --manifest-path crates/swiss-bus/protogen/Cargo.toml";

/// PANEL.PREFS is the only relay in this delivery (SPEC §bus.registry).
pub(crate) fn relayable(module: u32, sub: u32) -> bool {
    module == pb::v1::Module::Panel as u32 && sub == pb::v1::PanelSub::Prefs as u32
}
