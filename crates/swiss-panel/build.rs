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

// Release builds embed the panel gzip-compressed (SPEC §panel, Serving): every file under
// src/admin_assets is gzipped, one for one and under its own name, into
// $OUT_DIR/admin_assets_gz, which admin.rs embeds in place of the raw tree and sends as
// `Content-Encoding: gzip`. The tree is ~7 MB, three quarters of it vendored JS (mermaid alone
// 2.8 MB); gzipped it is ~2 MB, so ~5 MB stays out of the executable and off the wire.
//
// Debug builds skip it: rust-embed reads the raw tree from disk on each request there (the panel
// dev loop, SPEC §panel.toolchain), so there is nothing to compress and the tree is not a rerun
// trigger - a debug `cargo build` after `npm run build` does not rebuild this crate.
//
// The gzip header carries no mtime and no file name, so one tree always compresses to the same
// bytes: the binary stays reproducible and an unchanged file keeps its ETag across builds.

use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::Path;

use flate2::write::GzEncoder;
use flate2::Compression;

const TREE: &str = "src/admin_assets";

fn main() -> Result<(), Box<dyn Error>> {
    if std::env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_some() {
        println!("cargo:rerun-if-changed=build.rs");
        return Ok(());
    }
    // The directory is scanned whole, which catches a file added or edited anywhere under it
    // (its mtime is newer than the last run); compress_tree names every file as well, because
    // a deletion leaves nothing newer behind and only a named path gone missing reruns this.
    // A rerun rebuilds the crate, so rust-embed re-reads the file list too.
    println!("cargo:rerun-if-changed={TREE}");
    let out = Path::new(&std::env::var("OUT_DIR")?).join("admin_assets_gz");
    if out.exists() {
        // A file deleted from the tree must not live on in the embed.
        fs::remove_dir_all(&out)?;
    }
    compress_tree(Path::new(TREE), &out)
}

fn compress_tree(from: &Path, to: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            compress_tree(&entry.path(), &target)?;
        } else {
            println!("cargo:rerun-if-changed={}", entry.path().display());
            let mut gz = GzEncoder::new(Vec::new(), Compression::best());
            gz.write_all(&fs::read(entry.path())?)?;
            fs::write(&target, gz.finish()?)?;
        }
    }
    Ok(())
}
