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

//! The cleanup acceptance tests (docs/44 SS2.2): the two ways the harness promised
//! containers never outlive their run. Both drive a REAL second test process - the
//! same binary, re-execed with --exact - because both hazards are about process
//! boundaries, not in-process behavior:
//!
//! - a parallel run's startup must not delete a live engine (the old pid-based
//!   prune did exactly that);
//! - a RED run - whose exit path on Windows skips the CRT atexit hook - must still
//!   remove its containers, which is the it-reaper watchdog's whole job.

use std::time::Duration;

use swiss_it::engine::{engine, Kind};

/// The docker client this harness already uses, for the label-filtered listing.
async fn owned_by(pid: u32) -> Vec<String> {
    let docker = testcontainers::core::client::docker_client_instance()
        .await
        .expect("docker client");
    let list = testcontainers::bollard::query_parameters::ListContainersOptionsBuilder::new()
        .all(true)
        .filters(&std::collections::HashMap::from([(
            "label".to_string(),
            vec![format!("org.swiss-it.pid={pid}")],
        )]))
        .build();
    let seen = docker.list_containers(Some(list)).await.expect("list");
    seen.into_iter()
        .filter_map(|c| c.id)
        .collect()
}

/// Run one test of THIS binary in a child process; every env (DOCKER_HOST included)
/// is inherited. Returns the child's pid and exit code.
async fn run_child(extra_env: Option<(&str, &str)>) -> (u32, Option<i32>) {
    let mut cmd = std::process::Command::new(std::env::current_exe().expect("this binary"));
    cmd.args(["--exact", "smoke::redis_answers_ping", "--test-threads=1"]);
    if let Some((k, v)) = extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn the child test run");
    let pid = child.id();
    let status = tokio::task::spawn_blocking(move || child.wait())
        .await
        .expect("join the waiter")
        .expect("child runs to its own exit");
    (pid, status.code())
}

#[tokio::test]
async fn two_processes_keep_each_others_engines() {
    // The parent holds a live redis engine first - the situation a parallel run
    // walks into (two worktrees, two CI jobs, or the owner and the agent).
    let e = engine(Kind::Redis).await;
    let client = redis::Client::open(e.root_url.as_str()).expect("the redis URL parses");
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .expect("the parent engine connects");

    // A second process starts its own engine - its startup prune runs while the
    // parent's container is live. With the old pid-based prune this deleted the
    // parent's container here.
    let (child_pid, code) = run_child(None).await;
    assert_eq!(code, Some(0), "the child run must be green (pid {child_pid})");

    // The parent's engine survived the child's whole lifecycle untouched.
    let pong: String = redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .expect("PING still answers after the child ran");
    assert_eq!(pong, "PONG");
}

#[tokio::test]
async fn a_red_run_still_removes_its_containers() {
    // Same child, but it turns red AFTER the engine is up (smoke.rs panics on
    // SWISS_IT_FORCE_RED). libtest then exits through std::process::exit(101) -
    // ExitProcess on Windows, which never runs the CRT atexit hook. Whatever
    // removes the child's container from here on cannot be the child's own code.
    let (child_pid, code) =
        run_child(Some(("SWISS_IT_FORCE_RED", "1"))).await;
    assert_eq!(code, Some(101), "the deliberate red must fail the run (pid {child_pid})");

    // The reaper watchdog owns this: stdin EOF on the dead parent, then a
    // force-DELETE per recorded id. Ten seconds is generous for one container.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let left = owned_by(child_pid).await;
        if left.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "containers of the red run still present after 10 s: {left:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
