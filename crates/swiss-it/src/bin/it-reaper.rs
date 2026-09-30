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

//! The cleanup watchdog for the swiss-it containers - this repo's ryuk, minus the
//! container (SPEC §testing.it). Cleanup must not depend on the cleaned-up process
//! running any code: a FAILED libtest run on Windows exits through ExitProcess,
//! which skips the CRT atexit hook the green path uses, and a killed or crashed
//! process runs nothing at all.
//!
//! Protocol, one line each over stdin:
//!
//! \verbatim
//! <docker endpoint>
//! <container id>
//! <container id>
//! \endverbatim
//!
//! EOF on stdin is the parent's death - every exit path closes the pipe - and each
//! recorded id then gets one blocking force-DELETE over the raw socket code the
//! atexit hook shares (docker_raw), so the two cleaners cannot drift apart.
//! Duplicate DELETEs answer 404 and are fine; the parent deletes first when it can.
//!
//! The parent never closes its end while it lives, so this process just blocks on
//! lines and costs nothing while the suite runs.

use std::io::{BufRead, BufReader};

fn main() {
    let mut lines = BufReader::new(std::io::stdin()).lines();
    let Some(Ok(endpoint)) = lines.next() else {
        eprintln!("it-reaper: no docker endpoint on stdin; nothing to do");
        return;
    };
    let mut ids = Vec::new();
    for line in lines {
        match line {
            Ok(id) if !id.trim().is_empty() => ids.push(id),
            Ok(_) => {}
            Err(e) => {
                eprintln!("it-reaper: broken stdin mid-list ({e}); removing what arrived");
                break;
            }
        }
    }
    // EOF: the parent is gone, whatever way it went. Remove everything it recorded.
    eprintln!(
        "it-reaper: parent gone (endpoint {endpoint}); removing {} container(s)",
        ids.len()
    );
    for id in &ids {
        match swiss_it::docker_raw::raw_delete(&endpoint, id) {
            Ok(()) => eprintln!("it-reaper: removed container {}", &id[..id.len().min(12)]),
            Err(e) => eprintln!(
                "it-reaper: could not remove container {}: {e}; \
                 remove it by id once, the next run prunes it anyway",
                &id[..id.len().min(12)]
            ),
        }
    }
}
