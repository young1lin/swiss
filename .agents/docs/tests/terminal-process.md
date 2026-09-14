# Terminal and Process Integration Test Plan (functionality point → test matrix)

> Input: the Feature Overviews of the audit sub-documents `.agents/docs/terminal.md` and `.agents/docs/process.md` (the two chapter matrices below cite their line numbers row by row; all 17 + 13 rows are on record); test baseline: root `tests/` (chiefly `terminal_ws.rs`, `plugin_host.rs`, `adminapi.rs`) + per-crate inline `#[cfg(test)]`; umbrella plan: `.agents/docs/tests/README.md`; spec: `docs/08-testing.md`.

Execution discipline (all carried over from existing patterns; new tests copy them):

- **oneshot first**: ordinary HTTP assertions always go through `tower::ServiceExt::oneshot` against the same router, no real port. The sole exception is the WS upgrade (axum's upgrade extractor needs hyper's OnUpgrade state, which oneshot cannot provide — the module header comment in `tests/terminal_ws.rs` spells out this boundary): keep that file's rig, with the same app also served via `axum::serve` on an ephemeral `127.0.0.1:0` port; only the WS handshake and frame flow go over a real socket.
- **No timed sleeps**: time assertions use `#[tokio::test(start_paused = true)]` + `tokio::time::advance()` (mind the paused clock creeping forward 1 s on every idle park; the `an_expired_ticket_is_refused` comment in `terminal_ws.rs` carries the full countermeasure); event waits use the existing bounded poll loops (`wait_detached` / `await_run`: 10–20 ms interval × a capped number of rounds, panicking with context on timeout).
- **MCP_GATEWAY_MASTER_KEY**: keep `terminal_ws.rs`'s `pin_home_and_key()` (a one-shot OnceLock set of `swiss_core::secure::key::MASTER_KEY_ENV`) and `plugin_host.rs`'s `scratch_dir()`, so every test binary pins a deterministic master key and a data directory nailed to a temporary home.
- **Child processes**: the process side uses real short-lived processes, spelled exactly as in the existing tests — short output `cmd /c echo …` / `sh -c 'echo …'`, long-running `cmd /c ping -n 60 127.0.0.1` / `sh -c 'sleep 60'` (`echo_input` / `a_long_run…` in `tests/plugin_host.rs`); on the terminal side the remote end is always FakeShells, and the local end-to-end path uses print-and-exit programs like `whoami`.
- Neither of these two domains touches a database path, so the DB no-credentials-skip iron law does not apply; all remote SSH goes through the Fake provider and never touches the network.

---

## Chapter 1 Web Terminal (swiss-terminal)

### 1.1 Existing Coverage Inventory (test file/module → test name list → corresponding functionality points)

Matrix row numbers #T1..#T17 correspond to section 1.2; audit line numbers refer to `.agents/docs/terminal.md`.

**tests/terminal_ws.rs (15 tests; the WS integration mode proper)**

| Test name | Functionality point |
| --- | --- |
| the_enabled_plugin_lists_targets_and_local_is_off | #T1 (targets shape with local off + provider present) |
| the_disabled_terminal_plugin_answers_the_structured_503 | #T17 |
| cols_out_of_range_is_refused_not_clamped | #T2 (geometry 400, including the 70000 refusal) |
| a_ticket_route_answers_404_for_a_missing_session | #T4 |
| a_good_ticket_upgrades_and_bytes_flow_both_ways | #T5, #T8 (binary both ways, resize frame, garbage control frames ignored, exit final frame) |
| a_ticket_borrowed_from_another_session_is_refused | #T5 (wrong-session ticket 403) |
| a_replayed_ticket_is_refused | #T5 (replay 403) |
| an_expired_ticket_is_refused(paused clock) | #T5 (expired 403) |
| a_stream_without_a_ticket_is_a_400 | #T5 (missing ticket 400 naming the mint route) |
| a_reconnect_within_the_grace_picks_up_the_catch_up | #T4, #T11 (fresh re-minted ticket + catch-up buffer) |
| resize_over_the_redundant_http_channel_reaches_the_session | #T6 (HTTP resize arrives + out-of-range 400) |
| delete_closes_the_session_and_the_listing_empties | #T7 |
| the_session_listing_is_camel_case_and_counts_bytes | #T3 (field shape + bytesOut) |
| a_recording_is_written_when_configured | #T13 (recording path in the open response + file exists) |
| stopping_the_plugin_closes_live_sessions_with_a_visible_reason | #T17, #T9 (Shutdown reason line + error frame) |

**crates/swiss-terminal/src/terminal/session_tests.rs (27 tests; the session machine, fake provider/fake PTY)**: a_session_streams_its_shells_output_to_the_attached_client, keystrokes_and_resizes_reach_the_shell_in_the_order_they_were_sent (#T8/#T10); the_local_shell_is_refused_while_it_is_switched_off, the_total_cap_refuses_the_next_session_and_names_itself, local_sessions_are_uncapped_and_consume_no_remote_budget, the_per_target_cap_binds_before_the_total_one, allowed_targets_narrows_what_may_be_opened (#T2 caps/allowlist/local-toggle semantics); an_absent_provider_is_reported_by_name_not_as_an_empty_host_list, opening_a_remote_target_while_its_provider_is_gone_names_the_plugin (#T1 absence reported by name + #T14 provider stopping); the_local_view_reports_the_program_that_would_actually_run, the_local_view_lists_the_candidates_the_sheet_offers, the_configured_local_shell_is_what_a_session_runs_unless_overridden (#T14 local view/override precedence); an_attach_spends_its_ticket_and_a_reconnect_needs_a_fresh_one, a_ticket_for_a_session_that_has_closed_is_worth_nothing (#T4/#T5); a_client_that_stops_reading_parks_the_shell_and_loses_no_bytes, a_keystroke_gets_through_while_the_output_is_jammed (#T8 backpressure/Ctrl-C gets through); a_dropped_socket_does_not_kill_the_session_and_the_output_catches_up, the_catch_up_buffer_is_bounded_and_admits_what_it_dropped (#T11); an_idle_session_closes_and_says_which_setting_closed_it, output_keeps_a_session_alive_because_a_running_build_is_not_idle, a_session_nobody_ever_attached_to_is_collected_when_the_grace_expires, a_client_that_never_drains_is_warned_and_then_closed (#T12 four timers, paused clock); a_shell_that_exits_ends_the_session_with_its_code, shutdown_closes_every_session_saying_why (#T9/#T17); the_recording_holds_the_output_and_ends_with_the_reason, recording_off_means_no_file_and_a_null_path (#T13); a_second_attachment_joins_rather_than_replaces (#T10 session-layer fan-out).

**tickets.rs inline (5 tests)**: a_ticket_opens_its_own_session_exactly_once, a_ticket_expires_after_ten_seconds, a_ticket_is_bound_to_the_session_it_was_minted_for, expired_tickets_do_not_pile_up, closing_a_session_invalidates_its_outstanding_tickets, the_error_text_says_which_of_the_three_rules_was_broken (#T5 ticket three-state semantics).

**recording.rs inline (4 tests)**: a_recording_is_a_v2_header_and_one_line_per_chunk, a_multi_byte_character_split_across_two_chunks_survives, a_genuinely_invalid_byte_becomes_one_replacement_and_the_rest_survives, the_cap_stops_the_recording_with_a_visible_marker_and_never_rotates, finishing_twice_writes_one_marker (#T13 format details).

**local.rs inline (7 tests, real ConPTY/openpty)**: a_local_shell_produces_bytes_and_ends_with_an_exit, dropping_the_session_kills_the_shell_it_opened, an_unopenable_program_is_a_named_error_not_a_panic, the_shell_environment_says_terminal_whatever_the_gateway_inherited, a_local_pwsh_keeps_its_colours_under_a_no_color_launcher (skips itself when pwsh is absent), the_reported_program_is_the_one_that_would_run, the_candidate_list_is_probed_once_and_is_never_empty (#T14 the full local-source chain).

**config.rs inline (5 tests)**: an_empty_config_is_the_documented_defaults, every_documented_key_is_read, a_mistyped_limit_is_refused_not_defaulted, a_per_target_cap_above_the_total_cap_is_refused, an_idle_timeout_of_zero_means_no_timeout_and_is_allowed (#T16 parse side).

**src/plugins/terminal.rs inline (3 tests)**: the_schema_numbers_match_the_parser_defaults, the_descriptor_claims_exactly_the_terminal_routes (routes/pages/order/sidebar/restart_on_config_change), validate_rejects_what_the_parser_rejects (#T16 schema-parser mirror).

**src/plugins/terminal_api.rs inline (4 tests)**: geometry_is_refused_not_clamped, control_frames_parse_strictly, the_targets_route_lists_local_shells_and_a_resolved_shell (FixedLocal), server_control_objects_are_exact (#T9 server control-frame construction).

**crates/swiss-host/src/services/shell.rs inline (15 tests)**: PtySize boundaries, ShellRegistry registration/duplicate registration/seat withdrawal/absence naming/unknown-vs-unreachable target routing, leases held and released, duplex bytes/exit/resize recorded, full-queue parking loses no bytes, split endpoints, drop tears down immediately, drain reporting and wakeups, registry clean after a hundred lifecycles (#T14/#T15 host contract layer).

**crates/swiss-core/src/platform/pty/ (conpty.rs 14 tests + mod.rs 6 tests)**: quote escaping, PATH/PATHEXT parsing, default_shell precedence, candidate-list dedup and absolute paths, resolve tells the truth, environment-block overlay/removal/double NUL, a real PTY running commands/resize/drop killing the whole tree/kill waking the read end (#T14 platform layer).

**tests/plugin_host.rs generic plugin boundaries (binding on #T16/#T17)**: boot_disabled_plugins_guard_every_route_they_own, every_contributed_page_entry_is_actually_served, route_ownership_is_longest_prefix_at_segment_boundaries, config_api_validates_before_persisting_and_serves_the_schema (generic config PUT shape), config_driven_restart_only_for_plugins_that_declare_it (generic semantics of restart_on_config_change), enabled_plugins_keep_every_api_shape_they_had.

### 1.2 Functionality-Point → Test Matrix

| # | Functionality point (audit-doc line) | Existing tests | Gap | New test name (planned home) | Assertion essentials (method+path+status code+JSON fields/shape) | Special handling |
| --- | --- | --- | --- | --- | --- | --- |
| #T1 | Target list (terminal.md:9) | terminal_ws the_enabled_plugin_lists_targets_and_local_is_off; session_tests an_absent_provider_is_reported_by_name… (names-it variant, session layer); terminal_api inline the_targets_route_lists_local_shells_and_a_resolved_shell | The "empty seat" absent shape at the HTTP layer (the terminal_ws rig always registers FakeShells first, so the absent branch has never been exercised over the route) | targets_without_a_provider_say_who_is_missing (tests/terminal_ws.rs) | GET /api/terminal/targets → 200; local.enabled==false, local.shell non-empty; remote.presence=="absent", remote.targets==[], remote.reason non-empty containing "provides" | rig needs a configurable mode that registers no provider (see 1.4 rig_with); oneshot |
| #T2 | Opening a session (terminal.md:10) | terminal_ws cols_out_of_range…; session_tests the full caps/allowlist/local-toggle set; terminal_api inline geometry | Field-by-field request-body validation (unknown field/target required/shell type) and the two error-mapping branches Refused→409 and Shell→502 have never been exercised at the HTTP layer | an_open_body_is_validated_field_by_field (tests/terminal_ws.rs); opening_an_unknown_target_is_a_409_naming_it (tests/terminal_ws.rs); a_provider_that_fails_to_open_is_a_502 (tests/terminal_ws.rs) | Per case POST /api/terminal/sessions → 400 + message naming the field (unknown field/target: a non-empty string is required/shell: must be a string); unknown target → 409 with the target name in the message; provider open failure → 502 with the message passed through; GET sessions == [] after every rejection | 502 needs a fake whose open always fails (RefusingShells); oneshot |
| #T3 | Session listing (terminal.md:11) | terminal_ws the_session_listing_is_camel_case_and_counts_bytes | Stable ordering by opened and attached==true both unpinned | the_session_listing_orders_by_opened_and_marks_attachment (tests/terminal_ws.rs) | Open sessions A and B, attach A, then GET /api/terminal/sessions: array [A,B] (opened ascending), A.attached==true, B.attached==false, id/target/label each in place | oneshot; counted as soon as attach returns, no poll needed |
| #T4 | Re-minted reconnect ticket (terminal.md:12) | terminal_ws a_reconnect_within_the_grace…, a_ticket_route_answers_404…; tickets.rs inline 5 tests | None (200 {"ticket"} and 404 pinned; session-layer semantics complete) | — | — | — |
| #T5 | WebSocket stream (terminal.md:13) | terminal_ws missing-ticket 400/wrong-ticket 403/replay 403/expired 403 (paused clock)/101 upgrade | None | — | — | — |
| #T6 | Resize · redundant channel (terminal.md:14) | terminal_ws resize_over_the_redundant_http_channel…; WS resize frame in the same file; terminal_api inline | The resize route's own 404 and unknown-field 400 | resize_refusals_mirror_the_open_route (tests/terminal_ws.rs) | POST /api/terminal/sessions/deadbeef/resize {cols,rows} → 404 "no such terminal session"; live session + {"cols":80,"rows":24,"force":true} → 400 containing unknown field "force" | oneshot |
| #T7 | Closing a session (terminal.md:15) | terminal_ws delete_closes_the_session_and_the_listing_empties; session_tests shutdown/close family | The visible reason line + final frame for the **already attached** client on DELETE (the existing delete test never attaches) | delete_while_attached_tells_the_client_why (tests/terminal_ws.rs) | DELETE → 200 {"id":…,"closed":true}; WS then receives a Binary line containing "closed on request" (the CloseReason::Requested message) and Text {"t":"error","message":"closed on request"}, then the socket closes | Real socket (WS frames); non-Exited closes take the error final frame (same as the stop test) |
| #T8 | Keystrokes/control frames (terminal.md:16) | terminal_ws a_good_ticket… (binary both ways, resize frame, stream continues after {"t":"ping"} is ignored); terminal_api inline control_frames_parse_strictly | None | — | — | — |
| #T9 | Server control frames (terminal.md:17) | terminal_ws exit frame (a_good_ticket…), error frame (stopping_the_plugin…); terminal_api inline server_control_objects_are_exact; session_tests stalled (session layer) | The stalled text frame at the WS layer (zero integration tests have seen {"t":"stalled"} on the wire) | a_stalled_socket_is_warned_and_then_closed (tests/terminal_ws.rs) | Client stops reading + flood from the far end → once reading resumes, Text {"t":"stalled"} appears in the frame stream; then a Binary line/Text error containing "accepted no output" (the CloseReason::Stalled message); socket closes; GET sessions converges to [] | Real socket + timing-sensitive: stallSeconds=1, flood ≥8 MB (kernel+axus buffers absorb a few MB first); ordering of stalled vs data frames not asserted (same sink); P1 but conservative assertions |
| #T10 | Multi-attach fan-out (terminal.md:18) | session_tests a_second_attachment_joins_rather_than_replaces (session layer) | Two-socket fan-out at the WS layer (the product path of two tabs opening the same session) | a_second_tab_fans_out_output_and_both_inputs_reach_the_shell (tests/terminal_ws.rs) | Mint two tickets, both sockets get 101; after far.say("fan-out") **both** sockets receive Binary containing "fan-out"; ws_a/ws_b each send Binary input → far hears two Data entries in turn; far.exit(0) → both sockets receive {"t":"exit","code":0} then close | Real socket; the second ticket re-minted via POST /ticket |
| #T11 | Disconnect grace + catch-up buffer (terminal.md:19) | terminal_ws a_reconnect_within_the_grace…; session_tests catch-up/bounded drop/honest admission, 3 tests | None | — | — | — |
| #T12 | Idle/stall timeouts (terminal.md:20) | session_tests four-timer 4 tests (paused clock: idle both directions/output keepalive/never-attached collection/stall warn-then-close) | Nothing separate at the integration layer (the stall WS close is covered incidentally by the #T9 new test) | — | — | — |
| #T13 | asciicast v2 recording (terminal.md:21) | terminal_ws a_recording_is_written_when_configured; recording.rs inline 4 tests; session_tests 2 tests | A look at the cast file contents at the HTTP layer (header + output lines), pinning the writer end-to-end onto the route | the_cast_file_holds_a_v2_header_and_the_said_output (tests/terminal_ws.rs) | open(recording:true) → 201 with the recording path; attach, then far.say("recorded-line"); DELETE → 200; read the file: first line is JSON with version=="2", width==80, height==24, env.TERM=="xterm-256color"; some [t,"o",…] line contains "recorded-line"; the file contains the "closed on request" end marker | File read happens after DELETE (finish has flushed); P2 |
| #T14 | Local shell · off by default (terminal.md:22) | terminal_ws targets local off; session_tests local-toggle/view/override 3 tests; local.rs real PTY 7 tests; conpty 14 tests | The full chain (local.enabled + request-level shell override + real ConPTY + WS stream) — the local toggle's "once on, a local session can really be opened" has never been walked over HTTP | a_local_session_runs_a_real_command_end_to_end (tests/terminal_ws.rs) | rig config local.enabled=true; POST {"target":"local","cols":80,"rows":24,"shell":"whoami"} → 201 {id,ticket,recording==null}; after connect, receive ≥1 non-empty Binary frame (whoami output; VT sequences included still count) then Text {"t":"exit","code":0}; GET sessions converges to [] | Real short-lived child process (whoami prints and exits; works on both Windows and unix); PtyCommand::new takes only a single program name — the shell override cannot carry arguments (spelled out in a comment); real socket |
| #T15 | Remote SSH sessions (terminal.md:23) | terminal_ws everything (FakeShells placeholder); shell.rs contract layer 15 tests; tunnels-side manager.rs inline (separate volume) | A real russh channel does not enter the oneshot suite (see the "not suitable" section) | — | — | live-verify |
| #T16 | Plugin config read/write (terminal.md:24) | terminal.rs inline 3 tests; plugin_host config_api_validates… (generic shape), config_driven_restart… (generic restart semantics) | terminal's own closed loop: PUT /api/plugins/terminal/config → save triggers restart → live sessions closed with a reason → new config in effect (no test ties those three steps together) | a_config_put_restarts_the_plugin_and_closes_live_sessions (tests/terminal_ws.rs) | GET /api/plugins/terminal/config for the revision; open a session and attach; PUT {"config":{…local.enabled=true…quiet timer keys…}} → 200 with revision grown; WS receives the "closed: the terminal plugin is stopping" line + {"t":"error"}; GET sessions == []; GET targets local.enabled==true | Real socket; PUT shape copied from plugin_host config_api_validates… ({"config": …}, reply {revision,config,schema}) |
| #T17 | Plugin disable (terminal.md:25) | terminal_ws the_disabled_terminal_plugin_answers_the_structured_503, stopping_the_plugin_closes_live_sessions…; plugin_host boot_disabled… (generic boundary) | None | — | — | — |

### 1.3 Gap-Test Arrange/Act/Assert Details (ready to copy)

**targets_without_a_provider_say_who_is_missing** (tests/terminal_ws.rs)
- Arrange: `rig_with("absent", quiet(), ProviderKind::None)` — the same boot sequence as `rig()`, but FakeShells is not registered with `services.shells` (tunnels already disabled; the seat sits empty).
- Act: `rig.http("GET", "/api/terminal/targets", None)` (oneshot).
- Assert: 200; `body["local"]["enabled"]==json!(false)`; `body["local"]["shell"]` non-empty (the real LocalShells default program); `body["remote"]["presence"]==json!("absent")`; `body["remote"]["targets"]==json!([])`; `body["remote"]["reason"].as_str()` non-empty and containing "provides". Note: the `Absent(Some("tunnels"))` variant that names tunnels is covered at the session layer by the same-named test in session_tests; this test pins only the HTTP shape.

**an_open_body_is_validated_field_by_field** (tests/terminal_ws.rs)
- Arrange: `rig("open-validation", quiet())`.
- Act/Assert: for each case below, `POST /api/terminal/sessions` (oneshot):
  1. `{}` → 400, text contains "target: a non-empty string is required";
  2. `{"target":"box-one","cols":80,"rows":24,"color":"red"}` → 400, contains `unknown field "color"` (known: target, cols, rows, shell);
  3. `{"target":42,"cols":80,"rows":24}` → 400, the same target message (as_str comes up empty);
  4. `{"target":"box-one","cols":80,"rows":24,"shell":7}` → 400 "shell: must be a string";
  5. `{"target":" box-one ","cols":80,"rows":24}` → 201 (positive case: target is trimmed; take the id and finish with a DELETE).
- Closing assertion: after every rejection except the positive case, `GET /api/terminal/sessions` is `[]` (a rejection never leaves a half-open session).

**opening_an_unknown_target_is_a_409_naming_it** (tests/terminal_ws.rs)
- Arrange: `rig("unknown-target", quiet())`.
- Act: `POST {"target":"ghost","cols":80,"rows":24}`.
- Assert: 409 (the Refused mapping); text contains "ghost" and "no such"; `GET /api/terminal/sessions` == `[]`.

**a_provider_that_fails_to_open_is_a_502** (tests/terminal_ws.rs)
- Arrange: `rig_with("open-fails", quiet(), ProviderKind::FailsToOpen)` — the provider's list still returns box-one, but `open()` returns `Err(ShellError::Failed("the far side is on fire"))`.
- Act: `POST {"target":"box-one","cols":80,"rows":24}`.
- Assert: 502 (the Shell mapping); text contains "the far side is on fire" (the message passed through verbatim); sessions == `[]`.

**the_session_listing_orders_by_opened_and_marks_attachment** (tests/terminal_ws.rs)
- Arrange: `rig("order", quiet())`; A = rig.open().await for (id_a, ticket); B = rig.open().await; connect_session(id_a, ticket_a) 101.
- Act: `GET /api/terminal/sessions`.
- Assert: array length 2 with `rows[0]["id"]==id_a` and `rows[1]["id"]==id_b` (opened ascending); `rows[0]["attached"]==json!(true)`, `rows[1]["attached"]==json!(false)`; both rows' target/label are "box-one"/"the one box".

**resize_refusals_mirror_the_open_route** (tests/terminal_ws.rs)
- Arrange: `rig("resize-refuse", quiet())`; open for the id.
- Act/Assert: ① `POST /api/terminal/sessions/deadbeef/resize` body `{"cols":80,"rows":24}` → 404 "no such terminal session"; ② `POST /api/terminal/sessions/{id}/resize` body `{"cols":80,"rows":24,"force":true}` → 400 contains `unknown field "force"`; ③ `{"cols":80}` → 400 "rows is required".

**delete_while_attached_tells_the_client_why** (tests/terminal_ws.rs)
- Arrange: `rig("delete-live", quiet())`; open + connect 101.
- Act: `DELETE /api/terminal/sessions/{id}` (oneshot) → assert 200 and body `{"id": id, "closed": true}`.
- Assert (WS): loop `recv` until a Binary line contains "closed on request" (CloseReason::Requested's user-facing sentence written into the terminal); the following Text frame parses as `{"t":"error","message":"closed on request"}` (the final frame of a non-Exited close is error — isomorphic to the stop test); after that the socket closes (`ws.next()` giving None/Close/Err are all fine; copy the closing match from a_good_ticket…).

**a_stalled_socket_is_warned_and_then_closed** (tests/terminal_ws.rs)
- Arrange: config = `quiet()` + `{"stallSeconds": 1, "graceSeconds": 86400}`; `rig("stalled", config)`; open + connect 101; **the client does not read**.
- Act: spawn a task running `flood(far, 8 * 1024 * 1024)` against the far end (4 KiB chunks back to back; over a real socket the kernel buffer + hyper write buffer absorb a few MB first, and 8 MB guarantees write_side parks → the attachment queue fills → the driver stops reading → the stall clock starts); once the flood task finishes sending, the client enters the `ws.next()` loop.
- Assert: Text `{"t":"stalled"}` **appears** in the frame stream (no other fields; ordering relative to data frames not asserted — same sink); keep reading and a Binary line or Text error appears containing "accepted no output" (the CloseReason::Stalled message "closed: this terminal accepted no output for 1 seconds (stallSeconds)"); socket closes; `GET /api/terminal/sessions` converges to `[]` via bounded poll.
- Risk note: this is the only one of the new tests that consumes real timing (1 s stall + 1 s notice); both the flood volume and the assertions take the conservative side, and on failure the message must print the total bytes received so far.

**a_second_tab_fans_out_output_and_both_inputs_reach_the_shell** (tests/terminal_ws.rs)
- Arrange: `rig("fanout", quiet())`; `(id, ticket_a) = rig.open().await`; far = rig.far(); `POST /api/terminal/sessions/{id}/ticket` → 200 for `ticket_b`.
- Act: `connect_session(id, ticket_a)` and `connect_session(id, ticket_b)` both 101; `far.say("fan-out\n")`; `ws_a.send(Binary(b"one\r"))`, `ws_b.send(Binary(b"two\r"))`.
- Assert: ① ws_a and ws_b **each** `recv` Binary containing "fan-out" (output is copied to every attachment); ② `far.heard()` twice, matching `PtyInput::Data(b"one\r")` and `PtyInput::Data(b"two\r")` (input from any attachment reaches the PTY; arrival order not asserted); ③ `far.exit(0)` → both sockets receive Text `{"t":"exit","code":0}` and then close.

**the_cast_file_holds_a_v2_header_and_the_said_output** (tests/terminal_ws.rs)
- Arrange: `rig("cast", json!({"recording": true, "idleTimeoutMinutes": 0, "stallSeconds": 0}))`; POST open `{"target":"box-one","cols":80,"rows":24}` → 201, body`["recording"]` is a path string; connect (far side `rig.far()`).
- Act: `far.say("recorded-line\n")`; wait one beat (`recv` that Binary); `DELETE /api/terminal/sessions/{id}` → 200; read the cast file (`std::fs::read_to_string`).
- Assert: the first line is JSON: `version=="2"`, `width==80`, `height==24`, `env["TERM"]=="xterm-256color"`; some later line matches `[…,"o","…recorded-line…"]` (an asciicast v2 output event); the full text contains "closed on request" (the end marker carries the CloseReason message; details defer to the recording.rs inline `the_cap_stops_the_recording…` / `finishing_twice_writes_one_marker`).

**a_local_session_runs_a_real_command_end_to_end** (tests/terminal_ws.rs)
- Arrange: config = `{"local": {"enabled": true}, "idleTimeoutMinutes": 0, "stallSeconds": 0, "recording": false}`; `rig("local-e2e", config)` (FakeShells still in place as usual, but this test does not touch the remote).
- Act: `POST /api/terminal/sessions` body `{"target":"local","cols":80,"rows":24,"shell":"whoami"}` → 201; assert body`["recording"]` is Null (recording:false → no file); `connect_session(id, ticket)` 101.
- Assert: at least one non-empty Binary frame arrives (`whoami`'s output; ConPTY wraps VT sequences, so assert non-empty only); then Text `{"t":"exit","code":0}`; socket closes; `GET /api/terminal/sessions` converges to `[]` via bounded poll (a local session likewise occupies the listing and exits cleanly).
- The comment must state: `PtyCommand::new` accepts only a single program name, so the `shell` override cannot carry arguments ("cmd /c echo" would be taken as the program name) — `whoami` is chosen precisely because it prints and exits and resolves on PATH.

**a_config_put_restarts_the_plugin_and_closes_live_sessions** (tests/terminal_ws.rs)
- Arrange: `rig("config-restart", quiet())` (local off); `GET /api/plugins/terminal/config` → 200 for `revision`; open + connect 101.
- Act: `PUT /api/plugins/terminal/config` body `{"config": {"local": {"enabled": true}, "idleTimeoutMinutes": 0, "stallSeconds": 0, "recording": false}}` → assert 200, the reply's `revision` greater than the old value, and the reply carrying the three keys `{revision, config, schema}` (shape copied from plugin_host config_api_validates…).
- Assert: ① the WS client receives a Binary line containing "closed: the terminal plugin is stopping" (the Shutdown message) and Text `{"t":"error",…}` (restart_on_config_change's close path shares the stop reason); ② `GET /api/terminal/sessions` == `[]`; ③ `GET /api/terminal/targets` → `local.enabled==json!(true)` (the new instance came up on the new config); ④ `POST /api/terminal/sessions {"target":"ghost",…}` still 409 (the new instance's session machine is alive, not 503).

---

## Chapter 2 Process Capabilities and the Shared Process Supervisor (swiss-host / swiss-core::platform / swiss-mcp proc)

### 2.1 Existing Coverage Inventory (test file/module → test name list → corresponding functionality points)

Matrix row numbers #P1..#P13 correspond to section 2.2; audit line numbers refer to `.agents/docs/process.md`.

**tests/plugin_host.rs (5 execution-surface-specific tests + 2 adjacent)**

| Test name | Functionality point |
| --- | --- |
| the_execution_surface_lists_capabilities_and_runs_them | #P1, #P3, #P4, #P5, #P7 (/api/actions shape, 202+runId+owner manual, listing+capacity, ?output=0, echo real child) |
| a_submission_refusal_is_typed_and_leaves_no_run_behind | #P3 (action required/unknown capability 400/timeoutMs 0 → 400; GET 404; cancel 404; refusals leave no run behind) |
| disabling_the_process_plugin_withdraws_its_capabilities_but_keeps_the_history | #P12 (capabilities absent, new submissions 400 naming it, history readable, enable restores) |
| a_long_run_is_cancelable_over_the_api_and_reaped_before_the_answer | #P6 (cancel waits for the reap, first-wins idempotence; a real cmd→ping subtree tree-killed) |
| an_unregistered_action_saves_with_a_warning_and_refuses_to_run (jobs perspective) | #P1 adjacent (behavior of a jobs definition when the capability is missing) |
| editing_a_config_definition_applies_in_place_without_killing_in_flight_runs | #P3 adjacent (the in-flight-not-killed semantics) |
| boot_disabled_plugins_guard_every_route_they_own / every_contributed_page_entry_is_actually_served / route_ownership_is_longest_prefix_at_segment_boundaries | #P12 adjacent (plugin boundaries and page promises) |

**tests/adminapi.rs**: reports_gateway_memory_without_walking_a_process_tree_when_nothing_was_spawned (#P11 no-child shape: gatewayMb>0, childrenMb==0, childrenPending==false, processCount==1).

**crates/swiss-host/src/services/process.rs inline (5 tests)**: the_tail_buffer_keeps_only_the_last_max_bytes (BoundedCapture 16 KiB), secret_masking_needs_a_worthwhile_value, strict_env_resolution_fails_naming_the_variable, a_reader_stops_promptly_on_cancellation, a_reader_drains_to_eof_even_past_the_capture_cap (real-child reader cancellation/EOF draining).

**crates/swiss-host/src/services/runs.rs inline (11 tests)**: a_run_reaches_one_terminal_view_and_stays_readable, capacity_is_a_visible_refusal_not_a_growing_task_set (coordinator-layer capacity), a_queued_successor_starts_when_the_slot_frees, cancellation_is_scoped_to_the_owner_that_asked, a_cancel_after_the_finish_reports_the_finished_view, a_deadline_ends_the_run_as_timed_out_not_as_canceled (fake action), unknown_and_uncancelable_actions_are_refused_at_submit, a_panicking_action_is_one_failed_run_not_a_poisoned_pool (#P3/#P4/#P6 semantics layer).

**crates/swiss-host/src/services/actions.rs inline (12 + 4 tests)**: typed_exec_runs_and_captures_output, typed_input_is_validated_strictly (unknown keys/missing program/args type), an_arg_with_spaces_stays_one_argument, a_resolved_reference_is_masked_out_of_captured_output, cancellation_stops_a_typed_run_and_reports_it, a_cancel_that_lands_before_the_run_takes_its_handle_is_not_lost, ref_names_scan_in_order_and_deduplicate, legacy_command_tokenizes_like_the_old_runner, a_failing_legacy_command_is_an_outcome_not_an_error, legacy_command_env_vars_reach_the_child_and_refs_in_values_resolve, a_missing_vault_reference_in_a_command_is_refused, a_present_vault_reference_resolves_and_masks, an_unreferenced_vault_value_never_reaches_a_child_environment (#P1/#P2 action layer).

**crates/swiss-host/src/services/action.rs inline (2 tests)**: the_registry_resolves_by_name_and_refuses_duplicates, a_cancel_source_signals_every_handle (#P7 contract).

**crates/swiss-host/src/proc_pids.rs inline (6 tests)**: ledger_shape_is_pretty_pids_array, note_is_idempotent_and_drop_removes, a_missing_or_torn_file_reads_empty, reap_clears_first_and_skips_own_pid_and_dead_pids, reap_on_an_empty_ledger_is_a_no_op, the_file_name_is_port_scoped (#P10; the module comment says outright that "actually killing live foreign PIDs" is not tested).

**crates/swiss-host/src/mem.rs inline (10 tests)**: reports_megabytes_to_one_decimal, reports_the_gateways_own_footprint_without_walking_anything, a_gateway_with_no_children_reports_zero_rather_than_pending, the_panels_poll_never_pays_for_the_walk, a_walk_that_found_nothing_reports_pending_not_a_confident_zero, counts_the_gateway_alongside_the_subtree_it_measured, reuses_the_walk_within_the_cache_window_and_re_measures_after_an_invalidate, a_stale_entry_is_re_measured_rather_than_served, keys_the_cache_on_the_set_of_children_not_the_order_they_arrived_in, the_gateway_is_never_its_own_child (#P11, planted cache).

**crates/swiss-mcp/src/registry.rs inline (~30 tests, of which 9 lazy/idle)**: a_proc_mcp_is_lazy_by_default_and_every_other_type_is_not, a_lazy_entry_rests_at_idle_and_returns_to_idle_when_stopped, ensure_started_wakes_an_idle_entry_and_passes_a_running_one_through, the_idle_reaper_only_arms_for_lazy_entries, idle_ms_zero_opts_out_of_reaping_entirely, the_sweeper_reaps_a_lazy_child_whose_deadline_passed, activity_pushes_the_reap_deadline_back, child_pids_is_empty_when_nothing_was_spawned, close_all_stops_every_entry (#P8/#P9, fake builder); plus the full start/stop/restart/rename/delete/ping/race set (the separate mcp.md volume's home turf).

**crates/swiss-mcp/src/adapters/proc.rs inline (12 tests)**: tokenize family 5, decode (UTF-8/GBK fallback/replacement) 3, the_stderr_ring_keeps_only_the_tail, env_timeout_parsing_matches_number_or_default, proxy_cfg_resolves_toggles_and_timeout_from_the_def (#P8 adapter layer).

**crates/swiss-core/src/platform/mod.rs inline (8 tests)**: follows_a_chain_to_the_bottom_not_just_the_first_child, a_neighbours_subtree_is_never_swept_in (PID-reuse guard), the_root_itself_is_part_of_its_own_tree, several_roots_merge_into_one_set_without_double_counting, an_excluded_pid_takes_the_branch_below_it_with_it, a_parent_loop_terminates_instead_of_spinning, a_pid_the_snapshot_never_saw_is_just_itself, no_roots_is_an_empty_walk_not_the_whole_machine (#P13 BFS semantics, fake snapshot).

**src/builtin.rs inline (2 tests)**: mcp_descriptor_contributes_the_token_page_but_keeps_its_routes_host_owned, mcp_plugin_label_differs_from_every_page_label (#P12 adjacent; the process plugin descriptor itself has no dedicated assertions — pages:[]/routes:[] are pinned indirectly via the_execution_surface…).

### 2.2 Functionality-Point → Test Matrix

| # | Functionality point (audit-doc line) | Existing tests | Gap | New test name (planned home) | Assertion essentials (method+path+status code+JSON fields/shape) | Special handling |
| --- | --- | --- | --- | --- | --- | --- |
| #P1 | process.exec capability action (process.md:9) | plugin_host the_execution_surface…; actions.rs inline 10 tests | ① the `outputTruncated` chain (BoundedCapture→meta→RunView) and the 16 KiB tail never asserted at the HTTP layer; ② same for the stderr `[stderr]` tag; ③ a strict env reference with a missing variable naming the variable in a 400 at the HTTP layer (unit + action layer only so far) | a_chatty_childs_output_is_capped_and_flagged_truncated (tests/plugin_host.rs); stderr_arrives_tagged_next_to_stdout (tests/plugin_host.rs); the strict-reference 400 folded into the contrast test on the next row | ① POST /api/runs (action process.exec, input chatty_input>16 KiB) → 202 → await_run: state=="succeeded", exitCode==0, outputTruncated==json!(true), output contains the last-chunk marker and not the first-chunk marker, chars is a number; ② a command writing only to stderr → output contains "[stderr]" and the original text; ③ missing env variable → 400 message containing "SWISS_TEST_UNSET_VAR_X is not set" | Real short-lived child (cmd for / sh seq, ≈26 KB of output then exit); await_run existing helper; oneshot |
| #P2 | process.legacy-command capability action (process.md:10) | plugin_host (the_execution_surface… after-enable section); actions.rs inline (tokenize/failure-as-outcome/env arrival) | The lenient-vs-strict **contrast** between the two reference strictnesses has never been pinned in one place (the old semantics "unset→empty string" not silently changing meaning is a compatibility promise on record) | a_legacy_unset_reference_runs_empty_where_exec_refuses (tests/plugin_host.rs) | Two sections: legacy submits `{"command": legacy_echo("legacy-ok"), "env": {"NOTE": "${SWISS_TEST_UNSET_VAR_X}"}}` → 202 → await_run succeeded; exec submits an env with the same variable → 400 naming the variable | The env var name is invented inside the test to guarantee it is unset; real child; oneshot |
| #P3 | Manually submitted run (process.md:11) | plugin_host the_execution_surface… (202+runId+owner manual+run echo), a_submission_refusal… (timeoutMs 0 and over-cap 400) | ① the queueIfBusy queueing path (HTTP parameter) untested (covered together with #P4's 429); ② **zero coverage of the valid-timeoutMs chain** — api.rs accepting the deadline, the coordinator cancelling, and the honest "timeout" terminal state; existing tests only covered 0/illegal values and coordinator semantics with a fake action, and the real-child + HTTP-parameter line has never been walked | a_deadline_over_the_api_ends_a_run_as_timeout (tests/plugin_host.rs); the queueing path folded into a_full_pool_answers_429… (see the #P4 row) | POST /api/runs {action process.exec, input slow_input(), timeoutMs 1500} → 202 → await_run: state=="timeout", timedOut==json!(true), the canceled flag absent ("the deadline owns the timeout truth"), error contains an explanation that the process was terminated; contrast: the same input without timeoutMs is not asserted here (the 60 s default is too long; that semantics is pinned by the runs.rs inline) | Real long-running child (ping -n 60); timeoutMs must be ≥ child startup overhead (1500 ms is safe); oneshot |
| #P4 | Run listing (process.md:12) | plugin_host the_execution_surface… (runs+capacity shape) | **api.rs's 429 branch at zero coverage** (the runs.rs inline tests only the coordinator layer); a queueIfBusy=true queued successor likewise never walked at the HTTP layer | a_full_pool_answers_429_naming_the_capacity_and_a_queued_successor_runs (tests/plugin_host.rs) | A capacity-1 rig: placeholder slow run A → 202; B without queueIfBusy → 429 message containing "maxConcurrentRuns" and "(1/1)"; C with queueIfBusy:true → 202 and polling GET C?output=0 shows state=="queued"; cancel A → 200 canceled; await_run(C) → succeeded; GET /api/runs's capacity.maxConcurrentRuns==1 | Real long-running child (ping -n 60); full_app with a jobs capacity config; if the jobs schema disallows maxConcurrentRuns=1, fall back to the default 2: two placeholders + a third getting 429 + a fourth queueing |
| #P5 | Single run detail (process.md:13) | plugin_host the_execution_surface… (?output=0 drops output but keeps state; the 404 double case in the refusal test) | The outputTruncated flag (folded into the #P1 new test) | — | — | — |
| #P6 | Cancel run (process.md:14) | plugin_host a_long_run_is_cancelable… (waits for the reap + first-wins idempotence, real subtree tree-kill); runs.rs inline owner scoping/late cancel | None | — | — | — |
| #P7 | Capability listing (process.md:15) | plugin_host the_execution_surface… (both capabilities, provider/cancelable/schema), disabling_the_process_plugin… (rise and fall) | None | — | — | — |
| #P8 | proc MCP child-process hosting (process.md:16) | registry.rs inline full lifecycle/race set (fake builder); proc.rs inline 12 tests | A real stdio MCP child does not enter the oneshot suite (see the "not suitable" section: the handshake timeout is a process-wide OnceLock and needs a real MCP server) | — | — | live-verify (scripts/test-instance.ps1 + a real npx server) |
| #P9 | proc child idle reaping (process.md:17) | registry.rs inline 7 tests (lazy default/waking/sweeper/idleMs 0/activity pushes back/child_pids) | None (the fake builder already tests the semantics to the full; real-child reaping joins #P8 in live-verify) | — | — | — |
| #P10 | proc orphan-child reckoning (process.md:18) | proc_pids.rs inline 6 tests (ledger shape/idempotence/torn file/reap rules/port scoping) | The real tree-kill of live foreign PIDs and the boot sequencing (server.rs:53-68, on the serve path) both stay out of the oneshot suite | — | — | live-verify: kill the 19998 instance first to leave orphans behind, restart, watch the logs |
| #P11 | Process-tree memory accounting (process.md:19) | adminapi reports_gateway_memory… (no children); mem.rs inline 10 tests (pending/cache/excludes itself, planted) | Real-subtree measurement with `?tree=1`: the adminapi route needs a non-empty registry.child_pids(), i.e. a real proc child → integration infeasible; replaced by a crate-inline real-child test | a_real_child_subtree_measures_nonzero_and_counts_the_gateway (**crates/swiss-host/src/mem.rs inline**, a non-integration replacement) | Spawn a real child (ping -n 60) inside the tree_cache() lock for the pid; after invalidate, get_memory_info(&[pid], true): childrenMb>0.0, processCount>=2, childrenPending==false, measuredAt present; same pid without measuring: childrenPending==true; kill at the end | Real child; serialized by mem.rs's existing tree_cache() global lock; a replacement scheme — the route shape is already pinned by the adminapi tests |
| #P12 | Plugin disable (process.md:20) | plugin_host disabling_the_process_plugin… (capabilities withdrawn/history readable/enable restores) | **In-flight runs** cancelled and awaited to completion on disable (the audit says outright "in-flight runs are cancelled and awaited"; in the existing test the run was long terminal before the disable) | disabling_the_process_plugin_cancels_its_in_flight_runs (tests/plugin_host.rs) | Submit a slow run → poll state=="running" → POST /api/plugins/process/disable → 200; await_run → state=="canceled", canceled==json!(true); GET /api/runs/{id} still 200 and readable; GET /api/actions == [] | Real long-running child; the real disable-means-tree-kill path |
| #P13 | Platform process-tree walk/kill (process.md:21) | platform/mod.rs inline 8 tests (BFS semantics/cycle guards/exclusion semantics, fake snapshot); windows.rs has no tests (direct Win32 calls); job.rs has no inline tests | No new integration: the real tree kill is already exercised naturally by the #P6 existing test (cancelling a real cmd→ping subtree); the Win32 snapshot is covered via the memory surface and the #P11 replacement test | — | — | The platform snapshot is a process-global real call; unit tests rely on injecting a fake snapshot (existing helper in mod.rs) |

### 2.3 Gap-Test Arrange/Act/Assert Details (ready to copy)

**a_chatty_childs_output_is_capped_and_flagged_truncated** (tests/plugin_host.rs)
- Arrange: `let (app, _, _) = full_app("chatty", json!({})).await`; `chatty_input()` builds an input that emits >16 KiB (16 * 1024 = DEFAULT_OUTPUT_MAX_BYTES) of output and exits quickly: Windows `{"program": "cmd", "args": ["/c", "for", "/L", "%i", "in", (1,1,600)", "do", "@echo", "aaaaaaaa…(42-byte marker + line number)"]}` (zero-pad the line number so the tail marker is assertable), unix `{"program": "sh", "args": ["-c", "for i in $(seq 600); do echo \"aaaa…\$i\"; done"]}`.
- Act: `POST /api/runs` body `{"action": "process.exec", "label": "chatty", "input": chatty_input()}` → assert 202; `await_run(&app, run_id)`.
- Assert: `view["state"]==json!("succeeded")`, `view["exitCode"]==json!(0)`; `view["outputTruncated"]==json!(true)` (absent-not-null: the flag appears only when true); `view["output"]` non-empty, contains the last-chunk marker (e.g. "00000599") and does **not** contain the first-chunk marker ("00000001") — the cap keeps the tail; `view["chars"].as_u64().is_some()`; `view["output"].len()` ≤ 16 KiB + label overhead (a rough 20 KiB upper bound suffices; exact boundaries are left to the process.rs unit tests).

**stderr_arrives_tagged_next_to_stdout** (tests/plugin_host.rs)
- Arrange: `full_app("stderr", json!({}))`; input: Windows `{"program": "cmd", "args": ["/c", "echo", "to-stdout", "&", "echo", "boom-from-stderr", "1>&2"]}`, unix `{"program": "sh", "args": ["-c", "echo to-stdout; echo boom-from-stderr >&2"]}`.
- Act: submit → 202 → await_run.
- Assert: succeeded; output contains "to-stdout", "[stderr]", and "boom-from-stderr" (both pipes merged, stderr tagged; ordering not asserted).

**a_legacy_unset_reference_runs_empty_where_exec_refuses** (tests/plugin_host.rs)
- Arrange: `full_app("refs-strictness", json!({}))`; variable name `SWISS_TEST_UNSET_VAR_X` (never set inside the test, avoiding environment pollution).
- Act/Assert, first section (lenient): `POST /api/runs` body `{"action": "process.legacy-command", "input": {"command": legacy_echo("legacy-ok"), "env": {"NOTE": "${SWISS_TEST_UNSET_VAR_X}"}}}` → 202; `await_run` → state=="succeeded", output contains "legacy-ok" (an unset reference expands to an empty string and never 400s — the historical semantics of the Node-era jobs.json).
- Act/Assert, second section (strict): `POST /api/runs` body `{"action": "process.exec", "input": echo_input("x") with env changed to: {"env": {"NOTE": "${SWISS_TEST_UNSET_VAR_X}"}}}` → 400; `json["error"]` contains "SWISS_TEST_UNSET_VAR_X" and "is not set"; `GET /api/runs`'s runs empty or not containing the rejected one (a refusal leaves no run behind).

**a_full_pool_answers_429_naming_the_capacity_and_a_queued_successor_runs** (tests/plugin_host.rs)
- Arrange: `full_app_capacity("pool")` — `full_app("pool", json!({"plugins": {"jobs": {"config": {"maxConcurrentRuns": 1}}}}))` (verify the jobs config schema allows 1 before implementing; if the minimum is >1, fall back to the default capacity of 2 and the Arrange becomes two placeholders); helper `slow_input()` (Windows `cmd /c ping -n 60 127.0.0.1` / `sh -c "sleep 60"`, lifted out of a_long_run… into a shared function).
- Act: ① submit A (slow_input, label "a") → 202; poll `GET /api/runs/{A}?output=0` until state=="running"; ② submit B (without queueIfBusy) → **429**, error contains "maxConcurrentRuns" and "(1/1)"; ③ submit C (body adds `"queueIfBusy": true`) → 202; poll `GET /api/runs/{C}?output=0` until state=="queued"; ④ `POST /api/runs/{A}/cancel` → 200 canceled; ⑤ `await_run(C)`.
- Assert: ②③④⑤ as above; `GET /api/runs`'s `capacity.maxConcurrentRuns==json!(1)`; B's rejection left no run (runs holds exactly the two ids A and C).

**disabling_the_process_plugin_cancels_its_in_flight_runs** (tests/plugin_host.rs)
- Arrange: `full_app("stop-inflight", json!({}))`; submit a slow run → 202; poll `GET /api/runs/{id}?output=0` until state=="running" (the disable must race a real child, so see running first).
- Act: `POST /api/plugins/process/disable` → 200; `await_run(&app, run_id)`.
- Assert: `view["state"]==json!("canceled")`, `view["canceled"]==json!(true)`, `view["timedOut"]` absent (not the deadline's fault); `GET /api/runs/{id}` still 200 with output readable (history preserved, echoing the existing withdraw test); `GET /api/actions`'s actions is an empty array (the same assertion as the existing test; what is pinned here is the ordered outcome of "collect the in-flight first, then withdraw resolution eligibility": by the time the disable answers, the run is already terminal).

**a_deadline_over_the_api_ends_a_run_as_timeout** (tests/plugin_host.rs)
- Arrange: `full_app("deadline", json!({}))`; input = `slow_input()` (Windows `cmd /c ping -n 60 127.0.0.1` / `sh -c "sleep 60"`).
- Act: `POST /api/runs` body `{"action": "process.exec", "label": "slow", "input": slow_input(), "timeoutMs": 1500}` → assert 202; `await_run(&app, run_id)`.
- Assert: `view["state"]==json!("timeout")` (RunState::TimedOut's wire string is "timeout", not "timed-out"); `view["timedOut"]==json!(true)`; `view` has no "canceled" key (absent-not-null — in the deadline-vs-cancel race only the former can write the terminal state); `view["error"]` non-empty (the timeout path's explanatory message); `view["ms"].as_u64()` near 1500 (a rough ≥1400 assertion suffices; no upper bound, to survive CI jitter). Reaped before the answer: the cancel test's "the child is already reaped when the answer comes" semantics holds here too (await_run returns only at a terminal state).

**a_real_child_subtree_measures_nonzero_and_counts_the_gateway** (crates/swiss-host/src/mem.rs inline, replacement scheme)
- Arrange: `let _guard = tree_cache().lock().await` (the existing global lock); spawn a real child for the pid: Windows `Command::new("cmd").args(["/c", "ping", "-n", "60", "127.0.0.1"]).spawn()`, unix `Command::new("sleep").arg("60").spawn()`; `invalidate_memory_cache()`.
- Act: `let v = get_memory_info(&[pid], true)`; then `get_memory_info(&[pid], false)`.
- Assert: `v["childrenMb"].as_f64() > 0.0`; `v["processCount"].as_u64() >= 2` (the gateway itself + at least one child); `v["childrenPending"]==json!(false)`; `v["measuredAt"]` present; the second call `childrenPending==json!(true)` (same pid set; honestly reports pending when not measuring). Cleanup: `child.kill()` + wait, guard released.

---

## New Test Helpers Needed (signatures)

**tests/terminal_ws.rs** (keep the existing `Rig`/`FarSide`/`recv`/`wait_detached`; add only three):

```rust
/// A generalization of rig(): the provider decides what sits in the shell seat — None
/// leaves it empty (the absent-seat targets shape), Fake installs the existing FakeShells,
/// FailsToOpen one whose list is fine but open() always fails (the 502 mapping). rig()
/// becomes a one-line rig_with(…, ProviderKind::Fake) wrapper; the existing 15 tests stay untouched.
enum ProviderKind { None, Fake, FailsToOpen }
async fn rig_with(tag: &str, terminal_config: Value, provider: ProviderKind) -> Rig;

/// The WS-side jam() from the session-machine tests: sends 4 KiB chunks to the far end until budget_bytes
/// is spent or the session closes, returning the bytes written. Over a real socket the kernel and hyper buffers
/// absorb a few MB first, so be generous (8 MB by default); the caller runs it in a spawned task so it never blocks the test body.
async fn flood(far: &FarSide, budget_bytes: usize) -> usize;

/// Reads a socket until needle appears in the accumulated bytes (5 s cap; panics
/// with what was read on timeout), returning all text read — shared by the stalled/exit tests; reuses recv()'s timeout philosophy.
async fn read_until(ws: &mut Ws, needle: &str) -> String;
```

**tests/plugin_host.rs** (keep `full_app`/`send`/`local`/`json_body`/`await_run`/`echo_input`/`legacy_echo`):

```rust
/// The ping/sleep input inlined in a_long_run_is_cancelable…, lifted into a shared
/// helper — both new tests (429 queueing and disable-cancel) need a long-running slot.
fn slow_input() -> Value;

/// An input that emits >16 KiB and exits quickly (each line zero-padded with its
/// line number so "keeps the tail" is assertable); Windows: cmd for /L; unix: sh -c 'for i in $(seq 600) …'.
fn chatty_input() -> Value;
```

**crates/swiss-host/src/mem.rs** (for the inline replacement test):

```rust
/// Starts a ~60 s real child (ping/sleep) and returns its PID — the Arrange for
/// a_real_child_subtree…; it can live inside the tests module, no test-utils visibility needed.
fn live_child_pid() -> std::process::Child;
```

## Points Not Suitable for Integration Tests, and Alternatives

| Functionality point | Why it stays out of root tests/ | Alternative |
| --- | --- | --- |
| #T15 a real russh channel for remote SSH sessions (tunnel/shell.rs) | Needs an SSH server and credentials; the contract shape (byte stream, leases, drain) is already pinned by FakeShells + shell.rs's 15 tests, and the tunnels side has its own FakeConn (manager.rs inline) | scripts/test-instance.ps1 live-verify: configure a real tunnel on the 19998 instance and open a remote session |
| #P8 a real stdio MCP child (proc adapter) | Needs a real MCP server child (npx etc., minutes-long cold start); `PROC_HANDSHAKE_TIMEOUT_MS` is a process-wide OnceLock that cannot be changed inside the test process and is not parallel-safe | registry/proc inline (fake builder + pure functions) already covers the semantics; live-verify starts a real npx server to verify the handshake, the stderr tail, PID accounting |
| #P9 the idle-reaping closed loop with a real child | Same as above: the reaping sweeper's semantics are fully tested by the registry inline; a real child just swaps in a different child | live-verify: dial idleMs down on 19998 and watch the panel go idle→reaped |
| #P10 the real tree-kill and timing of boot orphan reckoning (server.rs:53-68) | The reckoning sits on the serve startup path, out of oneshot's reach; "actually killing live foreign PIDs" is explicitly listed as untested in the proc_pids.rs module comment (the risk of a wrongful kill outweighs a missed one) | live-verify: start a proc MCP on 19998 → kill -9 the gateway → restart, watch for the `proc-pid reap complete` log and the orphans vanishing; the ledger rules remain guarded by the 6 inline tests |
| #P11 adminapi route integration for ?tree=1 | The route's children come from registry.child_pids(), non-empty only with a real proc child (same as #P8) | The route shape is already pinned by the childless adminapi.rs test; real measurement goes through the mem.rs inline replacement test in 2.3 |
| Panel terminal-page interactions (reconnect backoff, paste keyboard, font size, epoch lifecycle) | admin_assets is byte-for-byte frozen (ADR-009); the panel JS's spec and tests live in the Node repo | vitest in ../local-mcp-gateway (the pure-function half, terminal-core.js); this repo only guards the_tree_is_byte_for_byte_the_node_builds |
| Panel Jobs page Run now / memory-chip clicks | Same as above; DOM behavior belongs to Node | vitest + live-verify; in this repo held by proxy through the /api/runs and /api/memory shape tests |

## Priorities (P0 security & wire contracts / P1 main paths / P2 edge cases)

**P0 (wire contracts: status-code/field semantics the panel consumes directly; the existing P0 surface is already complete — the items below close the gaps)**

1. an_open_body_is_validated_field_by_field (#T2 — the 400 semantics and messages are Node shape contracts)
2. opening_an_unknown_target_is_a_409_naming_it + a_provider_that_fails_to_open_is_a_502 (#T2 — the 409/502 branches of the terminal_api.rs:117-125 error funnel, zero coverage repo-wide)
3. a_full_pool_answers_429_naming_the_capacity_and_a_queued_successor_runs (#P4 — api.rs's 429 branch at zero coverage + queueIfBusy semantics)
4. Fold a_deadline… into the 429 test? No — see the next item, listed separately: a_legacy_unset_reference_runs_empty_where_exec_refuses (#P1/#P2 — the strict reference's named 400 is the credential-reference contract, the lenient one is the Node compatibility promise; both strictnesses pinned in one place)

**P1 (main paths)**

5. a_second_tab_fans_out_output_and_both_inputs_reach_the_shell (#T10)
6. a_stalled_socket_is_warned_and_then_closed (#T9 — the only server control frame never seen on the wire; timing-sensitivity accepted, assertions kept conservative)
7. delete_while_attached_tells_the_client_why (#T7 — user visibility of the close path)
8. a_config_put_restarts_the_plugin_and_closes_live_sessions (#T16 — terminal's own restart closed loop)
9. a_local_session_runs_a_real_command_end_to_end (#T14 — the switch-on path of the "local off by default" safety toggle has never been walked)
10. a_chatty_childs_output_is_capped_and_flagged_truncated (#P1 — the outputTruncated wire field + the read-time cap end to end)
11. a_deadline_over_the_api_ends_a_run_as_timeout (#P3 — timeoutMs is an API parameter; the valid-value chain and the "timeout" terminal state had zero coverage before; AAA in 2.3)
12. disabling_the_process_plugin_cancels_its_in_flight_runs (#P12 — disable really releases)
13. targets_without_a_provider_say_who_is_missing (#T1 — the reason shape the panel renders for the empty state)
14. stderr_arrives_tagged_next_to_stdout (#P1)

**P2 (edge cases)**

15. the_session_listing_orders_by_opened_and_marks_attachment (#T3)
16. resize_refusals_mirror_the_open_route (#T6)
17. the_cast_file_holds_a_v2_header_and_the_said_output (#T13 — the format is already pinned by the recording.rs inline; this adds the end-to-end glue)
18. a_real_child_subtree_measures_nonzero_and_counts_the_gateway (#P11 — mem.rs inline replacement)
19. The FINISHED_RING=32 history ring: no test (submitting 33 quick runs costs more than it is worth; the ring shape is guarded indirectly by runs.rs listing semantics and the to_json tests; add one later if this ever tightens)

Total: 12 new integration tests for terminal (all in tests/terminal_ws.rs), 7 for process (6 in tests/plugin_host.rs + 1 mem.rs inline replacement), and 6 new helpers (rig_with/ProviderKind, flood, read_until, slow_input, chatty_input, live_child_pid).
