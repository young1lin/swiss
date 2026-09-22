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

//! The exit story for the containers engine.rs starts (docs/44 §2.2, adapted to what
//! testcontainers-rs 0.28 actually ships - it has NO ryuk reaper, unlike its Java
//! sibling; cleanup there is `ContainerAsync`'s Drop, which needs a live tokio runtime
//! and therefore never runs for a value held in a static).
//!
//! This module owns that hole: every started container id is registered together with
//! the docker endpoint it lives behind, and an atexit hook force-removes them all with a
//! raw, blocking HTTP DELETE over the endpoint's own transport - TCP, unix socket or
//! Windows named pipe; no runtime exists at exit, so nothing async may be used here.
//! Best-effort by design: a hard-killed process still leaks, which is what the startup
//! prune in engine.rs is for.

use std::io::{Read, Write};
use std::sync::{Mutex, Once};
use std::time::Duration;

/// One registered container: the docker endpoint to talk to and the id to remove.
struct Lease {
    endpoint: String,
    id: String,
}

static LEASES: Mutex<Vec<Lease>> = Mutex::new(Vec::new());
static HOOK: Once = Once::new();

/// Record a started container and arm the exit hook. The first caller arms it; later
/// callers only append. Idempotent per process by construction.
pub(crate) fn arm(endpoint: &str, id: &str) {
    LEASES
        .lock()
        .expect("the swiss-it lease list")
        .push(Lease {
            endpoint: endpoint.to_string(),
            id: id.to_string(),
        });
    HOOK.call_once(|| unsafe {
        // The CRT's atexit on both platforms this suite runs on; the hook must be a
        // plain extern "C" function and nothing may unwind across it.
        if atexit(fire) != 0 {
            eprintln!("swiss-it: could not arm the container exit hook");
        }
    });
}

extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

/// extern "C" shim: unwinding across atexit is undefined, and the tests have already
/// reported their verdict by the time this runs - swallow everything.
extern "C" fn fire() {
    let _ = std::panic::catch_unwind(remove_all);
}

/// Force-remove every registered container, one blocking call each.
fn remove_all() {
    let leases = std::mem::take(&mut *LEASES.lock().expect("the swiss-it lease list"));
    for lease in leases {
        match raw_delete(&lease.endpoint, &lease.id) {
            Ok(()) => eprintln!(
                "swiss-it: removed container {} at exit",
                &lease.id[..lease.id.len().min(12)]
            ),
            Err(e) => eprintln!(
                "swiss-it: could not remove container {} at exit: {e} (endpoint {}); \
                 remove it by id once, the next run prunes it anyway",
                &lease.id[..lease.id.len().min(12)],
                lease.endpoint
            ),
        }
    }
}

/// The exact bytes sent for one removal. Pure, so the wire format has a test.
///
/// Deliberately UNVERSIONED: a versioned /v1.xx path pins an API era, and daemons keep
/// raising their minimum (docker 28 refuses v1.41 with a bare 400). Without a prefix
/// dockerd serves its own current version - exactly what a best-effort exit hook wants.
fn delete_request(id: &str) -> String {
    format!(
        "DELETE /containers/{id}?force=1&v=1 HTTP/1.1\r\nHost: docker\r\n\
         Connection: close\r\nContent-Length: 0\r\n\r\n"
    )
}

/// Accept 2xx, and 404 (already gone - a crashed run may have half-cleaned).
fn status_is_fine(head: &[u8]) -> Result<(), String> {
    let first = head.split(|&b| b == b'\n').next().unwrap_or(&[]);
    let text = String::from_utf8_lossy(first).trim().to_string();
    let code = text
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .parse::<u16>()
        .unwrap_or(0);
    if (200..300).contains(&code) || code == 404 {
        Ok(())
    } else {
        Err(text)
    }
}

/// Read and Write over one transport, so the same speak() serves TCP, unix and pipe.
trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

/// One blocking `DELETE /containers/{id}?force=1&v=1`, hand-rolled over whatever
/// transport the endpoint names. Loopback docker endpoints speak plain HTTP/1.1; an
/// https:// endpoint would need a TLS stack at exit and is refused with the id in the
/// error so the leftover is findable.
fn raw_delete(endpoint: &str, id: &str) -> Result<(), String> {
    let request = delete_request(id);
    let speak = |io: &mut dyn ReadWrite| -> Result<(), String> {
        io.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
        // Reading to EOF: Connection: close makes dockerd hang up after the reply.
        let mut all = Vec::new();
        io.read_to_end(&mut all).map_err(|e| e.to_string())?;
        status_is_fine(&all)
    };
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return Err(format!("unparsable docker endpoint: {endpoint}"));
    };
    match scheme {
        "tcp" | "http" => {
            let mut s =
                std::net::TcpStream::connect(rest).map_err(|e| format!("connect {rest}: {e}"))?;
            // Never wedge the exit of an already-green suite on a hung dockerd.
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
            speak(&mut s)
        }
        "unix" => {
            #[cfg(unix)]
            {
                use std::os::unix::net::UnixStream;
                let mut s = UnixStream::connect(rest).map_err(|e| e.to_string())?;
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
                speak(&mut s)
            }
            #[cfg(not(unix))]
            {
                Err(format!("unix-socket endpoint on a non-unix host: {endpoint}"))
            }
        }
        "npipe" => {
            #[cfg(windows)]
            {
                // npipe:////./pipe/docker_engine -> \\.\pipe\docker_engine
                let path = rest.replace('/', "\\");
                let mut f = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .map_err(|e| format!("open {path}: {e}"))?;
                speak(&mut f)
            }
            #[cfg(not(windows))]
            {
                Err(format!("named-pipe endpoint on a non-Windows host: {endpoint}"))
            }
        }
        other => Err(format!(
            "cannot clean up over {other}:// at exit (no TLS here); container {id} \
             must be removed by id",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_delete_request_is_well_formed_http() {
        let r = delete_request("abc123");
        assert!(
            r.starts_with("DELETE /containers/abc123?force=1&v=1 HTTP/1.1\r\n"),
            "{r}"
        );
        assert!(r.contains("Host: docker\r\n"), "{r}");
        assert!(r.contains("Connection: close"), "{r}");
        assert!(r.ends_with("\r\n\r\n"), "{r}");
    }

    #[test]
    fn removal_accepts_success_and_already_gone() {
        assert!(status_is_fine(b"HTTP/1.1 204 No Content\r\n").is_ok());
        assert!(status_is_fine(b"HTTP/1.1 404 Not Found\r\n").is_ok());
        assert!(status_is_fine(b"HTTP/1.1 500 boom\r\n").is_err());
        assert!(status_is_fine(b"not http at all").is_err());
    }
}
