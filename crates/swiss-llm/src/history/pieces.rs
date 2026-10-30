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

//! The content-addressed pieces a chat body and a results tail split into: each filed once
//! by hash, the envelope naming them, and the body rebuilt from them at read.

use super::*;

/// The keys of `request` that name one whole piece by hash in a v2 body: the wire
/// facts (endpoint, headers, options) folded as one `wire` piece, the system prompt,
/// the tools array, and every message of the dialogue on its own - the resent
/// conversation is the quadratic bulk, so the message is the dedup workhorse.
pub(super) const PIECE_KEYS: [&str; 3] = ["system", "tools", "messages"];

/// Split a results tail into its sealed reference and pieces (SPEC
/// llm.history): every tool entry is content-addressed on its own through the
/// same machinery call bodies use, and what the run column carries is the
/// REFERENCE - a JSON list of hashes, never plaintext. The decode path
/// ([LlmHistory::decode_tail]) rebuilds the exact filed value.
pub(super) fn split_tail(tail: &Value) -> (Value, Vec<Piece>) {
    let mut pieces = Vec::new();
    let hashes: Vec<Value> = tail
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .map(|tool| {
                    let text = tool.to_string();
                    let hash = piece_hash(&text);
                    pieces.push(Piece { hash: hash.clone(), text });
                    Value::String(hash)
                })
                .collect()
        })
        .unwrap_or_default();
    (json!({ "tools": hashes }), pieces)
}

/// The sha256 of a piece's exact serialized JSON, as lowercase hex - the piece's
/// address. Same digest the embedding fingerprint already uses this crate for.
pub(super) fn piece_hash(text: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// One piece this body refers to: its address and its exact serialized JSON.
#[derive(Clone)]
pub(super) struct Piece {
    pub(super) hash: String,
    pub(super) text: String,
}

/// Split a whole chat body into a v2 envelope plus the pieces it refers to. The
/// envelope is what the call row stores; the pieces are what the piece table keeps
/// once. The wire facts (endpoint/headers/options) fold into one `wire` piece; the
/// system prompt, the tools array and every message become pieces on their own; the
/// prompt, the turn and the whole response stay inline (the response is unique per
/// round trip, so a piece would buy nothing but a row). Bodies without any pieceable
/// key (an embedding, a timeout, a bare ask) come back unchanged with no pieces -
/// plain v1, the reader's other branch.
pub(super) fn split_body(body: &Value) -> (Value, Vec<Piece>) {
    let mut envelope = body.clone();
    let mut pieces = Vec::new();
    let Some(request) = envelope.get_mut("request").and_then(|r| r.as_object_mut()) else {
        return (envelope, pieces);
    };
    // The wire facts, as one piece: stable for a provider's life, resent every ask.
    let wire_keys: Vec<String> = ["endpoint", "headers", "options"]
        .iter()
        .filter(|k| request.contains_key(**k))
        .map(|k| (*k).to_string())
        .collect();
    if !wire_keys.is_empty() {
        let wire: Value = wire_keys
            .iter()
            .map(|k| (k.clone(), request[k.as_str()].clone()))
            .collect::<serde_json::Map<String, Value>>()
            .into();
        let text = wire.to_string();
        let hash = piece_hash(&text);
        for k in &wire_keys {
            request.remove(k.as_str());
        }
        request.insert("wire".into(), Value::String(hash.clone()));
        pieces.push(Piece { hash, text });
    }
    for key in PIECE_KEYS {
        let Some(value) = request.get_mut(key) else { continue };
        match value {
            // One message, one piece: the resent dialogue is the bulk.
            Value::Array(messages) if key == "messages" => {
                let count = messages.len();
                let mut hashes = Vec::with_capacity(count);
                for message in messages.iter_mut() {
                    let text = message.to_string();
                    let hash = piece_hash(&text);
                    hashes.push(Value::String(hash.clone()));
                    pieces.push(Piece { hash, text });
                }
                *value = Value::Array(hashes);
            }
            // A scalar piece (system, tools): its hash replaces the value.
            value if !value.is_null() => {
                let text = value.to_string();
                let hash = piece_hash(&text);
                *value = Value::String(hash.clone());
                pieces.push(Piece { hash, text });
            }
            _ => {}
        }
    }
    if pieces.is_empty() {
        return (body.clone(), pieces);
    }
    if let Some(obj) = envelope.as_object_mut() {
        obj.insert("v".into(), json!(2));
    }
    (envelope, pieces)
}

/// Rebuild the whole v1 body a v2 envelope refers to: every hash string walks back to
/// the piece it addresses. `piece_text` opens one piece's chunk chain. A piece that
/// will not open answers None - the caller says gone rather than short.
pub(super) fn resolve_body(envelope: &Value, piece_text: &dyn Fn(&str) -> Option<String>) -> Option<Value> {
    let mut whole = envelope.clone();
    {
        let request = whole.get_mut("request")?.as_object_mut()?;
        // The wire piece unfolds back into its keys.
        if let Some(hash) = request.remove("wire").and_then(|v| v.as_str().map(str::to_string)) {
            let wire: Value = serde_json::from_str(&piece_text(&hash)?).ok()?;
            for (k, v) in wire.as_object()? {
                request.insert(k.clone(), v.clone());
            }
        }
        for key in PIECE_KEYS {
            // A body may carry none of these (a bare ask): absent means absent, not
            // gone - the take() below only rewrites what is there.
            let Some(slot) = request.get_mut(key) else { continue };
            match slot {
                Value::Array(hashes) if key == "messages" => {
                    let owned = hashes.clone();
                    let mut messages = Vec::with_capacity(owned.len());
                    for hash in owned.iter() {
                        let text = piece_text(hash.as_str()?)?;
                        messages.push(serde_json::from_str(&text).ok()?);
                    }
                    *slot = Value::Array(messages);
                }
                Value::String(hash) => {
                    let text = piece_text(hash.as_str())?;
                    *slot = serde_json::from_str(&text).ok()?;
                }
                _ => {}
            }
        }
    }
    if let Some(obj) = whole.as_object_mut() {
        obj.remove("v");
    }
    Some(whole)
}

/// Every piece hash a v2 envelope names, in body order. The collect is what the
/// read path opens before resolving; the resolve itself walks the same slots.
pub(super) fn piece_hashes(envelope: &Value) -> Vec<String> {
    let mut hashes = Vec::new();
    let Some(request) = envelope.get("request") else { return hashes };
    if let Some(hash) = request.get("wire").and_then(Value::as_str) {
        hashes.push(hash.to_string());
    }
    for key in PIECE_KEYS {
        match request.get(key) {
            Some(Value::Array(items)) if key == "messages" => {
                for hash in items.iter().filter_map(Value::as_str) {
                    hashes.push(hash.to_string());
                }
            }
            Some(Value::String(hash)) => hashes.push(hash.to_string()),
            _ => {}
        }
    }
    hashes
}
