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

//! An empty HOST notice (SPEC §bus.topics). The composition calls this from the
//! hooks it installs; the body stays empty because the page re-reads the route.

use prost::Message as _;
use swiss_bus::pb::v1;

/// Publish one HOST notice. `sub` and `id` are the generated enum values.
pub fn publish(bus: &swiss_bus::Bus, sub: u32, id: u32) {
    publish_at(bus, v1::Module::Host as u32, sub, id);
}

/// Publish one empty notice on any module. The page re-reads the owner's route.
pub fn publish_at(bus: &swiss_bus::Bus, module: u32, sub: u32, id: u32) {
    let body = v1::Notice {}.encode_to_vec();
    bus.publish(module, sub, id, "", "", body);
}
