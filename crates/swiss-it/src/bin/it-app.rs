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

//! The managed-app stand-in for the detached group (SPEC §process.verify): a real
//! process with roles the tests can assert on, each a plain Rust function, never a
//! shell. Built only behind the `it` feature like the rest of this crate, so the
//! shipping graph never sees it.
//!
//! - `serve <port>`: bind 127.0.0.1:<port>, answer every connection with a fixed
//!   `HTTP/1.1 200 OK`, and print one line a second (the liveness a capture file
//!   can be read for).
//! - `launcher <port>`: spawn `serve <port>` through `detach_command` and exit 0 —
//!   the "launcher that exits" shape a real app start has.
//! - `flood`: write 1 MiB to stdout, then sleep 60 s — the pipeless capture must
//!   never block a writer nobody reads.
//!
//! The binary is std-only on purpose: the roles are blocking by nature, and pulling
//! a runtime into a helper would be weight the tests pay for nothing.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let role = args.get(1).map(String::as_str).unwrap_or("");
    match role {
        "serve" => {
            let port: u16 = args
                .get(2)
                .and_then(|p| p.parse().ok())
                .unwrap_or_else(|| usage("serve needs a port"));
            serve(port);
        }
        "launcher" => {
            let port: u16 = args
                .get(2)
                .and_then(|p| p.parse().ok())
                .unwrap_or_else(|| usage("launcher needs a port"));
            launcher(port);
        }
        "flood" => flood(),
        _ => usage("unknown role"),
    }
}

fn usage(why: &str) -> ! {
    eprintln!("it-app: {why}; roles: serve <port> | launcher <port> | flood");
    std::process::exit(2);
}

/// The fixed bytes every connection gets: enough HTTP for a client to call it 200,
/// and a body the tests can match byte for byte.
const OK_RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

fn serve(port: u16) {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("it-app serve: could not bind 127.0.0.1:{port}: {err}");
            std::process::exit(1);
        }
    };
    println!("it-app serve: listening on 127.0.0.1:{port} as pid {}", std::process::id());
    // One line a second: the heartbeat a capture file is polled for, and proof the
    // process was never blocked by its own output.
    std::thread::spawn(|| {
        let mut n = 0u64;
        loop {
            println!("it-app serve: alive {n}");
            n += 1;
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        // Drain a little of the request so a client that waits for its own write to
        // finish is not raced, then answer and close.
        let mut scratch = [0u8; 512];
        let _ = stream.read(&mut scratch);
        let _ = stream.write_all(OK_RESPONSE);
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
}

fn launcher(port: u16) {
    // The same detaching seam the gateway's app start uses (SPEC §process.detached):
    // the flags live in swiss-core, never re-spelled here. Stdio is inherited, so
    // serve's lines land in the same capture file the launcher itself was given;
    // stdin is null — nobody will ever read it.
    let exe = std::env::current_exe().unwrap_or_else(|err| {
        eprintln!("it-app launcher: could not find my own exe: {err}");
        std::process::exit(1);
    });
    let mut command = Command::new(&exe);
    command.arg("serve").arg(port.to_string());
    command.stdin(std::process::Stdio::null());
    swiss_core::platform::detach_command(&mut command);
    // The spawn rules of the app start itself (SPEC §process.detached): when the job
    // forbids the escape (os error 5), spawn again WITHOUT the breakaway flag - the
    // child then shares this job and dies with it, which the real start reports as
    // "shares the gateway's job". A test harness that runs inside such a job gets the
    // second path; one that does not, gets the first.
    let spawned = match command.spawn() {
        Ok(child) => Ok(child),
        Err(err) if cfg!(windows) && err.raw_os_error() == Some(5) => {
            eprintln!("it-app launcher: the job forbids breakaway, serving inside it");
            let mut shared = Command::new(&exe);
            shared.arg("serve").arg(port.to_string());
            shared.stdin(std::process::Stdio::null());
            swiss_core::platform::detach_command_without_breakaway(&mut shared);
            shared.spawn()
        }
        Err(err) => Err(err),
    };
    match spawned {
        Ok(child) => {
            println!(
                "it-app launcher: serve {port} handed off to pid {}",
                child.id()
            );
            // Exit 0 on purpose: the launcher's whole job is to be gone, leaving a
            // listening child it no longer parents.
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("it-app launcher: could not start serve: {err}");
            std::process::exit(1);
        }
    }
}

fn flood() {
    // 1 MiB in 64 KiB chunks with a newline in each, so a line-buffered stdout
    // flushes as it goes and the capture file grows while the process lives.
    let chunk = "0123456789abcdef".repeat(64) + "\n"; // 64 * 16 + 1 = 1025 bytes
    let mut written = 0usize;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    while written < 1024 * 1024 {
        match out.write_all(chunk.as_bytes()).and_then(|()| out.flush()) {
            Ok(()) => written += chunk.len(),
            Err(err) => {
                eprintln!("it-app flood: write failed after {written} bytes: {err}");
                std::process::exit(1);
            }
        }
    }
    println!("it-app flood: wrote {written} bytes");
    // Then outlive the write: a pipe would have blocked (or died of EPIPE) long
    // before a megabyte; a file must let the process sleep on.
    std::thread::sleep(Duration::from_secs(60));
}
