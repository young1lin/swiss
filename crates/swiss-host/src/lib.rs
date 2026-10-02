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

//! The host layer: the plugin host's mechanism, the run/action services, the connection
//! catalog, the browser model, config + config-store, auth and token management. Knows no
//! subsystem by type — that is the whole point of the contracts.

pub mod auth;
pub mod config;
pub mod config_store;
pub mod dbbrowser;
pub mod groups;
pub mod host;
pub mod local_only;
pub mod managed;
pub mod mask;
pub mod mem;
pub mod mongobrowser;
pub mod pathenv;
pub mod proc_pids;
pub mod reply;
pub mod secret_groups;
pub mod services;
pub mod token;
