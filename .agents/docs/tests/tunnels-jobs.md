# Tunnels and Jobs Integration Test Plan (functionality point → test matrix)

> Input: the "Feature Overview" tables of `.agents/docs/tunnels.md` and `.agents/docs/jobs.md` (tunnels 23 rows, jobs 25 rows, every row enters the matrix, none left out); test baseline: root `tests/` (app.rs, adminapi.rs, plugin_host.rs, terminal_ws.rs, env_precedence.rs, envelope_compat.rs, memory.rs, http_adapter.rs, groups_e2e.rs) + the inline `#[cfg(test)]` of each source file in `crates/swiss-tunnels`, `crates/swiss-jobs`. Spec: `docs/08-testing.md`.

## 0. General Rules and Testability Facts (shared by both chapters)

- **Oneshot, no real port, no sleep.** All HTTP assertions drive the axum Router via `tower::ServiceExt::oneshot`, listening on no port; all waiting follows the existing tests/ **bounded short-polling** pattern (20ms × a capped number of rounds, same as `editing_a_config_definition_applies_in_place_without_killing_in_flight_runs` in `tests/plugin_host.rs`); time-semantics assertions (DST/sleep/retry delays) always use an injected clock, never relying on real time passing.
- **Environment iron rules.** Every test file installs a deterministic master key up front: `swiss_core::secure::key::use_test_master_key()` (internally a one-time `set_var(MCP_GATEWAY_MASTER_KEY, "ab".repeat(32))`, equivalent to `test_master_key` in `tests/plugin_host.rs`); neither domain has a database dependency, so gate 2 (swiss-it) does not apply here (stated honestly, no useless branch invented).
- **Injection-seam visibility (decides which layer a test belongs to):**
  - `TunnelManager::with_connections`/`ConnFactory`/`FakeConn` and `clock::testing::FakeClock` are all `#[cfg(test)]` **crate-private** seams — unavailable to root `tests/`. Any behavior test needing a fake SSH connection or synthetic timezones stays in the crate's inline `mod tests` (as today); root `tests/` tests only HTTP wire contracts and composition-layer interplay.
  - Public seams available to root `tests/`: `swiss_tunnels::tunnel::api::Tunnels { store, manager, mcp_display }` and `api::mount` (a bare Router; `tests/plugin_host.rs` already constructs it this way); `TunnelManager::new(store, Option<Box<dyn McpView>>)` (a fake McpView can be injected → the stop/delete guard and mcpRows are testable at root level); `McpDisplay` (names/suggest, the trait at api.rs:34-39); on the jobs side `JobSystem::set_clock(Arc<dyn Clock>)` and `pub trait Clock` (clock.rs:12-16) — if root-level clock injection is needed, the test simply implements Clock itself (the FakeClock itself is not visible).
- **Dial determinism.** Any root-level test that actually exercises `test`/`start` points the SSH host at `127.0.0.1` with a closed port (loopback gives an immediate ECONNREFUSED, which `classify_message` buckets as `network`), never at an unreachable external address (the 15s READY_TIMEOUT would drag the suite down); rule bodies set `autoReconnect:false` explicitly, avoiding leftover reconnect timers.
- **New files:** `tests/tunnels_api.rs` (tunnels HTTP wire contracts + composition-layer interplay) and `tests/jobs_api.rs` (jobs manual gates/record contracts); behavior-level additions go into the inline `mod tests` of `crates/swiss-tunnels/src/tunnel/manager.rs` and `crates/swiss-jobs/src/jobs/api.rs`. Test names align with the existing style (whole-sentence snake_case; assertion style copied from `tests/plugin_host.rs` and the jobs api.rs inline tests).

---

## Chapter 1 Tunnels (swiss-tunnels) Integration Test Plan

### 1.1 Existing Coverage Inventory (test files/modules → test name lists → corresponding functionality points)

**Root tests/**

- `tests/plugin_host.rs`: `boot_disabled_plugins_guard_every_route_they_own` (the plugin-boundary 503 on tunnels' three routes), `enabled_plugins_keep_every_api_shape_they_had` (GET /api/tunnels 200), `inventory_shape_is_exact_and_the_mcp_plugin_started_the_mcps` (descriptor/pages/routes), `every_contributed_page_entry_is_actually_served` (tunnels page assets), `route_ownership_is_longest_prefix_at_segment_boundaries` (route ownership). → For functionality point #22's seat isolation see also `tests/terminal_ws.rs` (tunnels disabled in that rig releases the shell seat, plus its `the_enabled_plugin_lists_targets_and_local_is_off`).
- The remaining root files (app.rs, adminapi.rs, env_precedence.rs, envelope_compat.rs, memory.rs, http_adapter.rs) contain no tunnels assertions; `masks_secrets_in_details_and_keeps_the_stored_value_when_the_mask_returns` in adminapi.rs covers the `swiss_host::mask` sentinel mechanism tunnels shares (the mechanism basis of functionality point #3).

**Crate inline (`crates/swiss-tunnels/src/tunnel/`)**

- `api.rs` tests: `dir_listing_sorts_directories_first`, `dir_listing_reports_errors_not_panics` (#20 browse directory listing), `rule_input_keeps_js_number_semantics` (#6 request-body shaping), `stop_and_delete_accept_the_panels_bodiless_json_post` (#7 panel bodiless-POST compatibility), `fail_funnel_status_codes` (#17 error-funnel mapping).
- `store.rs` tests: `validates_connection_errors_exactly_like_node`, `key_auth_requires_key_path_password_auth_requires_password` (#1/#2 validation), `rule_validation_rejects_gateway_port_and_clashes` (#6 port validation), `update_keeps_enabled_and_mcp_links_and_adopts_host_key` (#1/#4 edit keeps order + hostKey re-adoption), `remove_connection_blocked_while_rules_use_it` (#1 delete guard), `groups_rename_reorder_and_default_group_rules` (#19), `loads_a_plaintext_file_with_node_defaults`, `is_fresh_when_no_file` (#21 freshness check), `round_trips_through_the_sealed_state_file` (#3 persistence), `mcp_link_maintenance` (#18).
- `manager.rs` tests (FakeConn + local echo server pattern, 52 in the module now): 9 start/stop and lifecycle tests (`binds_the_port_carries_traffic_and_frees_the_port_on_stop`, `is_idempotent_a_second_start_is_a_no_op_and_so_is_a_second_stop`, `serializes_a_start_and_a_stop_that_arrive_together`, `dials_once_for_many_rules_and_ends_the_client_when_the_last_one_stops`, `does_not_double_count_a_reference_when_a_rule_is_started_twice`, `close_all_frees_every_port_but_leaves_enabled_alone`, `a_start_that_lands_during_or_after_close_all_is_refused_not_leaked`, `starts_only_enabled_rules_and_never_throws_when_one_fails` (start-all), `collects_every_dependent_into_one_error_for_stop_all` (stop-all 409+force)) → parts of #7/#8, #14 (`a_listener_on_a_dead_client_is_rebuilt_on_start_not_reported_up`), #17 (`blocks_a_stop_while_a_linked_mcp_is_started_and_obeys_force`, `does_not_block_when_the_linked_mcp_is_not_running`); 6 reconnect and failure-classification tests (`releases_the_port_and_schedules_a_reconnect_when_auto_reconnect_is_on`, `does_not_schedule_a_reconnect_for_an_auth_failure`, `leaves_a_rule_in_error_port_released_when_auto_reconnect_is_off`, `does_not_retry_a_port_failure`, `spaces_network_reconnects_out_with_backoff_and_never_gives_up`, `brings_every_rule_on_a_shared_connection_down_together`) → #8/#9/#11; 5 edit and delete tests (`restarts_a_running_rule_on_the_new_port_and_frees_the_old_one`, `leaves_a_stopped_rule_stopped_after_an_edit`, `restarts_only_the_rules_that_were_running_when_a_connection_is_edited`, `refuses_to_delete_a_connection_with_rules_and_succeeds_once_they_are_gone`, `frees_the_port_when_a_running_rule_is_deleted`) → #1/#6; 2 port-diagnostics tests (`reports_the_holder_when_the_local_port_is_taken_by_someone_else`, `fails_a_rule_whose_connection_is_unknown_without_touching_the_port`) → #12/#6; 2 MCP-link tests (`reports_link_state_and_an_unknown_mcp_name_honestly`, `flags_a_stale_pool_only_when_the_reconnect_came_after_the_mcp_started`) → #16; the shell family (`a_shell_takes_a_reference_and_gives_it_back_when_the_session_drops`, `withdrawing_refuses_new_shells_and_names_the_plugin`, `the_target_list_reports_the_live_connection_state`, `a_typed_interrupt_gets_through_while_the_output_queue_is_full` etc.) → #22; the rows()-read-surface test `connection_rows_carry_the_proxy_jump_and_key_path_fields_for_the_panel` — the /api/tunnels connection row carries proxy/proxyUsername/jump/keyPath plaintext absent-when-unset and proxyPassword as the MASK sentinel, appended after the frozen key prefix (docs/27 §4 addendum; keyPath was added so the edit sheet prefills a custom key path without a save rewriting it to the default); the proxy/jump families (`a_proxied_connection_forwards_through_the_manager_lifecycle`, `a_jump_connection_rides_the_jump_servers_channel_end_to_end`, `editing_a_jump_reconnects_its_dependents_and_delete_lists_them`, `testing_a_jump_connection_uses_a_throwaway_chain_and_names_the_hop_on_failure`, `a_two_level_chain_releases_every_hop_when_the_target_stops` etc.); and the remote exec/file-operation family (`a_remote_exec_shares_the_rule_client_and_releases_its_lease`, `file_operations_round_trip_and_release_their_leases`, `the_real_client_execs_against_a_real_russh_server`, `the_real_client_cancels_a_stalled_exec`).
- `ssh.rs` tests: `fingerprint_matches_the_openssl_style_format` (#4), `expand_home_forms` (#2), `classify_sorts_auth_and_hostkey_from_network`, `as_tunnel_error_prefixes_and_classifies` (#9), `reading_a_missing_key_is_config_not_auth` (#2), `a_missing_vault_passphrase_reference_refuses_before_the_dial` (#3 strict semantics).
- `forward.rs` tests: `binds_forwards_and_releases_the_port`, `a_second_binder_gets_the_port_error`, `a_failing_opener_counts_channel_failures_not_tunnel_failures` (part of #23).
- `port.rs` tests: `probe_detects_a_live_listener` (#12), `force_free_refuses_the_gateway_itself` (#13).
- `import.rs` tests: `imports_snake_case_rows_stopped`, `missing_file_and_empty_config_import_nothing`, `default_key_path_is_assigned_when_absent` (#21).
- `mcpmatch.rs` tests: `an_unresolvable_definition_is_unmatchable`, `loopback_hosts_only`, `pg_urls_parse_by_hand`, `ports_read_like_js_number` (#15 rule pure functions).
- `types.rs` tests: `number_coercion_matches_js`, `number_formatting_matches_js_interpolation`, `rule_json_keeps_node_field_order`, `uuids_are_v4_shaped_and_unique` (wire shapes).

### 1.2 Functionality-Point → Test Matrix

| # | Functionality point (audit doc line) | Existing tests | Gap | New test name (english snake_case, proposed location) | Assertion essentials (method+path+status code+JSON fields/shape) | Special handling |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | SSH connection CRUD (tunnels.md:9) | store.rs `validates_connection_errors_exactly_like_node`, `key_auth_requires_key_path_password_auth_requires_password`, `remove_connection_blocked_while_rules_use_it`, `update_keeps_enabled_and_mcp_links_and_adopts_host_key`; manager.rs `restarts_only_the_rules_that_were_running_when_a_connection_is_edited`, `leaves_a_stopped_rule_stopped_after_an_edit`, `refuses_to_delete_a_connection_with_rules_and_succeeds_once_they_are_gone`; plugin_host.rs `enabled_plugins_keep_every_api_shape_they_had`; manager.rs `connection_rows_carry_the_proxy_jump_and_key_path_fields_for_the_panel` (the GET connection-row read surface: proxy/proxyUsername/jump/keyPath plaintext, absent-when-unset; proxyPassword as the MASK sentinel, an env ref rides as itself — docs/27 §4 addendum) | HTTP wire contract: the POST 201 / PUT / DELETE / 404 / 400 full chain untested (validation only tested down to the store layer) | `connections_crud_round_trip_over_http` (tests/tunnels_api.rs) | POST /api/tunnels/connections→201 `{connection:{id,name,host,port,username,authType,group?}}`; missing host→400; PUT /{id} rename→200 `{connection}`; PUT unknown id→404; DELETE→200 `{id,deleted:true}`; DELETE again→404 | bare-mount oneshot (following the api.rs inline `stop_and_delete...` style); the connection host uses 127.0.0.1 |
| 2 | SSH auth key/password (tunnels.md:10) | store.rs `key_auth_requires_key_path_password_auth_requires_password`; ssh.rs `reading_a_missing_key_is_config_not_auth`, `expand_home_forms` | HTTP-layer authType branch error messages (folded into act two of the #1 test) | folded into `connections_crud_round_trip_over_http` (tests/tunnels_api.rs) | authType=key without keyPath→400 and error mentions `keyPath`; authType=password without password→400 mentioning `password`; illegal authType→400 | real private-key parsing / password login cannot be oneshot → section 1.5 |
| 3 | Credential references persisted as ${ENV_VAR} (tunnels.md:11) | ssh.rs `a_missing_vault_passphrase_reference_refuses_before_the_dial` (strict semantics); store.rs `round_trips_through_the_sealed_state_file`; adminapi.rs `masks_secrets_in_details...` (same-origin sentinel mechanism) | the tunnels HTTP trio — masked on the way out / sentinel restored / literal persisted to disk — is untested | `connection_credentials_go_out_masked_and_disk_keeps_the_env_reference` (tests/tunnels_api.rs) | POST password:"${PG_PASS}"→201 and `connection.password == "••••••••"` (`swiss_host::mask::MASK`); GET rows always masked; inside `read_secure_json(tunnels.json)` password == "${PG_PASS}" literal; PUT changing only remark (no password sent)→200, the persisted reference remains | the master-key iron rule; unseal via `swiss_core::secure::statefile::read_secure_json` |
| 4 | TOFU host key (tunnels.md:12) | ssh.rs `fingerprint_matches_the_openssl_style_format`; store.rs `update_keeps_enabled_and_mcp_links_and_adopts_host_key` (re-adoption when host/port unchanged) | POST /trust's 404 and clear branches (HTTP); mismatch→write branch (manager inline) | `trust_endpoint_answers_404_for_unknown_and_clears_without_a_mismatch` (tests/tunnels_api.rs) + `trust_stores_a_presented_fingerprint_and_clears_without_one` (manager.rs inline) | HTTP: POST /connections/{unknown}/trust→404; trust POST on a newly created connection→200 `{id, hostKey:null}`. Inline: preset mismatches then trust→`Ok(Some(fp))` and store hostKey==fp; no mismatch and a key already present→cleared to None | mismatch state is manager-private → the write branch can only be inline (same crate can touch private fields); expected/actual detail needs a real sshd → section 1.5 |
| 5 | Connection test (tunnels.md:13) | none (entirely missing) | POST /test's 404 and failure-classification contract | `connection_test_answers_404_and_classifies_a_refused_local_dial_as_network` (tests/tunnels_api.rs) | POST /connections/{unknown}/test→404; POST test on a connection with host=127.0.0.1, port=<closed port>→200 `{ok:false, kind:"network", ms, error}` (ok false, not a transport error) | a loopback closed port refuses instantly; never an external address (15s timeout) |
| 6 | Local (-L) forward rule CRUD (tunnels.md:14) | store.rs `rule_validation_rejects_gateway_port_and_clashes`; api.rs `rule_input_keeps_js_number_semantics`; manager.rs `restarts_a_running_rule_on_the_new_port_and_frees_the_old_one`, `frees_the_port_when_a_running_rule_is_deleted`; types.rs `rule_json_keeps_node_field_order` | HTTP: POST 201 / start:true failure does not roll back / PUT 404 / port clash 400 | `rules_crud_round_trip_over_http` + `creating_a_rule_with_start_true_saves_it_even_when_the_dial_fails` (both tests/tunnels_api.rs) | POST /api/tunnels/rules→201 `{rule:{id,name,connectionId,localPort,targetHost,targetPort,...}}`; localPort=19998 (the gateway port)→400; duplicate localPort→400; PUT /{id} changing targetPort→200; PUT unknown→404; `start:true` + closed port→201 and `rule.state=="error"`, still present in the GET listing (the save was not rolled back) | the rig's TunnelStore uses 19998 as gateway_port (as plugin_host.rs); autoReconnect:false |
| 7 | Rule start/stop (tunnels.md:15) | manager.rs 9 start/stop tests + `starts_only_enabled_rules_and_never_throws_when_one_fails` (start-all) + `collects_every_dependent_into_one_error_for_stop_all` (stop-all 409/force); api.rs `stop_and_delete_accept_the_panels_bodiless_json_post`, `fail_funnel_status_codes` | HTTP: the start-failure 200+ok:false shape; the start-all/stop-all `{results}` shape | `start_answers_200_with_ok_false_when_the_dial_fails` + `start_all_and_stop_all_answer_the_results_shape_over_http` (tests/tunnels_api.rs) | POST /rules/{id}/start (closed port)→200 `{rule, ok:false, error:"…"}` (not a 5xx); POST /start-all→200 `{results:[{id,name,ok,error?}]}` all ok:false; POST /stop-all→200 `{results:[]}` (no running rules) | the stop-all 409 branch needs running rules (FakeConn) → already covered inline, not duplicated at root level |
| 8 | Auto-reconnect (tunnels.md:16) | manager.rs `releases_the_port_and_schedules_a_reconnect_when_auto_reconnect_is_on`, `spaces_network_reconnects_out_with_backoff_and_never_gives_up` (backoff+cap), `does_not_schedule_a_reconnect_for_an_auth_failure`, `leaves_a_rule_in_error_port_released_when_auto_reconnect_is_off` | none (covered) | — | — | FakeConn inline; bounded wait_until budget (the current pattern) |
| 9 | Non-retryable failure classification (tunnels.md:17) | types/ssh/manager: `classify_sorts_auth_and_hostkey_from_network`, `as_tunnel_error_prefixes_and_classifies`, `does_not_schedule_a_reconnect_for_an_auth_failure`, `does_not_retry_a_port_failure` | none (covered) | — | — | — |
| 10 | Transport-death detection (tunnels.md:18) | indirect: manager.rs `brings_every_rule_on_a_shared_connection_down_together` (die() simulates a transport break) | the real NAT-silence path of keepalive 15s×3 is untestable (see 1.5) | — | — | the inline simulation covers the observable behavior; a real transport → live-verify |
| 11 | Disconnect-recovery semantics (tunnels.md:19) | manager.rs `brings_every_rule_on_a_shared_connection_down_together`, `serializes_a_start_and_a_stop_that_arrive_together`, `a_start_that_lands_during_or_after_close_all_is_refused_not_leaked` | none (covered) | — | — | FakeConn inline |
| 12 | Port-occupation diagnostics (tunnels.md:20) | manager.rs `reports_the_holder_when_the_local_port_is_taken_by_someone_else`; port.rs `probe_detects_a_live_listener` | HTTP GET /port/{port}'s Node-compatible shape (explicit null owner) and 400 | `port_endpoint_reports_free_taken_and_rejects_an_invalid_port` (tests/tunnels_api.rs) | GET /port/<free>→200 `{port, free:true, owner:null}`; GET /port/<a squatted port>→`{free:false, owner:{pid,name}}`; GET /port/0→400 | the squat uses the test's own TcpListener (bind :0, held, not closed); owner.pdi asserted only under `cfg!(windows)` (unix has no netstat+tasklist holder lookup) |
| 13 | Force free (tunnels.md:21) | port.rs `force_free_refuses_the_gateway_itself` | HTTP: free 404 with no holder, 400 on an illegal port, killing the gateway itself refused | `force_free_refuses_to_kill_the_gateway_itself_over_http` (tests/tunnels_api.rs) | POST /port/<free>/free→404; POST /port/0/free→400; POST /port/<a port squatted by the test itself>/free→400 and body contains `error` (the holder is the gateway process itself, refuse-to-kill) | the test process IS the gateway process → self-occupation is a deterministic refuse-to-kill; really killing an external process → section 1.5; unix force_free refuses anyway, assert only the status+error fields |
| 14 | Own stale-listener recycling (tunnels.md:22) | manager.rs `a_listener_on_a_dead_client_is_rebuilt_on_start_not_reported_up` | none (covered) | — | — | FakeConn inline |
| 15 | MCP-match suggest (tunnels.md:23) | mcpmatch.rs 4 pure-function tests | HTTP GET /suggest/{port} shape and 400 | `suggest_matches_mcps_and_rejects_an_invalid_port` (tests/tunnels_api.rs) | GET /suggest/3306 (fake McpDisplay returns ["mysql"])→200 `{port:3306, mcps:["mysql"]}`; GET /suggest/0, /suggest/abc→400 | a fake McpDisplay injected via `Tunnels{mcp_display}`; the matching rules themselves are already inline |
| 16 | MCP-link display (tunnels.md:24) | manager.rs `reports_link_state_and_an_unknown_mcp_name_honestly`, `flags_a_stale_pool_only_when_the_reconnect_came_after_the_mcp_started` | HTTP GET /api/tunnels in-row mcpRows/mcps shape | `rows_carry_the_mcp_link_state_and_names` (tests/tunnels_api.rs) | GET /api/tunnels→200 top-level keys exactly `[connections, rules, ruleGroups, connGroups, mcps]`; with rule.mcps=["echo","ghost"] the row carries `mcpRows:[{name:"echo",state:"up",known:true},{name:"ghost",…,known:false}]` (absent-not-null) | a fake McpView (StartedView) + a fake McpDisplay injected in the same rig; stalePool already inline, not duplicated |
| 17 | rename/delete guard (tunnels.md:25) | manager.rs `blocks_a_stop_while_a_linked_mcp_is_started_and_obeys_force`, `does_not_block_when_the_linked_mcp_is_not_running`, `collects_every_dependent_into_one_error_for_stop_all`; api.rs `fail_funnel_status_codes` | HTTP 409 body wire contract (`{error,dependents,confirmRequired:true}`) + `?force=1`/`{force:true}` resend | `stop_and_delete_guard_answers_409_with_dependents_until_forced` (tests/tunnels_api.rs) | rule mcps=["echo"], fake view is_started("echo")=true: DELETE /rules/{id}→409 + dependents contains "echo" + confirmRequired==true; DELETE ?force=1→200; POST /rules/{id}/stop→409; POST stop body `{"force":true}`→200 `{rule}` | the guard does not require the rule to be running (dependents_of only looks at started MCPs) → no FakeConn needed |
| 18 | MCP rename/forget interplay (tunnels.md:26) | store.rs `mcp_link_maintenance`; manager.rs `reports_link_state...` | composition layer: MCP rename/delete carrying tunnel links through the real mcp plugin | `renaming_and_deleting_an_mcp_carries_the_tunnel_links` (tests/tunnels_api.rs, full-app rig) | full_app (with mcp plugin + echo MCP): rule mcps=["echo"]; POST /api/mcps/echo/rename {name:"echo2"}→GET /api/tunnels row mcps contains "echo2" and not "echo"; after deleting the MCP the row's mcps is empty | needs plugin_host.rs `full_app`-level composition (see 1.4); link migration goes through TunnelLinks in src/mcp_link.rs |
| 19 | Groups and drag ordering (tunnels.md:27) | store.rs `groups_rename_reorder_and_default_group_rules` | HTTP: PUT /order echo-back, the four groups endpoints, 404 unknown list, 400 semantics | `order_and_groups_round_trip_over_http` (tests/tunnels_api.rs) | PUT /order {rules:[id2,id1]}→200 echoing the same order; the GET listing follows the order; PUT /groups/lists (illegal kind)→404; PUT /groups/rules {groups:["a"]}→200; POST /groups/rules/rename {from:"a",to:"b"}→200 `{groups,moved:1}`; rename to default→400; PUT /groups/rules/{id} {group:"b"}→200 `{group:"b"}`; unknown group→400 | — |
| 20 | Private-key browsing (tunnels.md:28) | api.rs `dir_listing_sorts_directories_first`, `dir_listing_reports_errors_not_panics` | HTTP GET /keys, /browse shapes (errors embedded as an error field) | `keys_and_browse_serve_shapes_not_machine_state` (tests/tunnels_api.rs) | GET /api/tunnels/keys→200 `{keys:[{path,name}…], defaultPath:"…"}` (assert shape only, defaultPath a non-empty string); GET /browse?dir=<seeded scratch>→200 `{dir, parent?, entries:[{name,path,dir}]}` directories first in alphabetical order; ?dir=nonexistent→200 and body contains `error` (not a status code) | keys content varies with the machine's ~/.ssh → assert shape only; browse uses a scratch directory the test creates itself (deterministic) |
| 21 | First-run forward-port import (tunnels.md:29) | import.rs 3 tests (conversion semantics + freshness) | boot wiring: at plugin start is_fresh→import (optional, see the special-handling column) | `forward_port_config_imports_on_first_boot_all_disabled` (tests/tunnels_api.rs, P2) | pre-seed scratch %APPDATA%/forward-port/config.json + a full-app boot with no tunnels.json→GET /api/tunnels rules exist and all `enabled:false`; `read_secure_json(tunnels.json)` already on disk | needs set_var APPDATA→scratch: global pollution, must run serialized with other tests or only on Windows; the conversion semantics are already inline, this is a wiring add-on test, deferred by default |
| 22 | Interactive shell provision (tunnels.md:30) | manager.rs shell-family tests (refcount/shared client/refusal-return/withdraw/drain/target listing/typing and resize/registry decoupling/interrupt passthrough); terminal_ws.rs (seat isolation) | none (covered) | — | — | a real remote PTY → live-verify |
| 23 | Live statistics (tunnels.md:31) | forward.rs `a_failing_opener_counts_channel_failures_not_tunnel_failures` (channelFailures); manager.rs `binds_the_port_carries_traffic...` (traffic carrying) | the sockets/bytesIn/bytesOut row fields have never been asserted | `forward_stats_report_sockets_and_both_byte_counters` (manager.rs inline) | FakeConn+echo server: start a rule, after `round_trip(port,"hello")` `row_of`: `bytesIn>=5 && bytesOut>=5 && channelFailures==0`, lastError absent | FakeConn inline (the fields live in row(); the existing helper `row_of` works directly) |

### 1.3 Gap-Test Arrange/Act/Assert Details (tunnels)

The three-part specs below are keyed by matrix row number and ready to start from directly; shared helpers are in 1.4. Every test begins with `use_test_master_key()` (done inside `scratch()`).

**#1/#2 `connections_crud_round_trip_over_http`**
- Arrange: `let (app, dir) = bare_rig("conn-crud").await;`.
- Act 1: POST /api/tunnels/connections, body `conn_body("box")` (`{name,host:"127.0.0.1",port:1,username:"u",authType:"password",password:"p"}`).
- Assert 1: 201; `body["connection"]["name"]=="box"`, `body["connection"]["id"]` a string; `body["connection"].get("password")` equals `json!(swiss_host::mask::MASK)`.
- Act 2: POST the same path missing `host` → Assert 400 and the error message contains `host`; POST `authType:"key"` without `keyPath` → 400 containing `keyPath`; POST `authType:"password"` without `password` → 400 containing `password`; POST `authType:"bogus"` → 400.
- Act 3: PUT /api/tunnels/connections/{id} body same as Act 1 but name=="box2" → Assert 200, `connection.name=="box2"`; PUT /api/tunnels/connections/{a fresh uuid} → 404.
- Act 4: DELETE /{id} → 200 `{id, deleted:true}`; DELETE again → 404.

**#3 `connection_credentials_go_out_masked_and_disk_keeps_the_env_reference`**
- Arrange: `bare_rig("mask")`; the body's password uses `"${TUN_TEST_PASS}"` (no need to actually set that variable — what lands is the reference).
- Act 1: POST connections → Assert 201 and the password field == `MASK`.
- Act 2: GET /api/tunnels → Assert the row `connections[0]["password"]==MASK`.
- Act 3: `read_secure_json(&dir.join("tunnels.json"))` → Assert `["connections"][0]["password"]=="${TUN_TEST_PASS}"` (the reference persisted literally).
- Act 4: PUT /{id} body carrying only name/host/port/username/authType (no password field sent) → Assert 200; unseal again → password is still `${TUN_TEST_PASS}` (sentinel absent = kept).

**#4 HTTP `trust_endpoint_answers_404_for_unknown_and_clears_without_a_mismatch`**
- Arrange: `bare_rig("trust")`; POST a connection and record its id.
- Act 1: POST /api/tunnels/connections/{uuid::new}/trust → Assert 404.
- Act 2: POST /{id}/trust → Assert 200 `{id, hostKey:null}` (no mismatch presenting a key means the clear semantics).

**#4 inline `trust_stores_a_presented_fingerprint_and_clears_without_one` (manager.rs tests)**
- Arrange: scratch()+manager(FakeConn); `add_conn` and record the id; `m.mismatches.lock().unwrap().insert(id.clone(), "SHA256:abc".into());`.
- Act 1: `m.trust_host_key(&id)` → Assert `Ok(Some("SHA256:abc"))` and — rather than `stored_rule`… — via `m.with_store(|s| s.connection(&id))` assert `host_key==Some("SHA256:abc")`.
- Act 2: trust again (the mismatch is consumed) → `Ok(None)` and store host_key==None (cleared).

**#5 `connection_test_answers_404_and_classifies_a_refused_local_dial_as_network`**
- Arrange: `bare_rig("test")`; first `let port = free_port().await;` (grabbed then dropped, guaranteeing it is closed); POST a connection with host=127.0.0.1, port=port and record the id.
- Act 1: POST /connections/{a fresh uuid}/test → 404.
- Act 2: POST /connections/{id}/test → Assert 200, `body["ok"]==false`, `body["kind"]=="network"`, `body["error"]` a string, `body["ms"]` a number (fails fast).

**#6 `rules_crud_round_trip_over_http`**
- Arrange: `bare_rig("rule-crud")`; create a connection and record conn_id; `let local = free_port().await;`.
- Act 1: POST /api/tunnels/rules body `rule_body("pg", &conn_id, local)` (+autoReconnect:false) → Assert 201, `rule["localPort"]==local`, `rule["enabled"]==false`.
- Act 2: POST localPort==19998 → 400; POST with the same localPort as Act 1 → 400 (port clash).
- Act 3: PUT /rules/{id} changing targetPort → 200; PUT /rules/{a fresh uuid} → 404; DELETE → 200 `{id,deleted:true}`.

**#6 `creating_a_rule_with_start_true_saves_it_even_when_the_dial_fails`**
- Arrange: as above, the connection's port pointing at a closed port; the body adds `"start": true`.
- Act: POST → Assert 201, `rule["state"]=="error"`; GET /api/tunnels → the rule is still in the listing (a failure does not roll back the save).

**#7 `start_answers_200_with_ok_false_when_the_dial_fails`**
- Arrange: create a connection (closed port) + a rule (enabled:false, autoReconnect:false).
- Act: POST /api/tunnels/rules/{id}/start → Assert **200** (failure is a result, not a transport error), `body["ok"]==false`, `body["error"]` a string, `body["rule"]["id"]==id`.

**#7 `start_all_and_stop_all_answer_the_results_shape_over_http`**
- Arrange: as above (1 connection + 1 rule, not started).
- Act 1: POST /api/tunnels/start-all → Assert 200 `{results:[{id,name,ok:false,error}]}` (dial failures land in results).
- Act 2: POST /api/tunnels/stop-all → Assert 200 `{results:[]}` (no running rules). (The stop-all 409 branch is held by the inline `collects_every_dependent_into_one_error_for_stop_all`.)

**#12 `port_endpoint_reports_free_taken_and_rejects_an_invalid_port`**
- Arrange: `bare_rig("port")`; `let (l, taken) = squat().await;` (holding the listener).
- Act 1: GET /api/tunnels/port/{free_port} → 200 `{port, free:true, owner:null}` (Node-compatible: explicit null, not absent).
- Act 2: GET /port/{taken} → `free:false`; under `#[cfg(windows)]` `owner` is `{pid,name}` and `pid==std::process::id()`.
- Act 3: GET /port/0 → 400.

**#13 `force_free_refuses_to_kill_the_gateway_itself_over_http`**
- Arrange: as above (the squat lives in the test process).
- Act 1: POST /port/{free_port}/free → 404 (no holder).
- Act 2: POST /port/0/free → 400.
- Act 3: POST /port/{taken}/free → Assert 400, body contains `error` (the holder is the gateway's own process, refuse-to-kill; the owner shape is not asserted, Windows/Unix wording differs).

**#15 `suggest_matches_mcps_and_rejects_an_invalid_port`**
- Arrange: `rig_with_mcps("suggest", &[])` (the display fixed to return ["mysql"]).
- Act 1: GET /api/tunnels/suggest/3306 → 200 `{port:3306, mcps:["mysql"]}`.
- Act 2: GET /suggest/0, /suggest/abc → 400.

**#16 `rows_carry_the_mcp_link_state_and_names`**
- Arrange: `rig_with_mcps("rows", &["echo"])`; create a connection + rule, the rule body `mcps:["echo","ghost"]`.
- Act: GET /api/tunnels → Assert the top-level key set is exactly `{"connections","rules","ruleGroups","connGroups","mcps"}`; the rule row's `mcpRows` == `[{name:"echo",state:"up",known:true},{name:"ghost",state:"stopped",known:false}]` (state comes from the fake view; ghost unknown → known:false); top-level `mcps` == the fake display's list.

**#17 `stop_and_delete_guard_answers_409_with_dependents_until_forced`**
- Arrange: `rig_with_mcps("guard", &["echo"])` (StartedView makes is_started("echo")==true); create a connection + rule mcps:["echo"].
- Act 1: DELETE /api/tunnels/rules/{id} → Assert 409, `body["dependents"]` contains "echo", `body["confirmRequired"]==true`.
- Act 2: DELETE /rules/{id}?force=1 → 200 `{id,deleted:true}`.
- Act 3: create the same rule again; POST /rules/{id}/stop → 409 with the same body; POST /rules/{id}/stop body `{"force":true}` → 200 `{rule}` (both spellings `?force=1` and `{force:true}` tested once each).

**#18 `renaming_and_deleting_an_mcp_carries_the_tunnel_links`**
- Arrange: full-app rig (1.4's `full_tunnels_app`, with the real mcp plugin and a registered echo MCP); create a connection + rule mcps:["echo"].
- Act 1: POST /api/mcps/echo/rename body `{"name":"echo2"}` → GET /api/tunnels → the rule row's mcps contains "echo2" and not "echo".
- Act 2: delete that MCP (DELETE /api/mcps/echo2) → the rule row's mcps is an empty array (links point at no dead name).

**#19 `order_and_groups_round_trip_over_http`**
- Arrange: `bare_rig("groups")`; create 2 rules (id1, id2).
- Act 1: PUT /api/tunnels/order body `{"rules":[id2,id1]}` → 200 echoing `{"rules":[id2,id1]}`; the GET listing's rules order == [id2,id1]; PUT body `{"rules":"x"}` → 400.
- Act 2: PUT /api/tunnels/groups/lists → 404 (unknown list); PUT /groups/rules `{"groups":["a"]}` → 200 `{"groups":["a"]}`.
- Act 3: POST /groups/rules/rename `{"from":"a","to":"b"}` → 200 `{"groups":["b"],"moved":0}`; renaming to "default" → 400; renaming from a missing group → 400.
- Act 4: PUT /groups/rules/{id1} `{"group":"b"}` → 200 `{"group":"b"}`; PUT `{"group":"zz"}` → 400; PUT /groups/conns/{id} → 200 (connection groups share the same path).

**#20 `keys_and_browse_serve_shapes_not_machine_state`**
- Arrange: `bare_rig("keys")`; under scratch create `zed/`, `abc/`, `b.txt`, `a.key`.
- Act 1: GET /api/tunnels/keys → 200; `defaultPath` a non-empty string; every element of the `keys` array has string `path`/`name` (the concrete list is not asserted).
- Act 2: GET /api/tunnels/browse?dir={scratch_urlencoded} → 200, `entries` order == [abc(dir),zed(dir),a.key,b.txt] directories first, `parent` is Some.
- Act 3: GET /browse?dir={a nonexistent absolute path} → 200 and the body contains an `error` field, `entries==[]` (the error is embedded, not promoted to a status code).

**#21 (P2) `forward_port_config_imports_on_first_boot_all_disabled`**
- Arrange: a full-app rig but without pre-writing tunnels.json; fake up `forward-port/config.json` under scratch (one snake_case rule); `std::env::set_var("APPDATA", scratch)` (serial guard, see the matrix's special-handling column).
- Act: boot (GET /api/tunnels) → Assert the imported rule exists and `enabled==false`; `read_secure_json(tunnels.json)` exists.
- Teardown: restore APPDATA.

**#23 (inline) `forward_stats_report_sockets_and_both_byte_counters`**
- Arrange: the manager.rs tests' existing rig (FakeConn + `echo_server()`); add a rule and start it; `round_trip(port, "hello").await`.
- Act: `let row = row_of(&m, &rule_id);`.
- Assert: `row["bytesIn"].as_i64() >= 5`, `row["bytesOut"].as_i64() >= 5`, `row["channelFailures"]==json!(0)`, `row.get("lastError").is_none()`.

### 1.4 New Test Helpers Needed (tunnels, signatures given)

`tests/tunnels_api.rs` (each file carries its own helpers, following the adminapi.rs/plugin_host.rs convention):

```rust
fn scratch(tag: &str) -> std::path::PathBuf;
// use_test_master_key() + temp_dir/swiss-tunnels-{tag}-{random_hex(8)} + create_dir_all

fn bare_rig(tag: &str) -> (axum::Router, std::path::PathBuf);
// TunnelStore::new(dir/tunnels.json, 19998) -> TunnelManager::new(store, None)
// -> api::mount(Arc::new(Tunnels { store, manager, mcp_display: None }))

fn rig_with_mcps(tag: &str, started: &[&str]) -> (axum::Router, std::path::PathBuf);
// same as above, but TunnelManager::new(store, Some(Box::new(StartedView::new(started))))
// and mcp_display: Some(Arc::new(FixedDisplay { names: vec!["mysql"] }))

struct StartedView { names: Vec<String> }          // impl McpView
struct FixedDisplay { names: Vec<String> }         // impl McpDisplay: names()/suggest(_)->names

async fn full_tunnels_app(tag: &str) -> (axum::Router, std::path::PathBuf);
// copied verbatim from full_app_with_store in tests/plugin_host.rs (register echo MCP + builtin::register_all
// + build_app), for the #18 composition-layer test; returns the scratch dir for unseal assertions

async fn call(router: &axum::Router, method: &str, uri: &str, body: Option<Value>)
    -> (StatusCode, Value);
// modeled on call() from the crates/swiss-jobs/src/jobs/api.rs tests: oneshot + to_bytes + serde

fn conn_body(name: &str) -> Value;   // password auth, host 127.0.0.1, port 1
fn rule_body(name: &str, conn_id: &str, local: u16) -> Value; // autoReconnect:false, mcps can be added
async fn free_port() -> u16;         // bind 127.0.0.1:0 -> local_addr().port() -> drop
async fn squat() -> (tokio::net::TcpListener, u16); // bind :0 and hold it (holder = the test process)
```

`crates/swiss-tunnels/src/tunnel/manager.rs` inline: reuse the existing `scratch/manager/add_conn/add_rule/row_of/echo_server/round_trip`, no new helpers.

### 1.5 Points Not Suitable for Integration Tests, and Alternatives

- **#2 real private-key parsing (`russh::keys::decode_secret_key`) and password authentication, #4 TOFU first-connect learning and expected/actual mismatch detail, #10 the NAT-silent death of keepalive 15s×3, #8/#11's real transport-disconnect shapes**: need a real sshd. Alternative: classification/fingerprint/strict references are already inline (ssh.rs); observable behavior is simulated with FakeConn `die()` (as today); end-to-end verification goes through live-verify on the 19998 instance started by `scripts/test-instance.ps1` + a local/container sshd (following the swiss-live-verify skill), not into the cargo suite.
- **#13 force free killing a real external process**: the only safe victim (a child process the test builds itself) needs its own port-holding helper, cost above value; refusing to kill itself is already covered by the inline `force_free_refuses_the_gateway_itself` + the root-level #13 test. The kill path → 19998 live-verify.
- **#22 a real SSH PTY remote**: the 12 inline shell tests + terminal_ws.rs already cover the contract; a real PTY → live-verify.
- **Panel interactions (drag-order drop points, collapse persistence, polling patches, the 409 confirm-flow UI)**: DOM behavior is not drivable from the Rust oneshot suites → this repo's own vitest in crates/swiss-panel/panel/test/ (ADR-024 — admin-tunnels-pages.test.ts, admin-tunnel-advanced.test.ts; keyPath/advanced-sheet round-trips included); this repo tests only the API wire contracts backing them (matrix #17/#19).
- **#21's %APPDATA% environment read**: a global set_var cross-talks between tests, root-level tests need a serial guard; the conversion semantics are already inline, the boot-wiring test is listed as a P2 option.

### 1.6 Priorities

- **P0 (security and wire contracts)**: #3 (credential masking + the reference persisted to disk), #17 (the 409 dependents structured-confirm contract + both force spellings), #1/#6 (CRUD status codes and response shapes).
- **P1 (main paths)**: #5, #7 (200 ok:false and the results shape), #12, #15, #16, #18, #19, #23 (inline statistics), #4 (HTTP trust).
- **P2 (edge cases)**: #20 (keys/browse shapes), #13 (force free's three branches), #21 (import boot wiring, serial guard), #2's real-auth path (live-verify).

---

## Chapter 2 Jobs (swiss-jobs) Integration Test Plan

### 2.1 Existing Coverage Inventory (test files/modules → test name lists → corresponding functionality points)

**Root tests/plugin_host.rs** (12 jobs-related):

`boot_disabled_plugins_guard_every_route_they_own` (/api/jobs×4 routes' 503 boundary → #25), `enable_disable_over_the_api_is_live_and_repeatable` (enable/disable liveness round trip → #25), `config_api_validates_before_persisting_and_serves_the_schema` (#1), `jobs_config_put_is_validated_against_the_v2_model` (#1: dotted-path errors, S5 semantics persisted), `a_stale_jobs_config_revision_is_a_conflict` (#1 revision CAS), `a_placeholder_definition_boots_lenient_but_a_manual_save_is_strict` (#1 boot dual-track), `a_v1_jobs_table_migrates_at_boot_and_a_reboot_is_a_no_op` (#7), `a_failed_migration_fails_the_plugin_and_never_schedules` (#7), `editing_a_config_definition_applies_in_place_without_killing_in_flight_runs` (#1/#25 in-place apply), `an_unregistered_action_saves_with_a_warning_and_refuses_to_run` (#23), `disabling_the_process_plugin_withdraws_its_capabilities_but_keeps_the_history` (#23/#25), `the_execution_surface_lists_capabilities_and_runs_them`, `a_submission_refusal_is_typed_and_leaves_no_run_behind`, `a_long_run_is_cancelable_over_the_api_and_reaped_before_the_answer` (#15 host gate).

**Crate inline (`crates/swiss-jobs/src/jobs/`)**

- `api.rs` tests (9): `the_panel_tab_is_part_of_the_copied_tree` (#24), `put_list_delete_round_trip` (#6), `the_listing_carries_the_frozen_v1_shape_and_the_v2_fields` (#6/#19 listing shape), `v1_put_on_a_v2_only_job_is_a_409_pointing_at_the_config_editor` (#6), `invalid_definitions_are_400_with_a_reason` (#6/#1), `a_save_that_never_reached_disk_is_a_500_not_a_success` (#6 write_status), `a_manual_run_returns_the_record_and_history_shows_it` (#5/#16/#18), `an_async_manual_run_is_a_202_whose_record_settles_later` (#17), `the_history_cursor_parameter_walks_the_same_pages_as_before` (#18 cursor).
- `mod.rs` tests (30): `definitions_validate_at_every_entry_point` (#6), `the_store_seals_round_trip_and_drops_invalid_entries`, `absent_enabled_means_true`, `interval_occurrences_anchor_on_last_run_or_boot` (#3, including firstRun:immediate anchoring at 0), `cron_occurrences_count_matching_minutes_and_never_the_same_one_twice` (#4/#12 idempotency key), `a_manual_definition_never_auto_fires_and_has_no_next_due` (#2), `config_definitions_drive_the_due_set` (#8/#9), `apply_config_moves_capacity` (#21), `a_manual_run_claims_executes_records_and_persists` (#5/#15/#20), `an_unknown_job_is_unknown_and_a_claimed_job_is_busy` (#16 404/409 system-level), `a_missing_capability_is_reported_recorded_and_listed` (#23), `a_full_pool_refuses_the_occurrence_visibly` (#21), `a_wedged_command_is_killed_at_the_job_deadline` (#15), `shutdown_cancels_the_runs_this_scheduler_owns` (#25), `a_config_write_that_cannot_persist_is_an_error_not_a_warning`, `v1_edits_never_touch_run_state_and_delete_forgets_it` (#6/#20), `a_job_view_reports_running_a_next_due_and_the_v2_fields` (#19 view), `deleting_a_definition_keeps_its_in_flight_run_and_history` (#6/#18), `v1_edit_of_a_v2_only_definition_is_refused` (#6), `a_spring_forward_gap_minute_is_skipped_not_waited_for`, `a_fall_back_repeated_minute_runs_once_by_occurrence_key` (#12), `misfire_skip_summarizes_and_never_catches_up`, `misfire_run_once_catches_up_exactly_one` (#11), `overlap_skip_records_and_moves_the_anchor`, `overlap_queue_one_queues_and_a_full_queue_skips_visibly` (#13), `retry_runs_to_max_attempts_inside_one_occurrence`, `a_timeout_the_policy_does_not_name_is_final`, `a_canceled_attempt_never_retries`, `stopping_mid_delay_aborts_the_occurrence` (#14), `the_tick_is_a_map_scan_not_a_calendar_walk` (#8/#9, FakeClock.local_calls budget). All inject FakeClock via `set_clock` (independent of DST/sleep and the machine timezone).
- `def.rs` tests (20): `the_spec_example_parses_with_documented_defaults`, `absent_fields_mean_the_documented_defaults`, `schema_version_refuses_anything_but_two`, `config_level_fields_are_bounds_and_type_checked_with_paths`, `definition_level_fields_are_bounds_and_type_checked_with_paths`, `numbers_reject_strings_fractions_and_negatives_but_tolerate_js_spelling`, `unknown_fields_are_rejected_listing_the_layers_legal_fields`, `triggers_parse_or_are_refused_with_a_reason`, `a_total_deadline_over_24_hours_is_refused` (#14), `s5_semantics_parse_and_round_trip`, `definitions_have_a_count_cap_and_id_rules`, `v1_definitions_round_trip_through_v2_losslessly`, `v2_only_definitions_project_read_only_v1_rows`, `boot_parse_drops_old_placeholders_but_a_save_stays_strict`, `action_input_is_kept_verbatim_with_env_refs` (#1/#22 references verbatim).
- `schedule.rs` tests (8): cron parsing/evaluation/DOM-DOW OR/next_after/annual give-up (#4).
- `state.rs` tests (7): sealed round trip/ISO timestamps absent-not-null/bad file empty-state/missing file/write failure warn/forget/field-by-field downgrade (#20).
- `runlog.rs` tests (9): illegal-name refusal/seq increments and continues across restarts/newest-first pagination/absent-not-null/byte-budget trimming/age sweep/empty page/instance isolation/bounded read-through (#18).
- `migrate.rs` tests (7): fresh no-op/migration persisted/three-in-a-row convergence/crash-midpoint convergence/conflict config wins/bad rows dropped/unwritable failure leaves v1 untouched (#7).
- `runner.rs`: no tests (pure shape, indirectly pinned by the mod/api tests).
- Supporting surface (host side): inline tests in `swiss-host/src/services/actions.rs` (strict/lenient refs, masking, legacy tokenizer, typed validation, cancel) and inline tests in `runs.rs` (single terminal state, visible capacity refusal, queue-one, owner-scoped cancel, deadline→timed-out, non-cancelable refusal, panic isolation) → #15.

### 2.2 Functionality-Point → Test Matrix

| # | Functionality point (audit doc line) | Existing tests | Gap | New test name (proposed location) | Assertion essentials | Special handling |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | v2 job definitions (jobs.md:9) | def.rs 17; plugin_host `jobs_config_put_is_validated_against_the_v2_model`, `config_api_validates_before_persisting_and_serves_the_schema`, `a_stale_jobs_config_revision_is_a_conflict`, `a_placeholder_definition_boots_lenient_but_a_manual_save_is_strict` | none (covered) | — | — | dotted paths/CAS/boot dual-track all tested |
| 2 | Trigger manual (jobs.md:10) | mod.rs `a_manual_definition_never_auto_fires_and_has_no_next_due`; def.rs triggers | none (covered) | — | — | — |
| 3 | Trigger interval (jobs.md:11) | mod.rs `interval_occurrences_anchor_on_last_run_or_boot` (including firstRun:immediate); def.rs bounds | none (covered) | — | — | FakeClock inline |
| 4 | Trigger cron (jobs.md:12) | schedule.rs 9; def.rs `triggers_parse_or_are_refused_with_a_reason` (timezone local only); mod.rs `cron_occurrences...` | none (covered) | — | — | compile-once reuse is a performance property, backstopped by the next-due budget assertion |
| 5 | One-shot trigger (jobs.md:13) | mod.rs `a_manual_run_claims_executes_records_and_persists`; api.rs `a_manual_run_returns_the_record_and_history_shows_it` | none (covered) | — | — | — |
| 6 | v1-compatible edit path (jobs.md:14) | api.rs `put_list_delete_round_trip`, `the_listing_carries_the_frozen_v1_shape_and_the_v2_fields`, `v1_put_on_a_v2_only_job_is_a_409_pointing_at_the_config_editor`; mod.rs `v1_edits_never_touch_run_state_and_delete_forgets_it`, `v1_edit_of_a_v2_only_definition_is_refused` | none (covered) | — | — | — |
| 7 | v1→v2 migration (jobs.md:15) | migrate.rs 7; plugin_host `a_v1_jobs_table_migrates_at_boot_and_a_reboot_is_a_no_op`, `a_failed_migration_fails_the_plugin_and_never_schedules` | none (covered) | — | — | — |
| 8 | Scheduler tick (jobs.md:16) | mod.rs `the_tick_is_a_map_scan_not_a_calendar_walk`, `config_definitions_drive_the_due_set` | none (covered) | — | — | FakeClock inline |
| 9 | next-due table (jobs.md:17) | mod.rs the same two tests above (local_calls budget) + `a_job_view_reports_running_a_next_due_and_the_v2_fields` | none (covered) | — | — | — |
| 10 | Injected clock (jobs.md:18) | clock.rs FakeClock is used throughout by the mod.rs DST/misfire/retry tests; plugin_host `editing_a_config...` (real clock, in-place apply) | none (the mechanism itself); the root-level clock-injection seam is unused (recorded honestly) | (optional, P2) `an_interval_occurrence_fires_through_the_real_plugin_boot_under_an_injected_clock` (tests/jobs_api.rs) | before the full-app boot `jobs.set_clock(Arc::new(TestClock(AtomicI64)))` + advance → GET /api/jobs row running/lastRunAt changes | TestClock implements the pub trait `Clock` itself (FakeClock not visible); not done by default — the inline tests already cover the semantics, this is an optional composition-layer add-on |
| 11 | misfire recovery (jobs.md:19) | mod.rs `misfire_skip_summarizes_and_never_catches_up`, `misfire_run_once_catches_up_exactly_one` | none (covered) | — | — | FakeClock inline |
| 12 | DST semantics (jobs.md:20) | mod.rs `a_spring_forward_gap_minute_is_skipped_not_waited_for`, `a_fall_back_repeated_minute_runs_once_by_occurrence_key` | none (covered) | — | — | FakeClock synthesizes DST rules, independent of the machine timezone |
| 13 | overlap policy (jobs.md:21) | mod.rs `overlap_skip_records_and_moves_the_anchor`, `overlap_queue_one_queues_and_a_full_queue_skips_visibly` | none (covered) | — | — | — |
| 14 | Retry policy (jobs.md:22) | mod.rs 4 retry tests; def.rs `a_total_deadline_over_24_hours_is_refused` | none (covered) | — | — | — |
| 15 | runner execution (jobs.md:23) | actions.rs/runs.rs inline (strict/lenient refs, masking, tokenizer, cancel, deadline); mod.rs `a_wedged_command_is_killed_at_the_job_deadline`; plugin_host `the_execution_surface_lists_capabilities_and_runs_them`, `a_long_run_is_cancelable...`, `a_submission_refusal...` | none (covered) | — | — | held by host-side inline tests |
| 16 | Manual-run sync gate (jobs.md:24) | api.rs `a_manual_run_returns_the_record_and_history_shows_it`; mod.rs `an_unknown_job_is_unknown_and_a_claimed_job_is_busy` (system-level) | **HTTP-level 409 Busy** (the label gate's wire contract) | `a_busy_label_answers_409_while_the_same_job_runs` (tests/jobs_api.rs) | start a long task through the async gate first → poll to running==true → POST /api/jobs/{name}/run (sync by default) → 409 and the error mentions busy/label | long command cfg!(windows) `ping -n 20 127.0.0.1` else `sleep 10` (following the plugin_host precedent); finish by reaping via /api/runs/{id}/cancel |
| 17 | Manual-run async gate (jobs.md:25) | api.rs `an_async_manual_run_is_a_202_whose_record_settles_later` | none (covered) | — | — | — |
| 18 | Run log runlog (jobs.md:26) | runlog.rs 10; api.rs `the_history_cursor_parameter_walks_the_same_pages_as_before` | HTTP-level limit clamping (1..100) untested; illegal-name 404 untested | `history_limit_clamps_and_keeps_the_newest_page` + `runs_for_an_illegitimate_name_is_a_404` (tests/jobs_api.rs) | run the same job synchronously 3 times (fast echo) → GET runs?limit=0 → exactly 1 row; ?limit=100 → 3 rows and newest first; GET /api/jobs/{bad..name}/runs → 404 | real child processes ×3 (each <100ms); no sleep, sequential await |
| 19 | Run-record shape (jobs.md:27) | api.rs manual-run tests (record fields); mod.rs refused-shape assertions, `a_job_view_reports_running...` | none (covered) | — | — | the single-point ran_record guards against drift |
| 20 | Run-state file (jobs.md:28) | state.rs 7 | none (covered) | — | — | — |
| 21 | Capacity drive (jobs.md:29) | mod.rs `apply_config_moves_capacity`, `a_full_pool_refuses_the_occurrence_visibly` (scheduler gate); runs.rs `capacity_is_a_visible_refusal_not_a_growing_task_set` | **HTTP manual gate 429** | `a_full_pool_answers_429_and_names_the_capacity` (tests/jobs_api.rs) | boot config maxConcurrentRuns:1, maxQueuedRuns:0; start slow async to fill up → poll to running → POST /api/jobs/quick/run → 429 and the error contains capacity; GET /api/jobs quick row actionAvailable==true | wrap up by canceling slow; history not asserted (the record semantics of a manual capacity refusal are not promised) |
| 22 | Output capture (jobs.md:30) | def.rs `s5_semantics_parse_and_round_trip` (parsing); actions.rs `a_resolved_reference_is_masked_out_of_captured_output` (masking); process.rs inline (bounded tail read) | **capture:none runtime stripping has no test** (the output/chars/preview three fields) | `capture_none_records_the_run_without_output_chars_or_preview` (tests/jobs_api.rs) | boot config defines manual+legacy echo, output.capture="none" → POST run → 200 `run.ok==true` and both run and the GET runs row have `get("output")/get("chars")/get("preview")` all absent (absent-not-null); the control group, a job with the default tail, has output present | echo command cfg!(windows) `cmd /c echo hi` else `echo hi` (the same `echo_cmd` as actions.rs) |
| 23 | Action availability (jobs.md:31) | plugin_host `an_unregistered_action_saves_with_a_warning_and_refuses_to_run`, `disabling_the_process_plugin_withdraws_its_capabilities_but_keeps_the_history`; mod.rs `a_missing_capability_is_reported_recorded_and_listed` | none (covered) | — | — | — |
| 24 | Panel Jobs page (jobs.md:32) | api.rs `the_panel_tab_is_part_of_the_copied_tree`; plugin_host `every_contributed_page_entry_is_actually_served`, `inventory_shape...` | none (covered) | — | — | panel behavior = this repo's vitest (crates/swiss-panel/panel/test/, admin-jobs-v2.test.ts — ADR-024) |
| 25 | Lifecycle (jobs.md:33) | plugin_host `enable_disable_over_the_api_is_live_and_repeatable`, `boot_disabled_plugins_guard_every_route_they_own`; mod.rs `shutdown_cancels_the_runs_this_scheduler_owns`, `deleting_a_definition_keeps_its_in_flight_run_and_history` | none (covered) | — | — | — |

### 2.3 Gap-Test Arrange/Act/Assert Details (jobs)

**#16 `a_busy_label_answers_409_while_the_same_job_runs`**
- Arrange: `let (app, dir) = full_jobs_app("busy", json!({"plugins":{"jobs":{"config":{"definitions":{"slow":{"trigger":{"kind":"manual"},"action":{"type":"process.legacy-command","input":{"command": long_cmd()}},"timeoutMs":30000}}}}}}})).await;` (`long_cmd()`: windows `ping -n 20 127.0.0.1` / unix `sleep 10`).
- Act 1: POST /api/jobs/slow/run body `{"async":true}` → 202 `{runId}`; poll GET /api/jobs until `jobs[0]["running"]==true` (20ms×300, following the plugin_host style).
- Act 2: POST /api/jobs/slow/run (empty body, the sync gate) → Assert **409**, the error message contains `slow` (the label gate).
- Teardown: POST /api/runs/{runId}/cancel (await the reap) → no child process left behind for later tests.

**#21 `a_full_pool_answers_429_and_names_the_capacity`**
- Arrange: `full_jobs_app("capacity", …maxConcurrentRuns:1, maxQueuedRuns:0, definitions:{slow (manual+long command), quick (manual+echo)})`.
- Act 1: POST /api/jobs/slow/run `{"async":true}` → 202; poll until slow is running.
- Act 2: POST /api/jobs/quick/run → Assert **429**, the error contains `capacity`; GET /api/jobs → the quick row `actionAvailable==true` (capacity ≠ missing capability).
- Teardown: cancel slow's runId.

**#22 `capture_none_records_the_run_without_output_chars_or_preview`**
- Arrange: `full_jobs_app("capture", …definitions:{masked:{trigger manual, action legacy `cmd /c echo hi`/unix `echo hi`, output:{capture:"none"}}, talky:{same command, no output (default tail)}})`.
- Act 1: POST /api/jobs/masked/run → Assert 200, `run["ok"]==true`, `run.get("output").is_none() && run.get("chars").is_none() && run.get("preview").is_none()` (absent-not-null).
- Act 2: GET /api/jobs/masked/runs → `runs[0]` has the same three fields absent.
- Act 3 (control): POST /api/jobs/talky/run → 200 and `run["output"]` a non-empty string (proving the stripping is a policy, not global).

**#18 `history_limit_clamps_and_keeps_the_newest_page` + `runs_for_an_illegitimate_name_is_a_404`**
- Arrange: `full_jobs_app("history", …definitions:{tick:{trigger manual, action legacy echo}})`.
- Act 1: three consecutive POST /api/jobs/tick/run (sync, sequential await, no sleep).
- Act 2: GET /api/jobs/tick/runs?limit=0 → Assert 200, `runs` exactly 1 row (lower clamp 1); ?limit=100 → 3 rows, `runs[0]["seq"]` largest (newest first); when exhausted, no `nextBefore`, or a valid decreasing one.
- Act 3: GET /api/jobs/{bad..name}/runs → 404 (the name is validated before pagination).

**(Optional P2) #10 `an_interval_occurrence_fires_through_the_real_plugin_boot_under_an_injected_clock`**
- Arrange: a `full_jobs_app` variant: after constructing `JobSystem::open` but before `host.start_enabled()`, `jobs.set_clock(Arc::new(TestClock::at(t0)))`; define an interval everyMs:60000, firstRun:"immediate".
- Act: advance the `TestClock` (AtomicI64 store to t0+61s) and wait for a tick (bounded polling of GET /api/jobs until lastRunAt appears) → Assert the occurrence fired exactly once, lastOk==true.
- TestClock: a self-implemented `swiss_jobs::jobs::clock::Clock` (`now_ms/local_from_ms/ms_from_local` pass through as UTC, no DST rules).

### 2.4 New Test Helpers Needed (jobs, signatures given)

`tests/jobs_api.rs`:

```rust
fn long_cmd() -> &'static str;     // cfg!(windows) "ping -n 20 127.0.0.1" : "sleep 10"
fn echo_cmd() -> &'static str;     // cfg!(windows) "cmd /c echo hi"     : "echo hi"

async fn full_jobs_app(tag: &str, raw: Value) -> (axum::Router, std::path::PathBuf);
// copied verbatim from full_app_with_store in tests/plugin_host.rs (echo MCP registration + builtin::register_all
// + build_app + tunnels/jobs mount); returns before boot, so the #10 variant can insert set_clock

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Option<Value>, String); // same as plugin_host
fn local(method: &str, uri: &str) -> Request<Body>;
fn json_body(method: &str, uri: &str, body: Value) -> Request<Body>;
async fn await_running(app: &Router, name: &str) -> u64; // 20ms×300 poll for running==true, panic on failure
async fn await_run(app: &Router, run_id: u64) -> Value;  // modeled on plugin_host.rs await_run (polls to a terminal state)

struct TestClock(std::sync::atomic::AtomicI64);          // impl Clock, UTC pass-through (only the optional #10 test)
```

`crates/swiss-jobs/src/jobs/api.rs` inline: no new helpers (`scratch_system/call` already exist; #18's limit clamping sits at root level because it needs 3 real command runs, though the inline tests could reuse the same approach).

### 2.5 Points Not Suitable for Integration Tests, and Alternatives

- **The real one-second tick interval, real recovery after sleep, a real DST switch**: the docs/08 iron rule (vi.useFakeTimers → an injected clock) is already covered inline by FakeClock (#8/#10/#11/#12); root level never does real-clock waiting.
- **The server wiring of the hourly retention sweep (server.rs sweep_run_logs) and cross-file maxHistoryBytes**: the sweep function is already inline (`expired_entries_age_out_on_a_sweep`); the boot/hourly wiring lives inside run_gateway, unreachable by oneshot → 19998 live-verify; `maxHistoryBytes` is unimplemented as a recorded exception (the jobs.md inconsistencies list), no test invented.
- **All panel Jobs-page interactions** (cronstrue as validation, the form's two views, Run now's 30-minute polling cap, structured-signature patches): DOM behavior is not drivable from the Rust suites → this repo's own vitest in crates/swiss-panel/panel/test/ (admin-jobs-v2.test.ts — ADR-024); this repo's API surface is already covered by the api.rs inline tests + plugin_host.
- **Process-tree reaping / Job Object details (the #15 foundation)**: held inline by swiss-host `process.rs`/`runs.rs`; the daemon-level StopResult::Forced branch is deliberately untested per docs/08.
- **File-level tolerance of hand-corrupted rows in v1 jobs.json**: mod.rs `the_store_seals_round_trip_and_drops_invalid_entries` + the migrate.rs bad-row tests already cover it; not re-tested over HTTP.

### 2.6 Priorities

- **P0 (wire contracts)**: #22 (the capture:none record contract — the visible semantics of docs/11 §3.2), #16 (HTTP 409 Busy), #21 (HTTP 429 Capacity).
- **P1 (main paths)**: #18 (limit clamping + illegal-name 404).
- **P2 (edge cases/optional)**: #10 the root-level injected-clock composition test (the inline tests already cover the semantics, deferred by default).
- The remaining 22 rows are covered, nothing new.
