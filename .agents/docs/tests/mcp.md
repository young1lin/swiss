# MCP Integration Test Plan (functionality point → test matrix)

> Input: the feature-overview table in `.agents/docs/mcp.md` (27 rows; the matrix below corresponds row by row, and row numbers are that table's row numbers).
> Test baseline: root `tests/` (app.rs, adminapi.rs, http_adapter.rs, plugin_host.rs, memory.rs, envelope_compat.rs, env_precedence.rs, terminal_ws.rs, groups_e2e.rs) + inline `#[cfg(test)]` in `crates/swiss-mcp` (registry 34, calls 35, traffic 24, mcp_import 25, plus the adapters) + `crates/swiss-mcp/tests/dbbrowser_wiring.rs` + inline in root `src/` (app.rs 2, builtin.rs 3, server.rs 9).
> Driving style: everything goes through `build_app` to produce a Router and then `tower::ServiceExt::oneshot` (no real port, no sleep); the only exceptions are the existing `mcp_endpoint_serves_a_real_client` (a real rmcp client needs a URL) and `tests/http_adapter.rs` (the remote is this binary itself serving echo on an ephemeral loopback port) — new tests do not copy them; oneshot first.
> Environment iron rules: `sandbox()` in `tests/adminapi.rs` sets `MCP_GATEWAY_HOME` (scratch) and `MCP_GATEWAY_MASTER_KEY` (a fixed 32-byte value) once per test binary; new test files reuse the same pattern; live-database tests belong to gate 2 (crates/swiss-it behind `--features it`, fail-not-skip per engine.rs — docs/44), not to these suites; any assertion involving idling/deadlines uses direct calls (`reap_idle`/`check_all`) rather than real timing (the wall clock `swiss_core::util::now_ms` has no injection point, and pause/advance cannot touch it — this is the existing suite's settled answer).

## Existing Coverage Inventory (test file/module → test-name list → corresponding functionality points)

### tests/app.rs (16 tests: client endpoints and boundaries)

- `refuses_a_foreign_host_header` / `refuses_a_foreign_origin` → audit L9/L51 (the Host/Origin half of loopback_guard's triple check; the peer half needs a real socket).
- `health_and_panel_serve`, `unknown_routes_answer_the_node_404_shape` → L9 route shape (`no route for GET /nope`).
- `admin_api_lists_mcps_and_info` → L20 lightweight list rows (name/type/lifecycle/state=unknown for echo) + `/api/info` tokenEnv/build.
- `tools_paging_via_the_admin_api` → L19 (tools first page, pageSize 50).
- `panel_call_runs_a_tool_and_logs_it` → L18 (a panel call lands in the log with via=panel).
- `mcp_endpoint_rejects_missing_bearer_in_jsonrpc_shape` → L9/L41 (401 `{error:"Unauthorized"}`, the pre-body rejection shape).
- `mcp_delete_acknowledges_204` → L9/L50 (DELETE with a valid bearer → 204).
- `unknown_mcp_path_answers_503_jsonrpc` → L9/L42 (503 + JSON-RPC code -32603 "Unknown MCP path").
- `mcp_endpoint_serves_a_real_client` → L9 (an rmcp streamable-HTTP client over the full chain: initialize/tools/list/call).
- `oversized_mcp_body_is_refused` → L9/L41 (BODY_LIMIT 413).
- `new_path_serves_the_endpoint`, `old_root_path_is_no_longer_an_mcp_endpoint`, `old_root_post_of_a_registered_mcp_names_its_new_home`, `old_root_post_of_an_unknown_name_keeps_the_plain_404` → docs/24 (the `/mcp/{name}` domain: the new prefix serves; the retired root single-segment shape answers the plain 404; a registered name's 404 names `/mcp/<name>` and "update the client URL"; an unknown name keeps the plain shape).

### tests/adminapi.rs (111 tests: the full admin-plane tree)

- Boundary: `serves_reads_and_mutations_without_any_credentials`, `has_no_login_route_to_answer` → L9/L55 (/api/* has no login gate; loopback is the boundary).
- Listing: `tags_each_mcp_row_with_how_it_is_launched` (the tag column), six sorting tests including `sorts_by_name_until_the_user_arranges_the_list`, `adds_an_mcp_and_starts_it_while_the_list_stays_status_only`, `reports_a_started_mcp_as_up_once_its_probe_answers` → L20/L30/L34.
- Proxy: `stores_and_returns_a_per_mcp_proxy_on_http_and_rest_mcps`, `rejects_a_proxy_that_is_not_an_http_url` → L13/L14 (the proxy field; ${ENV} references preserved).
- Lifecycle: `stop_frees_the_server_and_start_brings_it_back` (restart included), `answers_a_listing_for_an_mcp_that_was_never_started` (503/404/wide-route kind whitelist) → L21/L19.
- CRUD: `edits_a_managed_mcp_and_restarts_with_the_new_config`, `edits_a_config_file_mcp_and_persists_an_override`, `deletes_a_managed_mcp_and_updates_the_store`, `deletes_a_config_mcp_even_when_no_config_file_holds_it`, `removes_the_entry_from_gateway_config_json_and_the_runtime_registry` (${ENV} references kept verbatim), `persists_added_mcps_to_managed_json`, `rejects_an_invalid_name_and_a_duplicate`, `keeps_cwd_and_the_expose_flags_across_an_edit`, `adds_a_remote_http_mcp_and_never_hands_its_key_to_the_browser`, `adds_a_declared_rest_mcp_and_keeps_its_tool_declarations`, `rejects_a_rest_mcp_with_no_tools_and_an_http_mcp_with_no_url` → L20.
- Rename: `renames_a_managed_mcp`, `keeps_a_renamed_mcps_position_and_prunes_a_deleted_one_from_the_order`, `carries_an_mcps_group_through_a_rename_and_drops_it_on_delete` → L22.
- Masking: `masks_secrets_in_details_and_keeps_the_stored_value_when_the_mask_returns`, `never_stores_the_mask_sentinel_when_the_type_changes_under_an_edit`, `masks_only_the_password_inside_a_connection_url_and_restores_it_on_save` → L31 (mask_def/unmask_body sentinel round trip).
- Panel trial runs: `reads_one_resource_from_the_panel_without_handing_it_the_token`, `records_every_tool_call_with_its_source_and_clears_on_request` → L18.
- Call history: `pages_the_call_log_and_serves_one_reply_in_full` → L25; tool history: `serves_one_tools_recent_runs_for_the_run_tab_dropdown` (limit/q/full arguments read back by seq/400 missing tool/404 unknown MCP) → L26.
- Traffic ring: `records_mcp_traffic_attributed_to_the_token_and_the_self_reported_client`, `attributes_a_server_discover_frame_via_its_meta_client_info`, `attributes_a_tokens_later_frames_to_the_name_it_announced_at_initialize`, `stores_the_full_redacted_request_body_for_the_expandable_raw_view` (rows carry no body/response, `password` values redacted to •••, reply captured), `pages_the_activity_log_newest_first_and_filters_to_actions_server_side`, `folds_clients_over_the_whole_ring_not_over_the_returned_page`, `answers_404_for_a_traffic_entry_that_has_rolled_out_of_the_ring`, `clears_one_clients_traffic_leaving_the_others_intact` → L27/L28.
- Token: `exposes_the_tokens_env_var_name_never_the_token`, `lists_creates_revokes_and_rotates_named_tokens` (create yields a 48-hex secret, rotate 401s the old secret, revoke 404s, persisted), `hands_a_stored_secret_back_so_a_connect_command_needs_no_rotate`, `keeps_secrets_out_of_the_list_and_the_secret_route_open`, `answers_404_for_an_unknown_token_id`, `returns_the_new_secret_after_a_rotate_not_the_old_one` → L29.
- Groups: eleven tests including `starts_with_the_default_group_and_every_mcp_in_it` (the list is never empty — `default` is materialized, ordinary, first) and `renames_default_like_any_other_group_carrying_the_first_slot_with_it` (renaming `default` carries both explicit and unassigned members with the first slot, adminapi.rs:1717,1862) → L30; also `rejects_a_malformed_duplicate_empty_or_last_group_destroying_list` (an empty list is the one refusal; `["default"]` alone is fine now), `refuses_a_rename_onto_an_existing_group_or_a_missing_one`, `deleting_a_group_by_omission_moves_its_mcps_to_the_first_remaining_group` (whole-table writes and deletes land members in the FIRST remaining group; rename carries members; 404/400).
- Import: `imports_stdio_and_http_entries_suffixes_collisions_and_skips_this_gateways_urls`, `imports_without_credentials_like_every_other_api_route` → L23.
- Others: `reports_gateway_memory_without_walking_a_process_tree_when_nothing_was_spawned` (`/api/memory`), `lists_prompts_paged_even_for_a_server_that_publishes_none`, `parses_a_json_body_sent_with_an_uppercase_content_type`; 5 vault tests + `an_mcp_builder_refuses_a_missing_vault_reference` + `the_rest_connection_test_fails_honestly_on_a_missing_vault_reference` → L24 (the rest/vault half of the connection test).
- Newer families since the original inventory: the groups-scope suite (`the_family_*` across the mcps/tokens/tunnels/targets/secrets/jobs scopes, plus the `reordering_*_never_rehomes_the_first_groups_default_members` pair); the OAuth authorize flow (`an_oauth_authorize_flow_runs_end_to_end`, `a_second_post_while_live_hands_back_the_same_flow`, figma/zai add tests); autostart (`autostart_route_*`); def revisions (`replace_parks_the_old_def_and_serves_the_new_one`, `restore_swaps_back_and_parks_the_live_def`, `six_replaces_keep_only_the_last_five_snapshots`); `a_disabled_mcp_refuses_clients_with_the_disabled_wording` (docs/28 D2); `a_mariadb_def_builds_and_carries_its_own_tag` (docs/29); and the docs/24 name-domain tests (`plugin_domain_names_need_no_host_reservation`, `an_mcp_named_test_or_import_stays_deletable_and_editable`, `imports_a_name_the_root_once_reserved_without_renaming` — collection actions live under `/api/mcpdefs/*`, never a static segment at the {name} position).

### tests/http_adapter.rs (7 tests: the http adapter as a real proxy)

- `lists_the_remotes_tools_through_the_proxy`, `fails_to_connect_when_the_configured_headers_do_not_authenticate` (build is the handshake; a wrong bearer = a start error), `round_trips_a_tool_call_to_the_remote`, `announces_only_the_capabilities_the_remote_has`, `declares_no_ping_so_a_metered_remote_is_never_probed`, `close_drops_the_connection_so_the_next_build_reconnects`, `a_vault_header_reference_authenticates_the_proxy` → L13; the remote is this binary's echo (`remote_echo()`) on an ephemeral loopback port, which does not count as a "real external port".

### tests/plugin_host.rs (7 MCP-related tests: plugin gating and the catalog)

- `inventory_shape_is_exact_and_the_mcp_plugin_started_the_mcps` (pages mcps/traffic/tokens order 10/20/30; the entry module is genuinely servable) → the three panel pages (L82-105).
- `boot_disabled_plugins_guard_every_route_they_own` (/api/mcps and deep /api/traffic paths answer host 503 for every method; the catch-all client POST also 503s and the lazy proc does not spawn; loopback answers before the plugin) → L9/L49/L195.
- `enabled_plugins_keep_every_api_shape_they_had`, `route_ownership_is_longest_prefix_at_segment_boundaries` (`/api/traffic`, `/api/mcps/a/calls` owner=mcp) → L190.
- `disabling_mcp_503s_api_db_and_marks_datas_requirement_unmet`, `the_catalog_seat_survives_a_hundred_mcp_restarts` → L33/L185 (the connection catalog: the 503 names it, requiresMet, a hundred start/stop cycles with no seat leak).

### crates/swiss-mcp inline (mapped to functionality points)

- registry.rs, 34 tests → L10 (state machine/concurrency/tombstone/generation/update_def carries toggles/close_all), L22 (rename+evictor+log renaming), L34 (check_all concurrent per entry, no ping reports unknown, late probes discarded, `reap_idle` called directly, `arm_idle only for lazy, activity defers, idleMs=0 opts out), L33 (catalog row keys).
- calls.rs, 35 tests → L25 (two-layer logging, preview/full text, redaction, via/client attribution, rename carries the alias over, age sweep, bodies pruned to 50 entries, tool-history full-argument filtering).
- traffic.rs, 24 tests → L27/L28 (ring cap, actions filter, folding over the whole ring, clearing leaves known_client untouched, disk-tail restore/no-resurrection, byte-budget trimming).
- mcp_import.rs, 25 tests → L23 (stdio→proc, url→http, -1/-2, self-proxy skip, name sanitization).
- adapters/mod.rs, 7 tests → L11 (make_adapter dispatch, unknown types, construction-time failure, http/rest have no ping, ${ENV} expanded only at make time); echo.rs, 2 tests → L16; direct.rs, 19 tests → L17 (toggle seeding/live on the very next request/disabled absent — engine layer); http.rs 18 + proxy.rs 12 tests → L13 (SSE/JSON single-shot, paged aggregation, annotations stripped, shared pool, assert_proxy_url); rest.rs, 40 tests → L14 (full template-language syntax, pick, declaration fidelity, no ping); proc.rs, 12 tests → L12 (tokenization/shim/GBK decoding/stderr ring/timeout parsing); mysql/pg/redis plus *_resources/*_browser → L15/L7 (connection strings, read-only sessions, SQL constants, command policy, key-shape folding, render budget in tool_server.rs, 5 tests); zai.rs 9 + remote.rs 9 tests → the zai vision MCP type and the remote-targets adapter (the former introspect.rs no longer exists); paging.rs, 1 test → L19 (cursor base64url round trip).
- src/server.rs inline, 9 tests → L180 (boot registration/override/mcpEnabled respected/lazy Idle/failure isolation/source tag); src/builtin.rs inline, 2 tests → L105/L230 (the token page belongs to the MCP group but its routes are host-owned; label unique).
- src/app.rs inline, 2 tests → L35 (the Data picker's visual-order ranking: `visual_order_slices_the_flat_rank_by_group` — two groups whose flat order interleaves, exactly what a cross-group drag produces, come out grouped in stored group order with members by flat rank; `visual_order_keeps_unranked_names_last_in_name_order_inside_their_group` — no manual order at all means name order inside each group, unassigned names in the first, src/app.rs:594,630).
- crates/swiss-mcp/tests/dbbrowser_wiring.rs → L33 (the SQL statements and parameter order the browser consumes).

## Functionality-Point → Test Matrix

A "—" in the gap column means the row has no integration gap; a "(partial)" marker means the row has sub-item gaps — see the new-test-names column.

| # | Functionality point (audit-doc row number) | Existing tests | Gap | New test names (English snake_case, intended location noted) | Assertion essentials (method + path + status code + JSON fields/shape) | Special handling |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | MCP client endpoint: bearer checks, lazy wakeup, generation cache (L9) | app.rs: `mcp_endpoint_rejects_missing_bearer_in_jsonrpc_shape`, `unknown_mcp_path_answers_503_jsonrpc`, `mcp_delete_acknowledges_204`, `mcp_endpoint_serves_a_real_client`, `oversized_mcp_body_is_refused`; registry.rs: `ensure_started_wakes_an_idle_entry_and_passes_a_running_one_through` | **No route-level tests for lazy wakeup or the generation cache** (lazy wakeup only has the registry inline; generation-cache eviction is entirely untested; the start-failure 503 is untested) | `a_lazy_mcp_is_woken_and_served_by_its_own_wakeup_request` (tests/adminapi.rs); `a_failed_start_answers_503_jsonrpc_and_an_admin_error_respectively` (tests/adminapi.rs); `restarting_an_mcp_serves_the_next_request_from_a_fresh_handler` (tests/adminapi.rs); `mcp_delete_without_a_bearer_is_refused` (tests/app.rs) | Lazy wakeup: POST /api/mcps {name:"lz",type:"echo",lazy:true}→201; GET /api/mcps row lifecycle=="idle"; POST /mcp/lz (bearer, tools/list)→**200** and tools contains echo (the wakeup request itself is served); re-query the row and lifecycle=="started". Failed start: register def {"type":"http","url":"http://127.0.0.1:1/mcp"}, registry.start→Err; POST /mcp/dead→503, body {"jsonrpc":"2.0","error":{"code":-32603,"message":"MCP 'dead' is not started (state: error)"},"id":null}; GET /api/mcps/dead/tools→503 {"error":"MCP 'dead' is not started"}; GET /api/mcps row lifecycle=="error", reason present (non-null). Generation: echo via POST /mcp/echo (tools/list)→200; POST /api/mcps/echo/stop→200; POST /mcp/echo→503 JSON-RPC; POST start→200; POST /mcp/echo→**200** (the old handler is evicted; the new generation serves). DELETE without bearer→401 {"error":"Unauthorized"} | All oneshot; 127.0.0.1:1 connection-refused returns instantly, no sleep; tests that drive /echo take `traffic_lock()` (the ring is process-wide); the lazy key is legal for echo (L137: lazy attaches to all types) |
| 2 | Registry: CRUD, start/stop/restart/rename (L10) | registry.rs, 34 tests (state machine, double start single build, stop/start races, tombstone, generation fence, update_def carries toggles, rename+evictor, close_all, unknown rejected); adminapi.rs CRUD/lifecycle tests | — (complete at the engine layer) | — | — | — |
| 3 | Adapter-family dispatch make_adapter (L11) | adapters/mod.rs: `routes_every_built_in_type_to_its_adapter`, `refuses_an_unknown_type_and_names_the_built_ins`, `a_def_that_cannot_be_configured_fails_at_make_time_not_at_the_first_request`, `env_refs_are_expanded_at_make_time_so_the_persisted_def_keeps_the_reference` | — | — | — | — |
| 4 | proc adapter: lazy start + idle reaping + subtree kill (L12) | registry.rs: `a_proc_mcp_is_lazy_by_default_and_every_other_type_is_not`, `the_sweeper_reaps_a_lazy_child_whose_deadline_passed`, `the_idle_reaper_only_arms_for_lazy_entries`, `activity_pushes_the_reap_deadline_back`, `idle_ms_zero_opts_out_of_reaping_entirely`; proc.rs, 12 tests (tokenization/shim/stderr ring/timeouts) | (partial) Job Object subtree kill and the PID-ledger fallback are not oneshot-able (see the "not suitable" section) | — | — | Not integration-testable: child process + FFI; live-verify |
| 5 | http adapter (L13) | tests/http_adapter.rs, 7 tests (the handshake is the test, wrong bearer refuses to start, roundtrip, caps narrowed, no ping, close reconnect, vault header); http.rs 18 + proxy.rs 12 inline tests | — | — | — | The existing loopback "remote" pattern stays; new tests do not copy it |
| 6 | rest adapter (L14) | rest.rs, 40 inline tests (templates/compilation/round trip/pick/headers); adminapi.rs: `adds_a_declared_rest_mcp_and_keeps_its_tool_declarations`, `the_rest_connection_test_fails_honestly_on_a_missing_vault_reference` | **No end-to-end**: a declared tool has never actually sent one HTTP request through the gateway; the ok:true half of the connection test is also missing | `a_rest_tool_round_trips_through_the_gateway` (new file tests/rest_adapter.rs); `the_rest_connection_test_makes_one_plain_get_and_reports_ok` (tests/rest_adapter.rs) | Round trip: local axum serves `/repos/o/r` returning JSON (ephemeral loopback port, same pattern as http_adapter.rs); POST /api/mcps {name:"gh",type:"rest",baseUrl,tools:[{name:"get_repo",input:{owner,repo required},request:{method:"GET",path:"/repos/{{owner}}/{{repo}}"},pick:["full_name"]}]}→201; GET /api/mcps/gh/tools→tools contains get_repo; POST /api/mcps/gh/call {tool:"get_repo",arguments:{owner,repo}}→200 {ok:true, text contains the full_name value, none of the unpicked keys}; the Authorization header the remote recorded equals the declared value. Connection test: POST /api/mcpdefs/test {same def}→200 {ok:true, ms, status:200} — one plain GET, not a metered tool | The new file needs its own sandbox() (MCP_GATEWAY_HOME+MASTER_KEY); abort the remote handle at test end |
| 7 | mysql/pg/redis in-process drivers (L15) | mysql.rs/pg.rs/redis.rs, sql.rs, *_resources.rs, tool_server.rs, dbbrowser_wiring.rs inline tests (connection strings, read-only sessions, SQL guards, command policy, render budget) | **No connection-test assertions at all via /api/mcpdefs/test** (testable-type dispatch, the 5s cap, driver error text passed back verbatim) | `connection_tests_answer_honestly_for_a_dead_port_and_refuse_untestable_types` and `the_connection_test_accepts_mariadb_like_mysql` (tests/adminapi.rs; the mariadb gate mirrors the panel's Test list, docs/29); the live-database ok:true half belongs to gate 2 (swiss-it), not to a self-skipping root test | Dead DB: POST /api/mcpdefs/test {type:"mysql",host:"127.0.0.1",port:1}→**200** {ok:false, ms:number, error contains "refused"/"Connection"} (the error is the result, not a 5xx; <=5s guaranteed by the timeout, connection-refused is instant); {type:"echo"}→400 {"error":"no connection test for type 'echo' (testable: mysql | mariadb | redis | pg | http | rest)"}; {type:"proc"} same 400. Live DB (optional): the ok:true half rides gate 2 (swiss-it engines), not an env-gated root test | The dead-port test needs no credentials and no sleep; live-DB assertions are gate-2 material (fail-not-skip, engine.rs); MASTER_KEY is guaranteed by sandbox() |
| 8 | echo demo adapter (L16) | echo.rs: `tool_definition_matches_the_node_build`, `get_tool_returns_echo_by_name_only`; also serves as the vehicle for every integration test | — | — | — | — |
| 9 | Tool toggles / resources master switch (L17) | direct.rs inline, 19 tests (`seeds_the_disabled_tool_set_from_the_def`, `hides_a_disabled_tool_from_tools_list`, `a_tool_toggle_is_live_on_the_very_next_request_with_no_rebuild`, `resources_off_returns_an_empty_list_and_is_live_too`, etc. — engine/adapter layer); http.rs `seeds_its_tool_toggle_from_the_def...` | **The entire HTTP API face is a gap**: the two routes `POST /api/mcps/{name}/tools/{tool}` and `POST /api/mcps/{name}/resources-toggle` have zero tests (persist first then mutate memory, the disabled tool absent from the client broadcast list, 501 for unsupported types, the unchanged short-circuit) | `toggling_a_tool_off_hides_it_from_the_clients_broadcast_list` (tests/adminapi.rs, P0); `the_resources_master_switch_empties_the_list_and_persists` (tests/adminapi.rs, P0); `toggles_on_a_type_without_toggles_answer_501` (tests/adminapi.rs) | Tool toggle: after with_echo, POST /api/mcps/echo/tools/echo {enabled:false}→200 {tool:"echo",enabled:false,disabledTools:["echo"]}; h.mcp("/mcp/echo",TOKEN,tools/list)→200 with **tools array empty** (the broadcast list is the contract); GET /api/mcps/echo/tools→tools empty + disabledTools==["echo"]; POST {enabled:true} again→200 without unchanged, tools/list shows echo again; repeat off→200 {unchanged:true}; persisted: ManagedStore::open_at(h.path).disabled_tools("echo")==["echo"]. Resources master switch: register_fixture("res") + start; POST /api/mcps/res/resources-toggle {enabled:false}→200 {enabled:false}; GET /api/mcps/res/resources→resources==[] and resourceEnabled==false; turn back on→200 {enabled:true}, the list shows echo://res again; persisted: resource_enabled("res")==Some(false) (after off); setting the same value again→200 {unchanged:true}. 501: after POST-creating a rest MCP (enabled:false), POST /api/mcps/{n}/tools/x→501 {error contains "tool toggles are not supported for MCP type 'rest'"}; resources-toggle likewise 501 | Take traffic_lock() (driving the endpoi... (line truncated to 2000 chars)
| 10 | Panel tool trial runs / resource reads (L18) | app.rs: `panel_call_runs_a_tool_and_logs_it`; adminapi.rs: `reads_one_resource_from_the_panel_without_handing_it_the_token`, `records_every_tool_call_with_its_source_and_clears_on_request` | (partial) The failure-path shape is not asserted (errors are reported as results: 200 + ok:false) | `panel_call_and_resource_report_failures_as_results_not_transport_errors` (tests/adminapi.rs) | POST /api/mcps/{echo}/call {tool:"nope",arguments:{}}→**200** {ok:false,isError:true,ms:number,text contains "unknown tool"}; POST /api/mcps/{fixture}/resource {uri:"bogus://x"}→200 {ok:false,text contains "unknown resource"}; call missing tool→400 "tool is required"; resource missing uri→400 (the existing tests already cover the last two; this adds the failure shapes) | oneshot; echo.call returning Err for an unknown tool triggers it |
| 11 | Paged browsing of tools/resources/prompts (L19) | adminapi.rs: `pages_tools_and_resources_on_demand_while_details_carries_logs`, `answers_a_listing_for_an_mcp_that_was_never_started` (503/404/kind whitelist), `lists_prompts_paged_even_for_a_server_that_publishes_none`; paging.rs: `cursor_round_trips_the_node_wire_format`; proxy.rs, 12 paged-aggregation tests | **Cursor paging has never been walked through the API** (nextCursor appears→follow it→exhaust it); the disabledTools/resourceEnabled companion fields have no assertions | `a_cursor_walk_pages_the_listing_to_its_end` (tests/adminapi.rs) | New PagedFixture (60 tools): GET /api/mcps/p/tools→200 {pageSize:50, tools.len()==50, nextCursor a non-empty string}; GET .../tools?cursor=<previous value>→200 {tools.len()==10, **nextCursor key absent**} (`assert!(body.get("nextCursor").is_none())`); disabledTools==[] throughout (the companion field is present); resources half: the resourceEnabled==true key is present | Needs the new PagedFixture (see helpers); oneshot; time-independent |
| 12 | MCP CRUD (L20) | adminapi.rs, 13 tests (edits/deletes/override/persist/invalid+duplicate/rest declaration fidelity/http url required) | (partial) the reserved-names half is settled the other way — docs/24 P2 made health/api/admin/mcp ordinary MCP names under /mcp/* (pinned by `plugin_domain_names_need_no_host_reservation` and `imports_a_name_the_root_once_reserved_without_renaming`); what stays open is empty pg url and mysql database identifiers | `rejects_an_empty_pg_url_and_a_bad_database_name` (tests/adminapi.rs) | {name:"pg1",type:"pg",url:""}→400 {error contains "url"}; {name:"my1",type:"mysql",database:"bad;name"}→400 {error contains "database"} (assert_ident); a valid 63-character name→201 | oneshot; extend with more cases in the style of the existing `rejects_an_invalid_name_and_a_duplicate` |
| 13 | Lifecycle actions (L21) | adminapi.rs: `stop_frees_the_server_and_start_brings_it_back` (restart included); the registry.rs lifecycle group | **Persistence not asserted** (the actions also write enabled/mcpEnabled in managed.json, kept across restarts — only the server.rs boot inline covers it from the file side) | `lifecycle_actions_persist_the_panel_stop_decision` (tests/adminapi.rs) | Config-sourced: h.register("cfg", echo) + start; POST /api/mcps/cfg/stop→200 {name,lifecycle:"stopped"}; reopening ManagedStore::open_at(h.path): enabled_for("cfg")==Some(false); POST start→200; enabled_for("cfg")==Some(true). Managed-sourced: create via POST /api/mcps then stop→that entry in store.all() has enabled==false. Unknown name: POST /api/mcps/ghost/start→400 {error contains "unknown"} (registry rejects) | oneshot; the has_server helper can re-verify a real stop |
| 14 | Rename (L22) | adminapi.rs: `renames_a_managed_mcp`, `keeps_a_renamed_mcps_position...`, `carries_an_mcps_group_through_a_rename...`; registry.rs: `rename_moves_the_entry_under_a_new_key`, `rejects_a_rename_onto_an_existing_name`, `renaming_to_the_same_name_is_a_no_op`, `the_evictor_fires_for_every_abandoned_name`; calls.rs: `files_a_call_issued_under_the_old_name_into_the_renamed_log` | (partial) **The call-log move and the old path going dark have no route-level tests**; tunnel-link following has zero tests (needs TunnelLinks injection) | `renaming_carries_the_call_log_and_the_old_path_stops_answering` (tests/adminapi.rs); `details_and_rename_and_delete_consult_the_tunnel_links` (tests/adminapi.rs) | Move: with_echo_named("old") + one panel call; POST /api/mcps/old/rename {name:"new"}→200 {name:"new"}; GET /api/mcps/new/calls→calls.len()==1 (history travelled); GET /api/mcps/old/calls→**404**; h.mcp("/mcp/old",TOKEN,tools/list)→503 {"error":{"message":"Unknown MCP path: old"}}; h.mcp("/mcp/new",...)→200. Tunnels: inject FakeTunnelLinks; GET /api/mcps/x/details→200 and details["tunnels"]==["t1"]; after rename the fake receives ("x","y"); after delete it receives forget("x") | The tunnel test needs Harness to expose ctx (see helpers); mcp calls take traffic_lock() |
| 15 | Client .mcp.json import (L23) | adminapi.rs: `imports_stdio_and_http_entries_suffixes_collisions_and_skips_this_gateways_urls`, `imports_without_credentials...`; mcp_import.rs, 25 tests | (partial) Structural garbage → 500 (Node semantics) untested | `import_answers_500_for_a_structurally_broken_file` (tests/adminapi.rs) | POST /api/mcps/import with body `[]` (not an object)→**500** {error non-empty}; then {"mcpServers":"nope"} (non-object value)→500 | oneshot; touches no network |
| 16 | Connection test (L24) | adminapi.rs: `the_rest_connection_test_fails_honestly_on_a_missing_vault_reference`; http_adapter.rs indirectly (build is the handshake) | **Main-path gap**: http handshake success/timeout cap, honest DB dead-database failure, 400 for untestable types (folded into the new tests of #7 and #6) | See #6 `the_rest_connection_test_makes_one_plain_get_and_reports_ok` and #7 `connection_tests_answer_honestly...` | (same as the #6/#7 rows) | The http success half can simply reuse rest_adapter.rs's local remote; no external network needed |
| 17 | Call history (L25) | adminapi.rs: `pages_the_call_log_and_serves_one_reply_in_full`, `records_every_tool_call...`; calls.rs, 33 tests (two layers/preview/full text/redaction/retention/rename alias) | (partial) The `stderr` companion field (appended by the index-page handler) has no assertions | `calls_listing_carries_the_stderr_companion_field` (tests/adminapi.rs, P2) | GET /api/mcps/{started echo}/calls→200 and the body["stderr"] key is present ("" when echo has no log) | oneshot; the proc stderr half is left to live-verify |
| 18 | Tool run history (L26) | adminapi.rs: `serves_one_tools_recent_runs_for_the_run_tab_dropdown` (limit/q/full arguments read back by seq/400/404); calls.rs history group, 8 tests | — | — | — | — |
| 19 | Traffic ring (L27) | adminapi.rs, 8 tests (attribution/_meta clientInfo/token memory/full-text redaction/paging/actions filter/404+400/per-client clear); traffic.rs, 25 tests (including disk-tail restore) | (partial) The `?mcp=`/`?client=`/`?method=` query parameters are not asserted through the API (inline tests already prove the query engine) | `traffic_filter_parameters_narrow_the_api_listing` (tests/adminapi.rs, P2) | Seed 3 rows via `swiss_mcp::traffic::record_traffic` (m1/m2 × two tokens); GET /api/traffic?mcp=m1→entries all e["mcp"]=="m1" and total==2; ?client=tb likewise; combined ?mcp=m1&method=tools/list narrows total | Take traffic_lock(); the process-wide ring OnceLock (audit L237 transitional state) is serialized by the lock |
| 20 | Folded client summary (L28) | adminapi.rs: `folds_clients_over_the_whole_ring_not_over_the_returned_page` (n:label/t:token keys, count/mcps/tokens); traffic.rs: `folds_the_whole_ring_into_one_row_per_client_newest_active_first`, `remembers_the_name_a_token_announced...` (mid-stream label upgrade) | — | — | — | — |
| 21 | Token management (L29) | adminapi.rs, 6 tests (CRUD/rotate/revoke/secret/404); builtin.rs: `mcp_descriptor_contributes_the_token_page_but_keeps_its_routes_host_owned` | (partial) **Plugin disable does not invalidate them** — only the descriptor test pins page ownership; there is no behavioral assertion that "/api/tokens still answers 200 while mcp is disabled" | `token_routes_survive_an_mcp_plugin_disable` (tests/plugin_host.rs) | After full_app, POST /api/plugins/mcp/disable→200; GET /api/tokens→**200** {tokens array contains "default"}; GET /api/mcps→503 (the host-owned vs plugin-tree contrast) | Uses the existing full_app assembly in plugin_host.rs; oneshot |
| 22 | Sidebar ordering and grouping (L30) | adminapi.rs, 17 tests (6 sorting + 11 grouping, incl. `renames_default_like_any_other_group_carrying_the_first_slot_with_it` at adminapi.rs:1862 and `deleting_a_group_by_omission_moves_its_mcps_to_the_first_remaining_group` at adminapi.rs:1885); the store-level semantics are additionally pinned by the managed.rs inline group suite (`materializes_default_at_the_front_of_an_unmarked_pre_v2_file`, `assigning_the_default_group_explicitly_stores_the_canonical_name`, `deleting_the_group_that_owns_the_first_slot_moves_the_sink`, `refuses_to_delete_the_last_remaining_group`, crates/swiss-host/src/managed.rs:988-1335) | (partial) **Deleting `default` itself by omission and the pre-v2 → groupsV2 migration have no route-level tests**; an explicit `{"group":"default"}` assignment (a real mcpGroups entry now, canonical casing) is only pinned store-level | `deleting_the_default_group_by_omission_moves_its_members_to_the_new_first_group` (tests/adminapi.rs, P1); `a_pre_v2_groups_file_serves_default_first_and_the_v2_marker_freezes_edits` (tests/adminapi.rs, P1); `assigning_default_explicitly_persists_a_real_entry_through_the_api` (tests/adminapi.rs, P2) | Default-delete: PUT /api/groups ["default","Docs"]→200, assign one MCP explicitly to default and leave one unassigned; PUT /api/groups ["Docs"]→200; GET /api/mcps→groups==["Docs"] and BOTH rows group=="Docs" (the new first group caught the explicit and the unassigned member alike). Pre-v2 migration: hand-write a PLAIN managed.json `{"mcps":[],"groups":["learn","ForTest"]}` into the scratch dir before `ManagedStore::open_at` runs in setup; GET /api/mcps→groups==["default","learn","ForTest"] (materialized at the front); PUT /api/groups ["learn","ForTest"]→200 (default deleted on purpose); reopen the raw file → `groupsV2`==true and groups carries no "default"; GET /api/mcps again→still no default (no resurrection). Explicit default: PUT /api/mcps/context7/group {"group":"DeFaUlT"}→200 {"group":"default"}; the raw mcpGroups carries context7→"default" (a real entry, not absence), and deleting every other group still answers it under the first slot | setup() opens the store on a fresh path each call — the pre-v2 file must be written before setup() constructs the Harness (a plaintext legacy file is tolerated and re-sealed on first read); no timing anywhere; store-level reopen checks use `ManagedStore::open_at(h.path)` as the existing tests do |
| 23 | Details (L31) | adminapi.rs: `pages_tools_and_resources_on_demand_while_details_carries_logs` (logs is a string, no list carried), 3 masking tests, `adds_a_remote_http_mcp...` (headers masked) | (partial) **The tunnels field and the state/reason shape are untested** (the same-screen health-failure column depends on tunnels) | Folded into #14 `details_and_rename_and_delete_consult_the_tunnel_links`; plus: `details_report_state_reason_and_source_fields` (tests/adminapi.rs, P2) | tunnels: see #14. Field shape: a stopped entry GET details→200 {source:"config"/"managed", state=="stopped", the reason key **absent**} (`assert!(body.get("reason").is_none())`); a fixture with a failing ping (Fixture.ping=Some(Err(...))) + check_all→state=="down" and reason contains the original error text; tunnels==[] when there are no links | FakeTunnelLinks injection; check_all called directly (the wall clock has no injection; do not use start_timer) |
| 24 | Client connect command (L32, assembled on the frontend) | The server-side dependency half: `hands_a_stored_secret_back...`, `exposes_the_tokens_env_var_name...` | Frontend assembly is not testable in this repo | — | — | Panel JS is a Node asset: covered by the Node-side vitest plus `scripts/test-instance.ps1` live-verify (the panel Copy command on 19998) |
| 25 | Database browsing (the Data backend in MCP form) (L33) | registry.rs: `the_catalog_lists_one_row_per_entry_and_keys_them_by_definition`; plugin_host.rs: `disabling_mcp_503s_api_db_and_marks_datas_requirement_unmet`, `the_catalog_seat_survives_a_hundred_mcp_restarts`, `enabled_plugins_keep_every_api_shape_they_had` (/api/db 200 shape); dbbrowser_wiring.rs; *_browser inline (redis_browser 8, pg_browser 3) | (partial) mysql_browser has no inline tests; the NotBrowsable error text naming adapter types lives on the lease side (swiss-data domain; see the .agents/docs/tests/data.md plan) | `mysql_browser_statements_match_the_wiring_contract` (inline in crates/swiss-mcp/src/adapters/mysql_browser.rs, P2; if that file already holds SQL constants, assert against the constants — same pattern as dbbrowser_wiring.rs) | Inline: assert on the browse/fk/index statements mysql_browser produces and on their placeholder order (same style as pg's max_placeholder) | /api/db lease behavior belongs to data.md; here we only pin the MCP-side SQL contract |
| 26 | Health probes / idle reaping / page-cache sweeping (L34) | registry.rs: `marks_a_passing_ping_as_up_with_latency`, `captures_the_raw_failure_reason_when_ping_fails`, `reports_unknown_when_an_adapter_has_no_ping`, `does_not_probe_a_stopped_entry...`, `discards_a_probe_result_that_lands_after_the_mcp_was_stopped` (generation fence), `the_idle_reaper_only_arms_for_lazy_entries`, `the_sweeper_reaps_a_lazy_child_whose_deadline_passed`, `activity_pushes_the_reap_deadline_back`, `idle_ms_zero_opts_out...`; adminapi.rs: `reports_a_started_mcp_as_up...` | (partial) **The page-cache 60s TTL sweep has no tests at all** (`sweep_page_caches`/`is_page_cache_stale`, zero assertions); the start_timer loop shape (15s/1s) is not testable (wall clock) | `an_untouched_page_cache_is_reported_stale_by_the_pure_predicate` (inline in crates/swiss-mcp/src/paging.rs); `sweep_page_caches_drops_only_the_stale_slots` (inline in crates/swiss-mcp/src/registry.rs) | Pure predicate: after PageCache::new(), `is_page_cache_stale(&c, now_ms())`==false; `is_page_cache_stale(&c, now_ms()+PAGE_CACHE_TTL_MS+1)`==true (now is an injected parameter, naturally testable). Sweep: register_fixture + start, GET tools to fill the cache; via the test hook, push tool_page/res_page touched_at back 61s (or set None as the control); after `sweep_page_caches()`, entry.data's tool_page.is_none()==true (swept) while the control slot remains | The current code's touched_at is private — needs a new `#[cfg(test)] fn test_backdate(&self, ms: u64)` hook (see helpers); **do not wait a real 60s** (the no-sleep iron rule); the 15s loop itself → live-verify |
| 27 | Data connection picker ranking, visual order (L35) | src/app.rs inline, 2 tests (`visual_order_slices_the_flat_rank_by_group` at app.rs:594 — two groups whose flat order interleaves come out grouped, members keeping their flat rank; `visual_order_keeps_unranked_names_last_in_name_order_inside_their_group` at app.rs:630 — no manual order at all means name order inside each group, unassigned in the first) | (partial) **The ranking is pinned only at the unit seam**; the route face (the /api/db connections list ordered visually) has no test in this suite — its lease-side assertions belong to `.agents/docs/tests/data.md` | `the_db_picker_follows_the_sidebar_visual_order_across_interleaved_groups` (tests/plugin_host.rs, P2) | In the existing full_app assembly (where /api/db already serves), register two browsable DB-type MCPs a and b in two groups whose PUT /api/order interleaves them (a, b from group one, b, a from group two); PUT /api/groups ["g1","default"] + PUT /api/mcps/{name}/group accordingly; GET /api/db→200 and the connections array lists g1's members before default's — the visual order — although the flat order interleaves | The catalog lists registered browsable halves without a live connection, so no DB credentials are needed; reuse the full_app assembly patterned on `enabled_plugins_keep_every_api_shape_they_had`; the Data-side picker details stay in tests/data.md |

## New Test Helpers Needed (builder/seed/fixture, with signatures)

1. **`PagedFixture`** (tests/adminapi.rs, alongside the existing `Fixture`) — provides the >50 tools that #11's cursor paging needs:

   ```rust
   /// An engine with N tools, so a listing must page: page 0 fills PAGE_SIZE and hands a cursor.
   struct PagedFixture { tools: usize }
   impl Engine for PagedFixture {
       fn kind(&self) -> &'static str { "paged" }
       fn tools(&self) -> Vec<ToolDef> {
           (0..self.tools).map(|i| ToolDef {
               name: format!("t{i:02}"), description: String::new(),
               input_schema: json!({"type":"object"}),
           }).collect()
       }
       async fn call(&self, _tool: &str, _args: &Value) -> Result<Value, String> {
           Err("no calls in the paging fixture".into())
       }
       fn meta(&self) -> ServerMeta { ServerMeta { name: Some("paged".into()), ..Default::default() } }
       async fn ping(&self) -> Option<Result<(), String>> { Some(Ok(())) }
   }
   // Register it through DirectAdapter exactly like register_fixture does today.
   ```

2. **`FakeTunnelLinks`** (tests/adminapi.rs) — gives #14/#23 an injectable read-only view that records calls:

   ```rust
   struct FakeTunnelLinks { calls: std::sync::Mutex<Vec<(&'static str, String)>> }
   #[async_trait] // actually a plain trait; implement per the swiss_tunnels::TunnelLinks signature (Send+Sync)
   impl swiss_tunnels::TunnelLinks for FakeTunnelLinks {
       fn tunnels_for_mcp(&self, _name: &str) -> Vec<Value> { vec![json!("t1")] }
       fn rename_mcp(&self, from: &str, to: &str) {
           self.calls.lock().unwrap().push(("rename", format!("{from}->{to}")));
       }
       fn forget_mcp(&self, name: &str) {
           self.calls.lock().unwrap().push(("forget", name.into()));
       }
   }
   ```

At the same time `Harness` must keep the `ctx: Arc<AppContext>` used for assembly (or provide `fn set_tunnel_links(&self, links: Arc<dyn swiss_tunnels::TunnelLinks>)` writing `ctx.tunnel_links` internally — it is a `pub RwLock<Option<...>>`; today `setup()` discards ctx after building, so it must instead retain it as a field).

3. **`rest_remote()`** (new file tests/rest_adapter.rs) — gives #6 a local JSON remote:

   ```rust
   /// One axum JSON endpoint on an ephemeral loopback port: GET /repos/{owner}/{repo}
   /// answers a fixed repo object and records the Authorization header it saw.
   async fn rest_remote() -> (String, Arc<std::sync::Mutex<Vec<Option<String>>>>, tokio::task::JoinHandle<()>);
   ```

The file header needs the same `sandbox()` as adminapi.rs (MCP_GATEWAY_HOME + MCP_GATEWAY_MASTER_KEY, one-time init) — ManagedStore seals an envelope when it writes to disk.

4. **`PageCache::test_backdate`** (crates/swiss-mcp/src/paging.rs, `#[cfg(test)]` pub(crate)) — for the #26 sweep test:

   ```rust
   #[cfg(test)]
   impl PageCache {
       /// Push touched_at back by ms so a sweep test can age a cache without sleeping.
       pub(crate) fn test_backdate(&self, ms: u64) { /* state.touched_at -= ms */ }
   }
   ```

Plus, inside the registry.rs test module, a small assertion helper that reads the private slot (the tests already live in the same file's `mod tests` and can read `entry.data.write().tool_page` directly).

5. Reuse the existing infrastructure: `traffic_lock()` / `clear_traffic(None)` (serializes the process-wide ring), `with_echo()` / `with_echo_named()` / `register_fixture()` / `row_named()` / `entry_for()` / `field_of()`, and `ManagedStore::open_at(h.path)` reopened to verify persistence — new tests use these throughout; do not build parallel helpers.

## Points Not Suitable for Integration Tests, and Alternatives

- **proc subtree kill (Job Object FFI) and the PID-ledger fallback**: needs a real child process and Windows handle lifetimes; tokenization/shim/decoding/stderr ring are already covered by the proc.rs inline tests. Alternative: `scripts/test-instance.ps1` attaches a real npx MCP on 19998, and after Stop/disabling the plugin you verify by PID that the subtree is gone (the swiss-live-verify flow).
- **Response body unreadable → 502 (app.rs:391-399)**: under oneshot, constructing a >16MB response has a terrible benefit/cost ratio. Alternative: code review + live-verify; or a future cfg(test) stub in the adapter (not mandated by this plan).
- **start_timer's 15s probe loop and 1s patrol**: the sleeps are driven by `tokio::time::sleep`, but the deadline checks use the wall clock `now_ms()`, which has no injection point — pause/advance cannot touch it. Alternative: keep the existing pattern (call `check_all()` / `reap_idle()` directly, see the registry.rs inline tests); real intervals and concurrent isolation are left to live-verify.
- **Everything in the panel TS (polling patch, focus protection, Copy-connect-command assembly, signature-based redraw skipping)**: panel behavior is not drivable from these Rust oneshot suites. Alternative: this repo's own vitest in crates/swiss-panel/panel/test/ (ADR-024) is the acceptance spec; `every_contributed_page_entry_is_actually_served` (plugin_host.rs) already pins that the entry module is genuinely servable.
- **DB-engine behavior against a real server** (mysql stream terminators, pg multi-statement groups, redis pipeline/offline queue): needs real credentials and a real server. Alternative: gate 2 (crates/swiss-it behind `--features it`: testcontainers engines, or `SWISS_IT_*_URL` overrides — fail-not-skip per engine.rs, docs/44); in a credential-free environment, scripts/test-instance.ps1 + a local dev database do the live-verify; the SQL text contract is already pinned by the inline tests + dbbrowser_wiring.rs.
- **Secret-envelope cross-implementation compatibility**: tests/envelope_compat.rs already has four-way assertions; not duplicated in the MCP domain.
- **/api/db leases and the NotBrowsable text**: the consuming side belongs to swiss-data, see `.agents/docs/tests/data.md` (if that file is produced by a parallel task); the MCP side only keeps the catalog row keys and the seat lifecycle (the two W3 tests in plugin_host.rs).

## Priorities (P0 security & wire contracts / P1 main paths / P2 edge cases)

- **P0** (wire contracts and security shapes, currently with zero coverage):
  1. `toggling_a_tool_off_hides_it_from_the_clients_broadcast_list` (#9)
  2. `the_resources_master_switch_empties_the_list_and_persists` (#9)
  3. `a_lazy_mcp_is_woken_and_served_by_its_own_wakeup_request` (#1; lazy wakeup is the outward contract of the product's biggest memory feature)
  4. `a_failed_start_answers_503_jsonrpc_and_an_admin_error_respectively` (#1; the two-track error shapes)
  5. `token_routes_survive_an_mcp_plugin_disable` (#21; host-ownership behavior)
- **P1** (main paths):
  6. `restarting_an_mcp_serves_the_next_request_from_a_fresh_handler` (#1)
  7. `lifecycle_actions_persist_the_panel_stop_decision` (#13)
  8. `renaming_carries_the_call_log_and_the_old_path_stops_answering` (#14)
  9. `details_and_rename_and_delete_consult_the_tunnel_links` (#14/#23)
  10. `a_cursor_walk_pages_the_listing_to_its_end` (#11)
  11. `connection_tests_answer_honestly_for_a_dead_port_and_refuse_untestable_types` (#7)
  12. `a_rest_tool_round_trips_through_the_gateway` (#6) + `the_rest_connection_test_makes_one_plain_get_and_reports_ok`
  13. `panel_call_and_resource_report_failures_as_results_not_transport_errors` (#10)
  14. `rejects_reserved_names_an_empty_pg_url_and_a_bad_database_name` (#12)
  15. `import_answers_500_for_a_structurally_broken_file` (#15)
  16. `toggles_on_a_type_without_toggles_answer_501` (#9)
- **P2** (edge cases):
  17. `an_untouched_page_cache_is_reported_stale_by_the_pure_predicate` + `sweep_page_caches_drops_only_the_stale_slots` (#26)
  18. `traffic_filter_parameters_narrow_the_api_listing` (#19)
  19. `calls_listing_carries_the_stderr_companion_field` (#17)
  20. `details_report_state_reason_and_source_fields` (#23)
  21. `mcp_delete_without_a_bearer_is_refused` (#1)
  22. `mysql_browser_statements_match_the_wiring_contract` (#25); the live-database half of #7 belongs to gate 2 (swiss-it), not to a root test
- **Added by the 6b84f26 groups re-audit** (`default` becomes an ordinary group; the Data picker ranks by visual order) — numbering continues:
  23. `deleting_the_default_group_by_omission_moves_its_members_to_the_new_first_group` (#22, P1 — the sink-slot semantics through the API)
  24. `a_pre_v2_groups_file_serves_default_first_and_the_v2_marker_freezes_edits` (#22, P1 — the legacy-file migration of the groupsV2 marker)
  25. `the_db_picker_follows_the_sidebar_visual_order_across_interleaved_groups` (#27, P2)
  26. `assigning_default_explicitly_persists_a_real_entry_through_the_api` (#22, P2)

## Gap-Test Arrange/Act/Assert Details (ready to copy)

Below is a copy-ready version of every new P0/P1 test (based on the existing Harness in tests/adminapi.rs; ellipses stand for verbatim calls to existing helpers).

```rust
// #9 P0 — the broadcast list IS the contract
#[tokio::test]
async fn toggling_a_tool_off_hides_it_from_the_clients_broadcast_list() {
    let _lock = traffic_lock().await;
    let h = with_echo().await;                       // Arrange: one started echo MCP at /echo
    let (status, body) = h
        .post("/api/mcps/echo/tools/echo", json!({ "enabled": false }))
        .await;                                      // Act
    assert_eq!(status, StatusCode::OK);              // Assert: 200 + shape
    assert_eq!(body["tool"], json!("echo"));
    assert_eq!(body["enabled"], json!(false));
    assert_eq!(body["disabledTools"], json!(["echo"]));

    // The CLIENT's tools/list no longer carries it — the list is the contract.
    let (status, list) = h
        .mcp("/echo", TOKEN, json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["result"]["tools"].as_array().map(Vec::len), Some(0));

    // The panel's listing agrees and names what to re-enable.
    let (_, tools) = h.get("/api/mcps/echo/tools").await;
    assert_eq!(tools["tools"], json!([]));
    assert_eq!(tools["disabledTools"], json!(["echo"]));

    // Idempotent re-off answers unchanged, and the state is on disk.
    let (_, again) = h.post("/api/mcps/echo/tools/echo", json!({ "enabled": false })).await;
    assert_eq!(again["unchanged"], json!(true));
    assert_eq!(
        ManagedStore::open_at(h.path.clone()).disabled_tools("echo"),
        vec!["echo".to_string()]
    );

    // Re-enable: live on the very next request, no restart.
    h.post("/api/mcps/echo/tools/echo", json!({ "enabled": true })).await;
    let (_, list) = h
        .mcp("/echo", TOKEN, json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }))
        .await;
    assert!(list["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == json!("echo")));
}
```

```rust
// #9 P0 — the resources master switch
#[tokio::test]
async fn the_resources_master_switch_empties_the_list_and_persists() {
    let h = setup();
    h.register_fixture("res");
    h.registry.start("res").await.expect("start");

    let (status, body) = h
        .post("/api/mcps/res/resources-toggle", json!({ "enabled": false }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "enabled": false }));

    let (_, page) = h.get("/api/mcps/res/resources").await;
    assert_eq!(page["resources"], json!([]));
    assert_eq!(page["resourceEnabled"], json!(false));

    assert_eq!(
        ManagedStore::open_at(h.path.clone()).resource_enabled("res"),
        Some(false)
    );

    let (_, back) = h
        .post("/api/mcps/res/resources-toggle", json!({ "enabled": true }))
        .await;
    assert_eq!(back, json!({ "enabled": true }));
    let (_, page) = h.get("/api/mcps/res/resources").await;
    assert!(field_of(&page["resources"], "uri").contains(&"echo://res".to_string()));

    // A type with no resource half answers 501, naming the type.
    h.post("/api/mcps", json!({ "name": "r2", "type": "rest",
        "baseUrl": "https://example.invalid",
        "tools": [{ "name": "x", "request": { "method": "GET", "path": "/x" } }],
        "enabled": false })).await;
    let (status, body) = h
        .post("/api/mcps/r2/resources-toggle", json!({ "enabled": false }))
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert!(body["error"].as_str().unwrap_or_default().contains("rest"));
}
```

```rust
// #1 P0 — the wakeup request is the served request
#[tokio::test]
async fn a_lazy_mcp_is_woken_and_served_by_its_own_wakeup_request() {
    let _lock = traffic_lock().await;
    let h = setup();
    let (status, _) = h
        .post("/api/mcps", json!({ "name": "lz", "type": "echo", "lazy": true }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(row_named(&list, "lz")["lifecycle"], json!("idle"));

    let (status, body) = h
        .mcp("/lz", TOKEN, json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .await;
    assert_eq!(status, StatusCode::OK, "the wakeup request is served, not 503");
    assert!(body["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == json!("echo")));

    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(row_named(&list, "lz")["lifecycle"], json!("started"));
}
```

```rust
// #1 P0 — a failed start: JSON-RPC to the client, {error} to the panel
#[tokio::test]
async fn a_failed_start_answers_503_jsonrpc_and_an_admin_error_respectively() {
    let _lock = traffic_lock().await;
    let h = setup();
    // Port 1 on loopback: connection refused, immediately — no sleep, no timeout wait.
    h.register("dead", json!({ "type": "http", "url": "http://127.0.0.1:1/mcp" }));
    assert!(h.registry.start("dead").await.is_err());

    let (status, body) = h
        .mcp("/dead", TOKEN, json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["jsonrpc"], json!("2.0"));
    assert_eq!(body["error"]["code"], json!(-32603));
    assert!(body["error"]["message"].as_str().unwrap().contains("not started"));

    let (status, body) = h.get("/api/mcps/dead/tools").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].as_str().unwrap_or_default().contains("not started"));

    let (_, list) = h.get("/api/mcps").await;
    let row = row_named(&list, "dead");
    assert_eq!(row["lifecycle"], json!("error"));
    assert!(row["reason"].as_str().is_some(), "the row carries the reason: {row}");
}
```

```rust
// #21 P0 — host-owned token routes outlive the plugin
#[tokio::test]
async fn token_routes_survive_an_mcp_plugin_disable() {
    let (app, _host, _registry) = full_app("tok-owned", json!({})).await;
    let (status, _) = send(&app, local("POST", "/api/plugins/mcp/disable")).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body, _) = send(&app, local("GET", "/api/tokens")).await;
    assert_eq!(status, StatusCode::OK, "tokens are host-owned, not the plugin's");
    assert!(body.unwrap()["tokens"].as_array().unwrap().iter()
        .any(|t| t["label"] == json!("default")));

    let (status, _, _) = send(&app, local("GET", "/api/mcps")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
```

```rust
// #1 P1 — generation cache eviction, observed through behavior
#[tokio::test]
async fn restarting_an_mcp_serves_the_next_request_from_a_fresh_handler() {
    let _lock = traffic_lock().await;
    let h = with_echo().await;
    let frame = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    assert_eq!(h.mcp("/mcp/echo", TOKEN, frame.clone()).await.0, StatusCode::OK);

    h.post("/api/mcps/echo/stop", json!({})).await;
    let (status, body) = h.mcp("/mcp/echo", TOKEN, frame.clone()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);          // the stale handler is gone
    assert!(body["error"]["message"].as_str().unwrap().contains("not started"));

    h.post("/api/mcps/echo/start", json!({})).await;
    let (status, body) = h.mcp("/mcp/echo", TOKEN, frame).await;      // a FRESH generation serves
    assert_eq!(status, StatusCode::OK);
    assert!(body["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == json!("echo")));
}
```

```rust
// #13 P1 — the panel Stop decision is on disk, per source
#[tokio::test]
async fn lifecycle_actions_persist_the_panel_stop_decision() {
    let h = setup();
    h.register("cfg", json!({ "type": "echo" }));               // config-sourced
    h.registry.start("cfg").await.expect("start");
    let (status, body) = h.post("/api/mcps/cfg/stop", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lifecycle"], json!("stopped"));
    assert_eq!(ManagedStore::open_at(h.path.clone()).enabled_for("cfg"), Some(false));

    h.post("/api/mcps/cfg/start", json!({})).await;
    assert_eq!(ManagedStore::open_at(h.path.clone()).enabled_for("cfg"), Some(true));

    // Managed-sourced: the entry's own enabled flag flips.
    h.post("/api/mcps", json!({ "name": "m1", "type": "echo" })).await;
    h.post("/api/mcps/m1/stop", json!({})).await;
    let stored = ManagedStore::open_at(h.path.clone());
    let entry = stored.all().into_iter().find(|m| m.name == "m1").expect("stored");
    assert!(!entry.enabled);
}
```

```rust
// #14 P1 — the call log follows the rename; the old path is dead
#[tokio::test]
async fn renaming_carries_the_call_log_and_the_old_path_stops_answering() {
    let h = with_echo_named("old").await;
    h.post("/api/mcps/old/call", json!({ "tool": "echo", "arguments": { "msg": "m" } })).await;

    let (status, _) = h.post("/api/mcps/old/rename", json!({ "name": "new" })).await;
    assert_eq!(status, StatusCode::OK);

    let (_, calls) = h.get("/api/mcps/new/calls").await;
    assert_eq!(calls["calls"].as_array().map(Vec::len), Some(1), "history travelled");

    assert_eq!(h.get("/api/mcps/old/calls").await.0, StatusCode::NOT_FOUND);
    let (status, body) = h
        .mcp("/old", TOKEN, json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["message"], json!("Unknown MCP path: old"));
    assert_eq!(h.mcp("/mcp/new", TOKEN,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).await.0, StatusCode::OK);
}
```

```rust
// #11 P1 — a cursor walk to exhaustion (needs the PagedFixture aid)
#[tokio::test]
async fn a_cursor_walk_pages_the_listing_to_its_end() {
    let h = setup();
    let d = def(json!({ "type": "paged" }));
    let adapter: Arc<dyn Adapter> = Arc::new(DirectAdapter::new(
        &d, "p", PagedFixture { tools: 60 }, h.calls.clone()));
    h.registry.register("p", Source::Config, d, adapter).expect("register");
    h.registry.start("p").await.expect("start");

    let (status, p0) = h.get("/api/mcps/p/tools").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p0["pageSize"], json!(50));
    assert_eq!(p0["tools"].as_array().map(Vec::len), Some(50));
    let cursor = p0["nextCursor"].as_str().expect("a cursor").to_string();
    assert_eq!(p0["disabledTools"], json!([]));          // companion field present

    let (_, p1) = h.get(&format!("/api/mcps/p/tools?cursor={cursor}")).await;
    assert_eq!(p1["tools"].as_array().map(Vec::len), Some(10));
    assert!(p1.get("nextCursor").is_none(), "exhausted: absent, not null: {p1}");
}
```

```rust
// #7 P1 — honest connection tests, no credentials needed
#[tokio::test]
async fn connection_tests_answer_honestly_for_a_dead_port_and_refuse_untestable_types() {
    let h = setup();
    let (status, body) = h
        .post("/api/mcpdefs/test", json!({ "type": "mysql", "host": "127.0.0.1", "port": 1 }))
        .await;
    assert_eq!(status, StatusCode::OK);                    // the failure IS the answer
    assert_eq!(body["ok"], json!(false));
    assert!(body["ms"].as_u64().unwrap_or(9_999) < 5_000, "capped at 5s: {body}");
    assert!(!body["error"].as_str().unwrap_or_default().is_empty());

    for bad in ["echo", "proc"] {
        let (status, body) = h
            .post("/api/mcpdefs/test", json!({ "type": bad, "command": "node x" }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert!(body["error"].as_str().unwrap_or_default().contains("no connection test"));
    }
}
```

```rust
// #10 P1 — a failed run is a result, not a transport error
#[tokio::test]
async fn panel_call_and_resource_report_failures_as_results_not_transport_errors() {
    let h = with_echo().await;
    let (status, body) = h
        .post("/api/mcps/echo/call", json!({ "tool": "nope", "arguments": {} }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], json!(false));
    assert_eq!(body["isError"], json!(true));
    assert!(body["ms"].is_number());
    assert!(body["text"].as_str().unwrap_or_default().contains("unknown tool"));

    let g = setup();
    g.register_fixture("res2");
    g.registry.start("res2").await.expect("start");
    let (status, body) = g
        .post("/api/mcps/res2/resource", json!({ "uri": "bogus://nothing" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], json!(false));
}
```

```rust
// #14/#23 P1 — details/rename/delete consult the injected tunnel view
#[tokio::test]
async fn details_and_rename_and_delete_consult_the_tunnel_links() {
    let h = setup();                       // Harness keeps ctx (new field) — see aids
    h.register("x", json!({ "type": "echo" }));
    let links = Arc::new(FakeTunnelLinks::default());
    h.ctx.tunnel_links.write().unwrap().replace(links.clone());

    let (_, det) = h.get("/api/mcps/x/details").await;
    assert_eq!(det["tunnels"], json!(["t1"]));

    h.post("/api/mcps/x/rename", json!({ "name": "y" })).await;
    assert!(links.calls.lock().unwrap().iter()
        .any(|(k, v)| *k == "rename" && v == "x->y"));

    h.delete("/api/mcps/y").await;
    assert!(links.calls.lock().unwrap().iter()
        .any(|(k, v)| *k == "forget" && v == "y"));
}
```

```rust
// #26 P2 — the pure staleness predicate (paging.rs inline, no clock injection needed)
#[test]
fn an_untouched_page_cache_is_reported_stale_by_the_pure_predicate() {
    let c = PageCache::new();
    let now = swiss_core::util::now_ms();
    assert!(!is_page_cache_stale(&c, now));
    assert!(!is_page_cache_stale(&c, now + PAGE_CACHE_TTL_MS));
    assert!(is_page_cache_stale(&c, now + PAGE_CACHE_TTL_MS + 1));
}
```

The remaining P2 items (`traffic_filter_parameters_narrow_the_api_listing`, `calls_listing_carries_the_stderr_companion_field`, `details_report_state_reason_and_source_fields`, `mcp_delete_without_a_bearer_is_refused`, `import_answers_500_for_a_structurally_broken_file`, `rejects_reserved_names...`) are all direct expansions of the assertion essentials in the table above; Arrange needs only the existing `setup()` / `with_echo()` helpers — copy them in the same shape.
