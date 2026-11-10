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

//! HOST.TOKENS (SPEC §host.tokens): create, rotate and remove each publish once.
//! A rotate or remove of an unknown id publishes nothing.

use std::sync::{Arc, Mutex};

use swiss_host::token::{single_token_manager, TokenManager};

fn counted() -> (TokenManager, Arc<Mutex<Vec<&'static str>>>) {
    let notes = Arc::new(Mutex::new(Vec::new()));
    let manager = single_token_manager("seed-secret");
    let sink = notes.clone();
    manager.set_notice(Some(Arc::new(move || {
        sink.lock().expect("notes").push("tokens");
    })));
    (manager, notes)
}

// SPEC §host.tokens: create, rotate and remove each publish once.
#[test]
fn create_rotate_and_remove_each_publish_once() {
    let (manager, notes) = counted();
    let created = manager.create("claude");
    assert_eq!(notes.lock().expect("notes").len(), 1);
    let rotated = manager.rotate(&created.id).expect("the token exists");
    assert_ne!(rotated.secret, created.secret);
    assert_eq!(notes.lock().expect("notes").len(), 2);
    assert!(manager.remove(&created.id));
    assert_eq!(notes.lock().expect("notes").len(), 3);
}

// SPEC §host.tokens: a refused write publishes nothing.
#[test]
fn a_refused_rotate_or_remove_publishes_nothing() {
    let (manager, notes) = counted();
    assert!(manager.rotate("missing").is_none());
    assert!(!manager.remove("missing"));
    assert!(notes.lock().expect("notes").is_empty());
}
