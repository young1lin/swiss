/*
 * Copyright 2026 The swiss authors
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

//! The sealed state format. `envelope.rs` is the frozen on-disk format every state file is
//! written in (docs/05 §1); `key.rs` resolves the master key from the OS credential store;
//! `statefile.rs` reads/writes sealed state; `envstore.rs` is the encrypted .env replacement;
//! `secretstore.rs` is the secret vault (docs/19), and `refs.rs` is the one resolver every
//! credential string passes on its way to a live use.

pub mod envelope;
pub mod envstore;
pub mod key;
pub mod refs;
pub mod secretstore;
pub mod statefile;
