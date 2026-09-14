# Data Integration Test Plan (functionality point → test matrix)

> Input: the "Feature Overview" table of the audit sub-document [.agents/docs/data.md](../data.md) (16 rows, L11-L26) and the "HTTP API and Routes" contracts (L30/L49/L51).
> Test baseline: root tests/ (app.rs, adminapi.rs, plugin_host.rs and 8 files in all) + per-crate inline #[cfg(test)].
> Route-prefix convention: **/api/db** (only the plugin id is called data; the /api/data/* from the task description does not exist, see audit L7).
> Test-shape discipline: oneshot (tower::ServiceExt), no real ports, no naked sleeps; every test binary sets MCP_GATEWAY_MASTER_KEY; live-database tests skip themselves without credentials (the two iron laws of docs/08).

## Existing Coverage Inventory (test file/module → test name list → corresponding functionality points)

### 1. crates/swiss-data/src/dbbrowser_api.rs `mod tests` (19 tests)

A port of the "data browser API" describe from Node's `dbbrowser.test.ts`: StubDb/StubRedis/StubProvider hung under the real `dbbrowser_router` and driven via oneshot. This is the mainline test of the /api/db route contract.

| Test name | Functionality point (audit line) |
| --- | --- |
| lists_only_browsable_mcps | #1 connection list (dialect none filter, label) |
| the_picker_ranks_by_the_sidebars_manual_order | #1 picker ranking (empty order → name order like /api/mcps; a manual order wins, names it never covers keep name-order slots at the end) |
| unknown_and_non_browsable_mcps_are_404 | #17 error contract (unknown MCP / has no database to browse, verbatim) |
| serves_paged_tables_and_rows | #2 table list, #3 row data page (shape: total/more/primaryKey/editable/columns) |
| forwards_the_tables_sort_and_rejects_unknown_keys | #2 sort forwarding + unknown-key 400 |
| forwards_filters_and_rejects_malformed_ones | #4 filter forwarding + malformed JSON/non-array 400 |
| serves_the_structure_detail | #5 table structure (columns/primaryKey/indexes/ddl) |
| streams_an_export_with_download_headers | #6 export (csv/json both formats, content-disposition, x-export-rows) |
| imports_csv_rows_and_rejects_malformed_payloads | #7 import (inserted, header/lines non-empty arrays, mapping length) |
| runs_structure_operations_and_rejects_unknown_ops | #8 DDL (truncate ok, unknown op 400) |
| forwards_edit_batches_and_rejects_empty_ones | #10 edit submission (results shape, empty batch 400) |
| runs_the_read_only_console | #9 SQL console (rowCount, empty sql 400) |
| serves_the_redis_key_browser | #1 (redis editable:false), #11 keys shape, #13 single key + empty key 400 + redis→SQL route 404 |
| runs_a_read_only_redis_command | #12 command console (reply shape, empty command 400) |
| non_sql_kinds_answer_the_flavored_404s | #17 cross-dialect 404 naming |
| no_provider_is_a_503_that_names_the_missing_plugin | #14/#17 no-provider 503 naming (disabled text) |
| a_provider_that_never_registered_is_a_generic_503 | #17 the generic 503 text for never-registered |
| a_draining_provider_refuses_with_a_stopping_503 | #14 withdrawal-period 503 (stopping text) |
| requests_release_their_leases_when_they_finish | #14 request-level lease return (tracker.outstanding()==0) |

### 2. Root tests/

| File: test name | Functionality point |
| --- | --- |
| plugin_host.rs:boot_disabled_plugins_guard_every_route_they_own | #18 plugin boundary (GET /api/db, GET /api/db/conn/table/rows?table=t → host 503 naming data/disabled) |
| plugin_host.rs:enabled_plugins_keep_every_api_shape_they_had | #18/#1 /api/db 200 + connections shape under the composition root |
| plugin_host.rs:route_ownership_is_longest_prefix_at_segment_boundaries | #18 /api/db belongs to the data plugin (segment boundary) |
| plugin_host.rs:inventory_shape_is_exact_and_the_mcp_plugin_started_the_mcps | #16 page contribution (data page order 40, entry /admin/js/views/data.js, no secondary tabs) |
| plugin_host.rs:disabling_mcp_503s_api_db_and_marks_datas_require_unmet | #15 requires/`requiresMet` flip with MCP start/stop; #14/#17 the 503 choreography under the composition root; Data's own disable goes through the host boundary |
| plugin_host.rs:the_catalog_seat_survives_a_hundred_mcp_restarts | #14 catalog seat through 100 start/stop rounds with zero leaks (a duplicate registration is an error) |
| app.rs:refuses_a_foreign_host_header / refuses_a_foreign_origin / oversized_mcp_body_is_refused | #18 loopback guard and the 2 MiB body limit (generic paths /health, /echo; **/api/db itself not pinned**) |

### 3. Supporting-layer inline tests (the pure-logic half of the functionality points; all count as "existing tests")

- **crates/swiss-host/src/dbbrowser.rs (31 tests)**: browse_paging_clamps (BROWSE_PAGE_SIZES/50/500), identifier_quoting (injection surface), read_paging_sql, browse_order_rules, filters_bind_comparisons_as_parameters / filters_keep_snowflake_ids_exact / filters_stack_with_and / contains_filters_escape_wildcards / is_null_filters_take_no_value / filters_refuse_unknown_input / filters_ride_along_in_rows_and_count / contains_on_typed_columns_casts_on_pg (#4 in full), edit_statements_bind_updates_by_full_primary_key / build_inserts_and_deletes / drop_unknown_columns / refuse_unaddressable_rows (#10 statement building), to_browse_columns_marks_pk_and_nullability / carries_comments, to_browse_indexes_folds_per_column_rows (#5), build_pg_ddl_sketches_the_catalog / omits_empty_pk_and_fk_clauses (#5 PG DDL sketch), with_explain_prefixes_once_and_strips_the_terminator (#9 Explain), ddl_ops_build_dialect_correct_statements (#8 dialect statements), numeric_bind_value_is_lossless_only (#7/#4 snowflake IDs), clamp_browse_limit_falls_back_and_caps (#2 table-page clamping), csv_helpers_round_trip (#6 RFC 4180), map_import_rows_keeps_snowflake_precision (#7), export_helpers (#6 constants), copy_out_helpers, browse_edit_of_parses_and_refuses (#10 edit parsing).
- **crates/swiss-host/src/services/catalog.rs (8 tests)**: a_duplicate_registration_is_refused_and_the_incumbent_stays, a_lease_counts_until_it_drops, withdrawal_refuses_new_leases_and_clear_releases_the_seat, drain_waits_for_lease_drops_and_reports_the_stragglers, wait_idle_is_woken_by_a_drop_not_just_the_timeout, a_hundred_lifecycles_leave_the_registry_exactly_empty, browserless_connections_are_not_browsable_but_are_named, two_definitions_sharing_host_and_port_stay_two_connections (#14 lease contract + connection-key identity rules).
- **crates/swiss-mcp/src/adapters/sql.rs (5 tests)**: reads_pass, writes_and_tricks_fail (#9 read-only shape: masking/multiple statements/INTO/CTE writes/EXPLAIN ANALYZE INSERT), row_limits (#9 with_row_limit), like_patterns, row_returning_shapes.
- **crates/swiss-mcp/src/adapters/mysql.rs (6 tests)**: connect_options_defaults, session_sql_gains_read_only_only_when_asked (#1 read-only session stamp), list_tables_sql_pairs_list_and_count, list_tables_sql_sorts_by_the_chosen_key_with_a_name_tiebreaker (#2), ping_ok_accepts_both_spellings, a_stream_that_never_terminates_is_an_error_not_an_empty_success.
- **crates/swiss-mcp/src/adapters/pg.rs (3 tests)**: url_parsing_matches_new_url, statement_labels, rows_in_an_open_group_at_stream_end_are_a_cut_not_a_result.
- **crates/swiss-mcp/src/adapters/pg_browser.rs (3 tests)**: total_of three states (#3 the node-pg difference in paged total).
- **crates/swiss-mcp/src/adapters/redis_browser.rs (8 tests)**: the scan_args family (#11 count clamping/string count/garbage count fallback/cursor/pattern passthrough/type allowlist lowercasing/unknown type named and rejected before dialing).
- **crates/swiss-mcp/src/adapters/redis.rs (5 tests)**: policy_rejects_the_named_harms, policy_lets_the_opt_ins_through, readonly_permits_reads_only (#12 assert_command_allowed full policy), flats_pair_and_scores_convert, read_windows_clamp (#13 partial).
- **crates/swiss-mcp/tests/dbbrowser_wiring.rs (5 tests)**: pg_table_list_supplies_every_placeholder (placeholder regression), pg_table_list_sort_rewrites_only_the_order_by + table_list_sort_refuses_unknown_keys_and_directions (#2 table-list sorts: the ORDER BY swap and the refuse-don't-default vetting), mysql_aliases_never_use_the_reserved_word_column, column_comment_wiring (#5 statement-level wiring).
- **crates/swiss-mcp/src/registry.rs (1 test)**: the_catalog_lists_one_row_per_entry_and_keys_them_by_definition (#1 RegistryCatalog keyed by definition, including the none row).
- **src/app.rs `mod tests` (2 tests)**: visual_order_slices_the_flat_rank_by_group, visual_order_keeps_unranked_names_last_in_name_order_inside_their_group (#1 picker visual order — flat rank sliced by stored groups, unranked names last in name order inside their group, unassigned names in the first group; src/app.rs:594-642).
- **mysql_browser.rs has no inline tests** (named at docs/08 L167: orchestration layer, belongs with the live-DB self-skipping tests).

## Functionality-Point → Test Matrix

Numbers #1-#16 correspond row by row to the audit's "Feature Overview" (L11-L26, not one row missing); #17-#19 are the three cross-cutting contracts of the "HTTP API and Routes" chapter (L30/L49/L51); #20-#21 split out the table-list-sort and picker-order points that landed with this port (amendments to audit rows #1/#2).

| # | Functionality point (audit line) | Existing tests | Gap | New test name (location) | Assertion essentials (method+path+status code+JSON fields/shape) | Special handling |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Browsable connection list (L11) | lists_only_browsable_mcps; the_picker_ranks_by_the_sidebars_manual_order (empty order → name order; manual rank wins, uncovered names last); app.rs visual_order 2 tests (group-sliced ranking); serves_the_redis_key_browser (redis row); registry.rs the_catalog_lists…; catalog.rs two_definitions_sharing_host_and_port… | Ordering behavior now pinned (name fallback + manual rank); readonly/state field passthrough still unpinned | connection_rows_carry_every_field (dbbrowser_api.rs inline) [G1] | GET /api/db → 200; mysql row readonly/state/editable field by field; redis row editable:false | None (stub) |
| 2 | Lazy-paged table list (L12) | serves_paged_tables_and_rows; forwards_the_tables_sort…; dbbrowser_wiring 5 tests (incl. both table-sort tests); mysql.rs list_tables_sql two tests (incl. the sort tiebreaker); dbbrowser.rs clamp_browse_limit… | grep/limit forwarding not asserted; table_page_args 200/1000 clamping at **zero tests** (sql.rs has no such test) | tables_forward_grep_page_and_limit_verbatim (dbbrowser_api.rs) [G2] + table_page_args_defaults_and_caps (sql.rs inline) [G15] | GET tables?grep=us&page=1&limit=500 → 200 and the stub receives {"grep":"us","page":"1","limit":"500"} (q_raw numeric fidelity); the unit test pins DEFAULT 200/MAX 1000/negative page to 0 | None |
| 3 | Row data page (L13) | serves_paged_tables_and_rows; read_paging_sql; browse_paging_clamps; pg_browser total_of 3 tests | offset/order/dir/schema forwarding and the q_non_empty empty-string semantics not asserted; editNote passthrough unpinned (the real text belongs to live DB) | data_forwards_paging_order_and_schema_with_the_node_semantics (dbbrowser_api.rs) [G3]; editNote/editable computation belongs to live DB [G27/G29] | GET data?table&schema&offset&limit&order&dir → 200 and opts verbatim-equal; schema=/filters= empty string = "not sent" | None |
| 4 | Field-level filters (L14) | forwards_filters_and_rejects_malformed_ones; dbbrowser.rs filters_* 8 tests | The MAX_FILTERS=16 boundary untested (17 entries should 400) | seventeen_filters_are_refused_but_sixteen_pass (dbbrowser_api.rs) [G4] | 16 entries → 200; 17 entries → 400 error containing "at most 16" | None; all values bound as parameters, already pinned by filters_bind… |
| 5 | Table structure (L15) | serves_the_structure_detail; to_browse_* 3 tests; build_pg_ddl 2 tests; wiring column_comment/mysql_aliases | foreignKeys field not asserted (minor); real SHOW CREATE/catalog assembly belongs to live DB | foreignKeys assertions fold into G11's RefusingDb control group, no separate entry needed; live DB G24/G28 pins the real DDL | (route layer already enough) schema?table → 200 {columns,primaryKey,indexes,foreignKeys,ddl} | Live-DB differences: mysql SHOW CREATE vs pg sketch, tested separately |
| 6 | Full-table export (L16) | streams_an_export_with_download_headers; export_helpers; csv_helpers_round_trip | **x-export-capped=1 untested**; format defaulting to csv and the no-table-name fallback untested | a_capped_export_sets_the_capped_header [G5], export_defaults_to_csv_and_falls_back_to_the_table_name (dbbrowser_api.rs) [G6] | capped stub → header "1"; default request → text/csv + filename="csv-table" | Chunked pulling (EXPORT_CHUNK) belongs to live DB |
| 7 | CSV import (L17) | imports_csv_rows_and_rejects_malformed_payloads; numeric_bind_value…; map_import_rows_keeps_snowflake_precision | **the over-10 000-row 400 untested**; mapping null=skip-column forwarding/semantics unpinned | imports_over_ten_thousand_rows_are_refused [G7], import_mapping_may_skip_columns_with_null (dbbrowser_api.rs) [G8] + map_import_rows_skips_null_and_empty_mappings (dbbrowser.rs inline) [G21] | 10001 rows → 400 "too many rows for one import (max 10000)"; mapping ["id",null] passed through with only id left as a bound column | Single-transaction rollback belongs to live DB [G26] |
| 8 | Structure operations DDL (L18) | runs_structure_operations_and_rejects_unknown_ops; ddl_ops_build_dialect_correct_statements | **rename missing to → 400 untested**; readonly refusal texts belong to the adapter (live DB); warn logs cannot be captured (see "Not suitable") | rename_without_to_is_refused (dbbrowser_api.rs) [G9]; the three readonly refusals belong to live DB [G27/G29] | POST ddl {"op":"rename","table":"users"} → 400 {"error":"rename needs the new table name"} (stub identical to build_ddl_op_sql:1203) | None |
| 9 | Read-only SQL console (L19) | runs_the_read_only_console; sql.rs reads_pass/writes_and_tricks_fail/row_limits | **assert_single_statement at zero tests**; limit_report (limitApplied/note) JSON shape at zero tests | single_statement_guard_trims_and_refuses_smuggled_ones [G16], limit_report_only_speaks_when_the_cap_bit (sql.rs inline) [G17]; end-to-end limitApplied belongs to live DB [G25/G29] | assert_single_statement("SELECT 1;")=="SELECT 1", ("SELECT 1; DROP…")→Err "multiple statements", ("SELECT ';'") Ok; limitReport: cap not engaged→{}, engaged→{"limitApplied":50}, caller-supplied limit→no note | The console read-only message replacement ("the Data view console is read-only…") lives in the adapter layer; live-DB G25 pins it |
| 10 | Buffered-edit transactional commit (L20) | forwards_edit_batches_and_rejects_empty_ones; edit_statements_* 4 tests; browse_edit_of_parses_and_refuses | **the over-1000-entry 400 untested** (MAX_EDITS, route side); first-error whole-batch rollback belongs to live DB | edit_batches_over_one_thousand_are_refused (dbbrowser_api.rs) [G10]; rollback belongs to live DB [G26/G29] | 1001 entries → 400 error containing "too many edits in one batch (max 1000)" | None |
| 11 | Redis key paging (L21) | serves_the_redis_key_browser; redis_browser scan_args 8 tests | keys parameter fidelity (pattern/count/type passthrough, empty cursor=not sent) not asserted; pipeline/DBSIZE belong to live DB | keys_forward_params_with_the_node_query_semantics (dbbrowser_api.rs) [G13]; live DB [G30] | GET keys?pattern=sess*&cursor=&count=500&type=HASH → 200 and opts=={"pattern":"sess*","count":"500","type":"HASH"} | count clamping 1..1000 already unit-tested; type allowlist 400 already unit-tested (before dialing) |
| 12 | Redis command console (L22) | runs_a_read_only_redis_command; redis.rs policy 3 tests | The guard-refusal→400-verbatim route mapping untested (folded into the G11 passthrough); **REPLY_CAP=1000 truncation at zero tests** | browser_errors_pass_through_verbatim_as_400 covers the command refusal [G11]; run_command_caps_array_replies_at_one_thousand (redis_browser.rs, purify cap_reply first) [G20]; guard verbatim text belongs to live DB [G30/G31] | cap_reply (a 1001-item array) → {"truncated":true,"note":"showing first 1000 of 1001","items":[…1000]} | cap_reply extraction follows the scan_args/total_of precedent (sanctioned at docs/08 L172-179) |
| 13 | Redis single-key read (L23) | serves_the_redis_key_browser (string type); flats_pair_and_scores_convert; read_windows_clamp | **type_aware_read six-type shapes/truncation/module-type guidance at zero tests** (a pub async fn over the RedisReadClient trait, fakeable) | type_aware_read_shapes_each_collection_type + type_aware_read_flags_truncation (redis.rs inline, FakeReadClient) [G18/G19]; live-DB measurement [G30] | string/hash/list/set/zset/stream each its own value shape; hash>1000 truncation marked truncated+note; module-type note contains "JSON.GET" | FakeReadClient implements the pub trait RedisReadClient, zero connections |
| 14 | Connection lease (L24) | requests_release_their_leases_when_they_finish; a_draining_provider…; catalog.rs 8 tests; plugin_host seat 100 rounds | The choreography of an in-flight request riding out a withdrawal (route level) untested | an_in_flight_page_survives_provider_withdrawal (dbbrowser_api.rs) [G14] | While a request hangs, begin_withdraw → new requests 503 "…is stopping…"; the hanging request eventually 200; tracker back to zero; after clear, 503 "…currently disabled" | No sleep: tokio::sync event gate + spawn oneshot |
| 15 | Plugin inventory dependency declaration (L25) | disabling_mcp_503s_api_db_and_marks_datas_require_unmet (flip + absent-not-null); inventory_shape… | **No gap** | — | — | — |
| 16 | Page contribution (L26) | inventory_shape_is_exact… (data page order 40/entry/no secondary tabs) | **No gap** | — | — | — |
| 17 | Error contract: driver passthrough/404 verbatim/503 naming/malformed JSON (L49) | unknown_and_non_browsable…; no_provider…; never_registered…; non_sql_kinds… | **Browser Err → 400 verbatim passthrough untested** (the module header comment's core contract); malformed JSON body → 400 untested | browser_errors_pass_through_verbatim_as_400 [G11], invalid_json_bodies_are_refused_before_the_handler (dbbrowser_api.rs) [G12] | All 10 endpoints × Err("driver said no") → 400 {"error":"driver said no"}; POST query body "not json" → 400 error starting with "invalid JSON body" | The NodeBody extractor takes effect even on the standalone router |
| 18 | Security boundary: loopback guard + 2 MiB + plugin boundary (L30) | boot_disabled_plugins… (data disabled 503); app.rs 3 tests (generic paths) | **/api/db's own loopback 403 and 413 unpinned** | api_db_refuses_foreign_hosts_and_origins [G22], an_oversized_api_db_body_is_refused_413 (tests/plugin_host.rs) [G23] | GET /api/db + Host evil.test → 403 "Host must name this machine"; foreign Origin → 403; POST import 2 MiB+1 → 413 "request body exceeds 2097152 bytes" | 413 must go through build_app(DefaultBodyLimit); P0 |
| 19 | Request-parameter fidelity q_non_empty/q_raw/coerced_str (L51) | forwards_the_tables_sort… (page/sort/dir); runs_the_read_only_console (coerced sql) | The fidelity assertions for the data/keys sides are scattered and missing | Folded into G3 (data) and G13 (keys) | See rows #3/#11 | None |
| 20 | Table-list sorting (L12 amendment: browse_table_sort + adapter ORDER BYs) | forwards_the_tables_sort_and_rejects_unknown_keys (route forwarding + stub-mirrored 400); dbbrowser_wiring pg_table_list_sort_rewrites_only_the_order_by + table_list_sort_refuses_unknown_keys_and_directions; mysql.rs list_tables_sql_sorts_by_the_chosen_key_with_a_name_tiebreaker | Empty-string sort/dir at the route unpinned (q_non_empty drops them → name-asc default, not a 400); real ordered output only provable against a live DB | tables_treat_empty_sort_and_dir_as_not_sent (dbbrowser_api.rs inline) [G32]; a sort=rows&dir=desc call asserting descending approxRows folded into G24/G28 (live) | GET tables?sort=&dir= → 200 and the stub receives neither key; live: the page comes back rows-descending | The stub already vets via browse_table_sort, so the 400 shape has a control; G32 pins the route's empty-string seam |
| 21 | Picker follows the sidebar order (L11 amendment: OrderSource + visual_order) | the_picker_ranks_by_the_sidebars_manual_order (route: flat rank + name fallback); app.rs visual_order_slices_the_flat_rank_by_group + visual_order_keeps_unranked_names_last_in_name_order_inside_their_group (pure fn) | Composition wiring untested: build_app actually feeding visual_order into the router, and a PUT /api/order reorder showing up on the next GET /api/db without a remount | put_order_reorders_the_picker_on_the_next_request (tests/plugin_host.rs) [G33] | full_app: GET /api/db → baseline order; PUT /api/order {"order":["db-b","db-a"]} → 200; GET /api/db again → the new order | Reuses full_app/send; no remount between the calls is the whole point |

## Gap-Test Arrange / Act / Assert (ready to copy)

All of the following land in existing test files, with style and naming aligned to the existing suites. G1-G14 and G32 go inside `crates/swiss-data/src/dbbrowser_api.rs`'s `mod tests` (reusing StubDb/StubRedis/StubProvider/router_of/db_entry/redis_entry/call/urlencode and the Seen recorder); G15-G17 in `crates/swiss-mcp/src/adapters/sql.rs` `mod tests`; G18-G19 in `crates/swiss-mcp/src/adapters/redis.rs` `mod tests`; G20 in `crates/swiss-mcp/src/adapters/redis_browser.rs` `mod tests`; G21 in `crates/swiss-host/src/dbbrowser.rs` `mod tests`; G22-G23 and G33 in `tests/plugin_host.rs` (reusing full_app/send); G24-G31 are the new file `crates/swiss-mcp/tests/db_live.rs`.

**G1 connection_rows_carry_every_field**
- Arrange: `StubDb` gains a `readonly: bool` field (`readonly()` returns it); `router_of(vec![db_entry("zeta", StubDb{readonly:false}), db_entry_readonly("mid"), redis_entry("alpha")])` (an empty sidebar order — the ordering half is already pinned by the_picker_ranks_by_the_sidebars_manual_order).
- Act: `call(app, "GET", "/api/db", None)`.
- Assert: 200; the "mid" row `readonly == true`, `editable == true`, `state == "stopped"`, `dialect == "mysql"`, `label == "stub @ localhost"`; the "alpha" row `editable == false`.

**G2 tables_forward_grep_page_and_limit_verbatim**
- Arrange: a seen-recording StubDb.
- Act: `call(app, "GET", "/api/db/db/tables?grep=us&page=1&limit=500", None)`.
- Assert: 200; `seen.tables_opts == Some(json!({"grep":"us","page":"1","limit":"500"}))` — q_raw: numeric parameters get their own Number() cast on the browser side via js_number, and the route forwards the strings verbatim.

**G3 data_forwards_paging_order_and_schema_with_the_node_semantics**
- Arrange: StubDb's `read_table` additionally records `seen.data_opts = Some(o.clone())`.
- Act 1: `GET /api/db/db/data?table=users&schema=app&offset=10&limit=20&order=id&dir=desc`.
- Assert 1: 200; `data_opts == {"table":"users","schema":"app","offset":"10","limit":"20","order":"id","dir":"desc"}`.
- Act 2: `GET /api/db/db/data?table=users&schema=&filters=`.
- Assert 2: `data_opts == {"table":"users"}` — q_non_empty treats an empty schema as "not sent" and parse_filters treats an empty string as "not sent" (dbbrowser_api.rs:126/154).

**G4 seventeen_filters_are_refused_but_sixteen_pass**
- Arrange: two independent routers (each with a fresh seen); terms(n) = (0..n) of `json!({"column":"id","op":"eq","value":i})`.
- Act: `GET /api/db/db/data?table=users&filters=` + urlencode(terms(16).to_string()) → 200; terms(17) → 400.
- Assert: the 17-entry error contains `"at most 16"` (MAX_FILTERS, dbbrowser_api.rs:116/161-164); with 16 entries seen.filters has length 16.

**G5 a_capped_export_sets_the_capped_header**
- Arrange: CappedDb (export_table always returns `{"format":"csv","columns":["id"],"rows":100000,"capped":true,"body":"id\r\n1"}`).
- Act: `GET /api/db/db/export?table=users&format=csv`.
- Assert: 200; `headers["x-export-capped"] == "1"`; `headers["x-export-rows"] == "100000"`; content-type contains text/csv. The control group uses the existing StubDb (capped:false) asserting "0" (the existing test pinned only rows, not capped).

**G6 export_defaults_to_csv_and_falls_back_to_the_table_name**
- Arrange: a seen-recording StubDb (export_table additionally records opts).
- Act: `GET /api/db/db/export` (no query at all).
- Assert: content-type contains text/csv (format defaults to csv); content-disposition == `attachment; filename="csv-table"` (q_or_empty falls an empty table name back to "table", dbbrowser_api.rs:354); the stub receives `{"table":"","format":"csv"}`.

**G7 imports_over_ten_thousand_rows_are_refused**
- Arrange: any StubDb router.
- Act: POST `/api/db/db/import`, body `{"table":"t","header":["id"],"lines":[10001×"1"],"mapping":["id"]}` (build lines with `Value::Array((0..10001).map(|_| json!("1")).collect())`).
- Assert: 400; `error == "too many rows for one import (max 10000)"` (IMPORT_ROW_CAP, dbbrowser_api.rs:435-439).

**G8 import_mapping_may_skip_columns_with_null**
- Arrange: a seen-recording StubDb.
- Act: POST import, `{"table":"t","header":["id","junk"],"lines":["1,x"],"mapping":["id",null]}`.
- Assert: 200; `body == {"inserted":1}`; `seen.imported["mapping"] == json!(["id", null])` (a null column=skip; mapping validation allows null, dbbrowser_api.rs:441-449).

**G9 rename_without_to_is_refused**
- Act: POST `/api/db/db/ddl`, `{"op":"rename","table":"users"}`.
- Assert: 400; `error == "rename needs the new table name"` — the stub's branch is verbatim-identical to the real build_ddl_op_sql (dbbrowser.rs:1202-1204); pin it at the route layer first, and test the real adapter path against a live DB.

**G10 edit_batches_over_one_thousand_are_refused**
- Act: POST `/api/db/db/edits`, `{"table":"t","edits":[10001? No — 1001×{"op":"delete","pk":{"id":1}}]}`.
- Assert: 400; error contains `"too many edits in one batch (max 1000)"` (MAX_EDITS, dbbrowser_api.rs:112/526-530).

**G11 browser_errors_pass_through_verbatim_as_400**
- Arrange: `struct RefusingDb;` (every DbBrowser method returns `Err("driver said no".into())`) and `struct RefusingRedis;` (the same for its three methods); build a router for each.
- Act/Assert (each asserts status==400 and body==`{"error":"driver said no"}`):
  - GET `/api/db/db/tables` / `/api/db/db/data?table=t` / `/api/db/db/schema?table=t` / `/api/db/db/export?table=t`;
  - POST `/api/db/db/import` / `/api/db/db/ddl` / `/api/db/db/query` (sql:"SELECT 1") / `/api/db/db/edits` (edits:[{op:"update",pk:{id:1},changes:{}}]);
  - GET `/api/db/rdb/keys`; POST `/api/db/rdb/command` (command:"GET k"); GET `/api/db/rdb/key?key=k`.
- Cross-assertion: GET `/api/db/rdb/tables` still 404 "MCP 'rdb' (redis) has no database to browse" (the dialect 404 takes precedence over browser errors).

**G12 invalid_json_bodies_are_refused_before_the_handler**
- Act: POST `/api/db/db/query`, raw body bytes `"not json"` (no content-type also works — NodeBody ignores Content-Type, reply.rs:31-32).
- Assert: 400; error starts with `"invalid JSON body"` (reply.rs:59-64; the extractor refuses before the handler).

**G13 keys_forward_params_with_the_node_query_semantics**
- Arrange: StubRedis additionally records `seen.keys_opts` (list_keys records o.clone() up front; readonly/label as before).
- Act: `GET /api/db/cache/keys?pattern=sess*&cursor=&count=500&type=HASH`.
- Assert: 200; `keys_opts == {"pattern":"sess*","count":"500","type":"HASH"}` — the empty cursor is dropped by q_non_empty (=scan from the start), count goes through q_raw as the verbatim string, and a non-empty type is passed through (dbbrowser_api.rs:549-567).

**G14 an_in_flight_page_survives_provider_withdrawal**
- Arrange: `struct SlowDb { gate: tokio::sync::watch::Receiver<bool> }`, whose list_tables awaits `gate.changed().await` first and then returns a normal page; new helper `fn router_with_catalog(rows) -> (Router<()>, Arc<CatalogRegistry>, Arc<LeaseTracker>)` (like catalog_router_of but returning the catalog and tracker).
- Act: `let inflight = tokio::spawn(call(app.clone(), "GET", "/api/db/db/tables", None))`; `tokio::task::yield_now().await` to let it hang; `catalog.begin_withdraw()`; a new GET tables → 503 "the mcp plugin is stopping; database browsing is momentarily unavailable"; `gate.send(true)` to release it.
- Assert: `inflight.await`'s status == 200 (the in-flight request is not hard-preempted); afterwards `tracker.outstanding() == 0`; after `catalog.clear()`, GET `/api/db` → 503 "the mcp plugin provides database connections and is currently disabled".
- Special handling: fully event-driven (watch channel), no sleep, no real port; under the single-threaded current_thread runtime, spawn+yield_now is enough for the in-flight request to take the lease first.

**G15 table_page_args_defaults_and_caps (sql.rs)**
- Assert: `table_page_args(None, None) == TablePage { page: 0, limit: 200, offset: 0 }` (DEFAULT_TABLE_LIMIT); `table_page_args(Some(json!(5000)), Some(json!(2))) == { page: 2, limit: 1000, offset: 2000 }` (MAX_TABLE_LIMIT); page "-3" and "junk" both collapse to 0; `DEFAULT_TABLE_LIMIT == 200 && MAX_TABLE_LIMIT == 1000` pinned in passing.

**G16 single_statement_guard_trims_and_refuses_smuggled_ones (sql.rs)**
- Assert: `assert_single_statement("SELECT 1;\n") == Ok("SELECT 1")`; `assert_single_statement("SELECT 1; DROP TABLE t")`'s Err contains "multiple statements"; `assert_single_statement("SELECT ';'") == Ok("SELECT ';'")` (a semicolon inside the mask is not a delimiter); trailing whitespace is also Ok.

**G17 limit_report_only_speaks_when_the_cap_bit (sql.rs)**
- Arrange: `let hit = with_row_limit("SELECT * FROM big", 200);` (limit_applied==Some(200)).
- Assert: `limit_report(&hit, 50, None) == json!({})` (silent when the cap did not bite); `limit_report(&hit, 200, Some(json!(200))) == {"limitApplied":200}` (the caller supplied its own limit → no explanation); `limit_report(&hit, 200, None)` contains both limitApplied and note; `let multi = with_row_limit("SELECT 1; SELECT 2", 200);` → `limit_report(&multi, 0, None)` has only the note, no limitApplied (the three branches at sql.rs:368-392).

**G18 type_aware_read_shapes_each_collection_type (redis.rs, FakeReadClient)**
- Arrange: `struct FakeReadClient { replies: Vec<(String, Value)> }` implementing the pub trait `RedisReadClient` (type_of/ttl/get/llen/lrange/hlen/scard/zcard/xlen/call replay from the scripted queue; scan_round goes through call("HSCAN"/"SSCAN",…)).
- Act/Assert: for each type, call `type_aware_read(&fake, "k", 0, 1000)`:
  - string (TYPE→"string", TTL→-1, GET→Some("hello")) → `{"key":"k","type":"string","ttl":-1,"value":"hello"}`;
  - list (LLEN→1001, LRANGE→1000 items) → value with 1000 items, length 1001, truncated true, note present;
  - hash (HLEN→5, HSCAN→[cursor "0", flat 10]) → value is a field object;
  - set (SSCAN, same route) → value is an array;
  - zset (ZCARD→2, ZRANGE WITHSCORES flats) → value is [{member,score}…];
  - stream (XLEN→1, XRANGE flats) → value is [{id,fields}…];
  - module type (TYPE→"ReJSON-RL") → note contains `"JSON.GET"` (the guidance text at redis.rs:608-614).
- Script according to the actual command sequences at redis.rs:445-619; assertions pin only the shapes and boundary values of key/type/ttl/value/truncated/note, not internal ordering.

**G19 type_aware_read_flags_truncation (redis.rs)**
- Assert: the hash/set collection cap of 1000 (HLEN→2000, SCAN returning 1000 per round) → truncated true with the note text present; the stream window clamp (offset+window < length → truncated); the `read_window(&json!({"offset": 5, "limit": 10}))` semantics re-verified in passing.

**G20 run_command_caps_array_replies_at_one_thousand (redis_browser.rs)**
- Pre-refactor: lift the truncation at redis_browser.rs:189-197 into a pure function `fn cap_reply(reply: Value) -> Value` (run_command calls it instead; following the purification precedent of scan_args/total_of, docs/08 L172-179).
- Assert: `cap_reply(json!([1001 items])) == {"truncated":true,"note":"showing first 1000 of 1001","items":[the first 1000 items]}`; 1000 items and non-arrays (scalars/objects) return unchanged.

**G21 map_import_rows_skips_null_and_empty_mappings (dbbrowser.rs)**
- Assert: `map_import_rows(&["id","junk"], &["1,x"], &["id", null])` and `&["id", ""]` both bind only the id column (both None and "" skip — the Node `if (!col)` at dbbrowser.rs:1322).

**G22 api_db_refuses_foreign_hosts_and_origins (tests/plugin_host.rs)**
- Arrange: `let (app, _, _) = full_app("db-guard", json!({})).await`.
- Act: `send(&app, Request::get("/api/db").header(header::HOST, "evil.test:19999").body(Body::empty()))` → 403, error contains "Host must name this machine"; then with HOST 127.0.0.1:19999 + ORIGIN http://evil.test → 403.
- This pins the app.rs generic-guard assertions onto the Data tree (routes merged into build_app must sit inside the guard; the comment at server.rs:254-258 worries about exactly such a leak).

**G23 an_oversized_api_db_body_is_refused_413 (tests/plugin_host.rs)**
- Arrange: full_app; body = `"x".repeat(swiss::app::BODY_LIMIT + 1)`.
- Act: POST `/api/db/db/import` (with HOST 127.0.0.1:19999, any content-type).
- Assert: 413; `error == "request body exceeds 2097152 bytes"` (DefaultBodyLimit→NodeBody 413, reply.rs:47-55).

**G24-G31 live-database tests (new file crates/swiss-mcp/tests/db_live.rs; the three env vars skip independently)**

The iron law, made concrete: every test opens with `let Ok(url) = std::env::var("SWISS_TEST_LIVE_MYSQL_URL") else { return; }` (PG/REDIS likewise, separate variables and separate skips — the mysql/pg/redis capability differences are asserted independently, with no cross-dependence); a file-level `sandbox()` sets MCP_GATEWAY_MASTER_KEY + a scratch home in one shot (copied from tests/adminapi.rs:34-49). Driving: not over HTTP (the routes are already covered by stubs) — go straight to `make_adapter(&def, "live", &calls)` → `adapter.browser()` for BrowserFlavor::Db/Redis (swiss-mcp's Adapter::browser hook, mod.rs:272-274) and call the real browser methods — no ports, no sleeps. Seed data connects directly via sqlx/redis (already among swiss-mcp's main dependencies; listing the same versions in dev-dependencies adds zero new compile cost).

- **G24 live_mysql_browses_tables_rows_schema_and_export**: build a pool, DROP/CREATE TABLE swiss_data_test(id BIGINT PRIMARY KEY, name VARCHAR(64)), insert 2 rows; list_tables({"page":"0"}) → tables contains the table, limit==200 (default page); read_table(limit "10") → columns[0].isPrimaryKey==true, editable==true, rows.len()==2, total==2; describe_table → ddl starts with "CREATE TABLE" (SHOW CREATE TABLE), foreignKeys array present; export_table(csv) → body contains "id,name", capped==false.
- **G25 live_mysql_console_is_read_only_and_reports_the_applied_limit**: build a 60-row table; run_query("SELECT * FROM swiss_data_test", None) → rowCount==50, `limitApplied == 50`, note present (no limit passed); run_query (same statement, Some(json!(60))) → rowCount==60 and no limitApplied; run_query("DELETE FROM swiss_data_test", None) → Err containing "the Data view console is read-only"; run_query("SELECT 1; DROP TABLE swiss_data_test", None) → Err (the shape check refuses multiple statements).
- **G26 live_mysql_edits_commit_atomically_and_import_round_trips**: apply_edits([insert, update by pk, delete by pk]) → results {op,affected:1} per entry, and read_table verifies the end state; then send [a good update, an update whose pk names a nonexistent column] → Err and read_table identical to the previous end state (the first error rolls back the whole batch); import_table with a 3-row CSV → {inserted:3} and the data visible. Finish with DROP.
- **G27 live_mysql_readonly_connections_refuse_writes_at_the_right_layer**: add "readonly":true to the def; read_table → editable==false and editNote=="this MCP is configured readonly"; apply_edits → Err containing "editing is disabled"; import_table → Err containing "import is disabled"; ddl_op(truncate) → Err containing "structure operations are disabled" (verbatim at mysql_browser.rs:258-260/344-346/412-414); run_query("SELECT 1") still Ok.
- **G28 live_pg_browses_with_the_catalog_ddl_sketch**: SWISS_TEST_LIVE_PG_URL; CREATE TABLE public.swiss_data_test(id BIGINT PRIMARY KEY, name TEXT) + one foreign key + column comments; list_tables (schema filter) → rows carry approxRows/size; describe_table → ddl assembled by build_pg_ddl (containing "CREATE TABLE", the col_description comments, foreignKeys folded); read_table → total goes through total_of's numeric path.
- **G29 live_pg_console_and_edits_follow_the_same_contracts**: run_query SELECT → limitApplied semantics as in G25 (PG-side, the default_transaction_read_only=on read-only session does not block SELECT); EXPLAIN SELECT → Ok; WITH DELETE → refused; apply_edits updating by pk → affected; readonly def → the PG refusal texts corresponding to G27 (pg_browser.rs is isomorphic).
- **G30 live_redis_keys_command_and_type_aware_reads**: SWISS_TEST_LIVE_REDIS_URL (redis://127.0.0.1:6379/15 suggested, a db the tests own exclusively); connecting directly, FLUSHDB, then SEED string/hash (1001 fields)/list/zset each carrying the swiss_t: prefix; list_keys({"pattern":"swiss_t:*"}) → keys carry type/ttl, total==DBSIZE, done==false when the first-page cursor is non-zero; list_keys({"type":"bogus"}) → Err listing the allowed types (scan_args refuses before dialing); read_key("swiss_t:h") → type hash, value exactly 1000 fields, truncated==true, note present; run_command("GET swiss_t:s") → "…"; run_command("KEYS *") → Err containing "rejected" (the guard's verbatim text); run_command("LRANGE swiss_t:l 0 -1") → an array; finish with FLUSHDB.
- **G31 live_redis_readonly_permits_reads_only**: def "readonly":true; run_command("GET k") → Ok; run_command("SET k v") → Err containing "configured readonly"; run_command("EVAL \"return 1\" 0") (no allowEval) → Err containing "allowEval".

**G32 tables_treat_empty_sort_and_dir_as_not_sent (dbbrowser_api.rs)**
- Act: `GET /api/db/db/tables?sort=&dir=` (both keys present, both values empty).
- Assert: 200; `tables_opts` carries neither `sort` nor `dir` — q_non_empty drops empty strings before the browser sees them, so browse_table_sort's name-asc default applies instead of a 400; the contrast case (`?sort=evil` → 400 "unknown key") is already pinned by forwards_the_tables_sort_and_rejects_unknown_keys.

**G33 put_order_reorders_the_picker_on_the_next_request (tests/plugin_host.rs)**
- Arrange: `full_app` with two browsable db MCPs (say "db-a", "db-b"); `send(GET /api/db)` → names == ["db-a","db-b"].
- Act: `send(PUT /api/order, {"order":["db-b","db-a"]})` → 200 `{"order":[…]}`; then `send(GET /api/db)` again on the same app — no remount, no rebuild.
- Assert: the second GET's names == ["db-b","db-a"] — the OrderSource is a closure read per request (dbbrowser_api.rs:45-47), so the store write reorders the picker on the very next poll; this also pins the build_app wiring (src/app.rs:485-509) that the app.rs inline visual_order tests leave untested.

## New Test Helpers Needed (builder/seed/fixture, signatures)

All extend existing test modules; no new product-code visibility (the cap_reply extraction is the only item that touches product code):

1. **dbbrowser_api.rs tests**:
   - `struct StubDb { seen: SeenRef, readonly: bool }` (`readonly()` returns the field; add false at the existing construction sites); `fn db_entry_readonly(name: &str) -> Arc<StubRow>`.
   - `struct RefusingDb;` / `struct RefusingRedis;` — every trait method `Err("driver said no".into())`; `fn refusing_db_entry(name) / refusing_redis_entry(name) -> Arc<StubRow>`.
   - `struct CappedDb;` — export_table returns `{"format":"csv","columns":["id"],"rows":100000,"capped":true,"body":"id\r\n1"}`.
   - `struct SlowDb { gate: tokio::sync::watch::Receiver<bool> }`; `fn router_with_catalog(rows: Vec<Arc<StubRow>>) -> (Router<()>, Arc<CatalogRegistry>, Arc<LeaseTracker>)`.
   - `Seen` gains `data_opts / keys_opts / export_opts: Option<Value>`; StubRedis::list_keys and StubDb::read_table/export_table record them up front.
2. **redis.rs tests**: `struct FakeReadClient { script: Vec<(String, Value)> }` implementing `RedisReadClient` (dequeue and replay by command name; `fn scripted(pairs: Vec<(&str, Value)>) -> FakeReadClient`).
3. **redis_browser.rs**: purify `fn cap_reply(reply: Value) -> Value` (the REPLY_CAP truncation); run_command calls it instead.
4. **crates/swiss-mcp/tests/db_live.rs**: `fn live_url(var: &str) -> Option<String>` (missing → the whole test returns, self-skipping); `async fn mysql_browser_over(url: &str, extra: Value) -> Arc<dyn DbBrowser>` / pg / redis (via make_adapter + browser()); seed functions `async fn seed_mysql(url: &str, ddl: &str)` and `async fn seed_redis(url: &str, pairs: &[(&str, &str)])` (direct sqlx/redis connections); file-level `fn sandbox()` copying adminapi.rs's OnceLock set_var pattern (including MCP_GATEWAY_MASTER_KEY). dev-dependencies must list sqlx/mysql, sqlx/postgres, and redis in swiss-mcp's Cargo.toml (same versions as the main dependencies, zero growth of the build graph).
5. **tests/plugin_host.rs**: no new helpers needed (G22/G23 reuse full_app/send; the body is built directly with Request::builder).

## Points Not Suitable for Integration Tests, and Alternatives

| Functionality point | Why it is not suitable | Alternative |
| --- | --- | --- |
| The full panel-interaction set (buffered-edit amber bar/SQL preview mirror/dialect-aware copy/300ms debounce/localStorage history/Shift range select) | admin_assets is byte-for-byte frozen; the panel JS is the spec, not the thing under test (umbrella plan §7) | Node-side vitest (the admin-pages/admin-panel suites in ../local-mcp-gateway); this repo relies on ADR-009 shape tests + the byte-equivalence guard |
| Run logs (data view import info / ddl **warn** / commit info, audit L81) | log::log goes straight to println! (log.rs:26-28); stdout cannot be captured inside the test process; the line shapes already have log.rs inline unit tests | Start an isolated instance with scripts/test-instance.ps1, then live-verify while watching the 19998 logs |
| The server-side version of with_explain (audit L127) | The panel client already mirrors it (data-sql.js); the server-side version exists only as a test anchor; unit tests exist | Keep as is (dbbrowser.rs with_explain_prefixes_once…); add no integration test |
| The pool-sharing invariant (browser and MCP tools share one Arc<Lazy<Pool>>, audit L87) | Guaranteed by the type system (cloning an Arc builds no new pool); under oneshot, "the same pool" cannot be observed without opening a real connection | Trust the compiler on the type-level fact; if behavioral evidence is wanted, consecutive calls on the same browser in the G24-G31 live-DB tests implicitly verify pool reuse |
| The drain 5-second timeout's warn("closing with unreleased connection leases") | A real 5 s wait; the catalog.rs drain_waits…/wait_idle… unit tests already cover the semantics with injectable waits | The inline unit tests suffice; the compose-level behavior is approximated by G14's withdrawal choreography test |
| Windows FFI/platform differences | Unrelated to the Data domain | — |

## Priorities (P0 security & wire contracts / P1 main paths / P2 edge cases)

- **P0 (security & wire contracts)**: G22 (the /api/db loopback guard — pinning the security boundary onto the Data tree), G23 (the 413 body limit), G11 (driver error text passed through verbatim — the module header's core wire contract), G12 (the malformed-JSON 400 shape). All four are error/security shapes the panel JS as spec depends on, and all four are exactly the kind most likely to slip through silently after merging into build_app.
- **P1 (main paths)**: G1, G2, G3, G4 (filter cap), G5, G6, G7 (import cap), G8, G9, G10 (edit cap), G13, G15 (table_page_args), G16 (assert_single_statement), G17 (limit_report), G18, G19 (type_aware_read six types), G20 (REPLY_CAP), G21, G33 (the picker's live reorder through the composition root), G24-G31 (the whole live-DB family: mysql/pg/redis differences asserted separately, self-skipping without credentials). Of these, G15/G16/G17/G18/G20 are pure-logic blanks in the existing suites — lowest cost, most direct payoff; G24-G31 fill in the "browser-purification residue" named at docs/08 L172-179.
- **P2 (edge cases)**: G14 (the choreography of an in-flight request riding out a withdrawal — the catalog side already covers the semantics; the route level is hardening), the control-group refinement of G5, the filename-fallback details of G6, G32 (empty sort/dir at the route). G1's former ordering half is now covered by the_picker_ranks_by_the_sidebars_manual_order and dropped from G1.

With all the gaps above closed, the 16 Feature Overview rows + 3 cross-cutting contract rows + the 2 split-out sort/picker rows (#20/#21) all land in at least one layer of "root tests/ end-to-end or crate inline", most in two layers (route stub + pure logic; live DB makes three).
