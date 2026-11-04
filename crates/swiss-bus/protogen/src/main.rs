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

//! Writes `crates/swiss-bus/src/pb/swiss.bus.v1.rs` from the `.proto` sources
//! (SPEC §bus.codegen). Run with:
//! `cargo run -j 2 --manifest-path crates/swiss-bus/protogen/Cargo.toml`
//!
//! The generated file's first line is a SHA-256 over the sources. A swiss-bus unit
//! test recomputes it, so a hand edit or a stale file fails with this command.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const REGENERATE: &str = "cargo run -j 2 --manifest-path crates/swiss-bus/protogen/Cargo.toml";

fn main() {
    if let Err(err) = run() {
        eprintln!("swiss-bus-protogen: {err}\nregenerate with: {REGENERATE}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let crate_dir = manifest_dir
        .parent()
        .ok_or("protogen has no parent directory")?
        .to_path_buf();
    let proto_dir = crate_dir.join("proto");
    let out_dir = crate_dir.join("src").join("pb");
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;

    let mut sources = Vec::new();
    collect(&proto_dir, &proto_dir, &mut sources)?;
    sources.sort();
    if sources.is_empty() {
        return Err("no .proto sources under crates/swiss-bus/proto".into());
    }
    let digest = digest_of(&proto_dir, &sources)?;

    let files: Vec<PathBuf> = sources.iter().map(|rel| proto_dir.join(rel)).collect();
    let descriptors = protox::compile(&files, [&proto_dir]).map_err(|e| e.to_string())?;
    let mut config = prost_build::Config::new();
    // Map fields encode in key order, so a golden frame re-encodes byte for byte.
    config.btree_map(["."]);
    config.out_dir(&out_dir);
    config.compile_fds(descriptors).map_err(|e| e.to_string())?;

    let generated = out_dir.join("swiss.bus.v1.rs");
    let body = fs::read_to_string(&generated).map_err(|e| e.to_string())?;
    let stamped = format!("// proto-sha256: {digest}\n{body}");
    fs::write(&generated, stamped.replace("\r\n", "\n")).map_err(|e| e.to_string())?;
    println!("wrote {} ({digest})", generated.display());
    Ok(())
}

/// `.proto` files as paths relative to `proto/`, with forward slashes.
fn collect(proto_dir: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            collect(proto_dir, &path, out)?;
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "proto") {
            let rel = path.strip_prefix(proto_dir).map_err(|e| e.to_string())?;
            let text = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push(text);
        }
    }
    Ok(())
}

/// SHA-256 over the sorted relative paths: each path, a NUL, its bytes, a NUL
/// (SPEC §bus.codegen).
fn digest_of(proto_dir: &Path, sources: &[String]) -> Result<String, String> {
    let mut hasher = Sha256::new();
    for rel in sources {
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        let bytes = fs::read(proto_dir.join(rel)).map_err(|e| format!("{rel}: {e}"))?;
        hasher.update(&bytes);
        hasher.update([0]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
