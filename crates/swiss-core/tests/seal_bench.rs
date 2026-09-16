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

//! One-off measured answer to "what does sealing cost?" (not part of the suite; run with
//! `cargo test -p swiss-core --release bench_seal -- --ignored --nocapture`). Numbers, not
//! ranges: cipher throughput of the existing envelope on THIS machine, plus the concrete cost
//! of the one design that must never happen - re-sealing a whole growing file per append.

use std::time::Instant;

use swiss_core::secure::envelope::seal;

fn master() -> [u8; 32] {
    // A fixed scratch key: the bench measures the cipher path, not key custody.
    let mut m = [0u8; 32];
    for (i, b) in m.iter_mut().enumerate() {
        *b = i as u8;
    }
    m
}

fn bench(label: &str, size: usize, iters: usize) {
    let m = master();
    let plaintext = "x".repeat(size);
    let t = Instant::now();
    let mut sink = 0usize;
    for _ in 0..iters {
        let s = seal(&m, "bench", &plaintext);
        sink += s.ct.len(); // incompressible "use" of the output
    }
    let secs = t.elapsed().as_secs_f64();
    let total = size * iters;
    println!(
        "{label:<26} {size:>9} B x {iters:>4}: {secs:>7.3} s  -> {:>8.1} MB/s (sink {sink})",
        (total as f64 / 1_048_576.0) / secs
    );
}

/// The append trap, quantified: naive "seal the whole file on every append" is quadratic.
/// Simulates a cast growing by 1 KB events, re-sealing the whole file each time (the only
/// correct whole-file-AEAD semantics), against a chunked stream that seals only the new event.
fn bench_append_trap() {
    let m = master();
    let events = 2000usize;
    let event = "o".repeat(1024);

    let mut file = String::new();
    let t = Instant::now();
    for _ in 0..events {
        file.push_str(&event);
        let _ = seal(&m, "bench-trap", &file); // whole-file re-seal per append
    }
    let whole_secs = t.elapsed().as_secs_f64();

    let t = Instant::now();
    let mut sealed_bytes = 0usize;
    for _ in 0..events {
        let s = seal(&m, "bench-chunk", &event); // seal only the appended chunk
        sealed_bytes += s.ct.len();
    }
    let chunk_secs = t.elapsed().as_secs_f64();

    println!(
        "append {} x 1KB events: whole-file re-seal {:>7.3} s vs chunked {:>7.3} s  -> {:>5.0}x waste (sealed {} B)",
        events, whole_secs, chunk_secs, whole_secs / chunk_secs, sealed_bytes
    );
}

#[test]
#[ignore]
fn bench_seal_throughput_and_append_trap() {
    bench("seal 1KB", 1024, 2000);
    bench("seal 64KB", 65_536, 500);
    bench("seal 1MB", 1_048_576, 100);
    bench("seal 5MB", 5_242_880, 20);
    bench_append_trap();
}
