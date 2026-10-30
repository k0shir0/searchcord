# Rust parity ledger

Python reference: `481f62c6d1e111ca6f5771554a0e89ecd42b088e`.
Rust owns scenarios, fixtures and assertions; Python is a temporary call adapter.
No production Rust backend exists yet. Selecting it must fail explicitly.

| Chunk | Source coverage | Rust owner/contracts | Status/evidence |
|---|---|---|---|
| 01 | Original 36 Python cases, browser queue, JS syntax | Baseline / cross-platform CI | Linux: 36 Python + 25 Node pass; JS syntax pass. |
| 02 | Disposable archives, process control, HTTP, SQLite, traces, network isolation | `searchcord-parity/tests/harness.rs`: `disposable_archives_control_reference_and_verify_fts5_trigram`, `real_http_event_and_command_traces_are_collected`, `driver_blocks_live_network_and_path_escape`, `backend_selection_rejects_unfinished_rust_instead_of_skipping`, `dropping_harness_joins_reference_and_removes_temporary_archives` | Linux: all five pass; formatting and Clippy pass. SSE fixture verifies transport capture only; production job event contracts remain in 06–09. |
| 03 | Seven `tests/test_archive_storage.py` cases | `searchcord-parity/tests/archive_storage.rs` | Linux: all 7 pass, plus 5 harness tests; original 36 Python + 25 Node remain green. |
| 04 | Nine `tests/test_search_display.py` cases | `searchcord-parity/tests/search_display.rs` | Linux: all 9 pass over real HTTP, including forced empty partial scan pages; formatting/Clippy pass. |
| 05 | Permissions and discovery | Future test-first chunk | Pending |
| 06 | All nine `tests/test_durable_scrape.py` cases | `searchcord-parity/tests/durable_scrape.rs` | Linux: nine pass with controlled fetch/commit/credential barriers; all 30 Rust contracts, 36 Python and 25 Node pass. |
| 07 | All five `tests/test_scrape_pacing.py` cases | `searchcord-parity/tests/scrape_pacing.rs` | Linux: all five pass with supplied clocks, draws and responses; 35 Rust contracts, 36 Python, 25 Node pass. |
| 08 | `snapshot_codec.py`, `search_snapshot.py`, packed reader and media publication | `searchcord-parity/tests/packed_snapshot.rs` | Linux: seven contracts pass; 42 total Rust tests, original Python/Node checks green. |
| 09–10 | Remaining routes, browser/performance | Future test-first chunks | Pending; milestone not yet reached |
| 11–38 | Production Rust implementations and qualification | Future packages | Unimplemented |

## Storage mapping (03)

Each Python method below is `ArchiveStorageTests.test_<name>`; its Rust contract
has the same name without `test_`, in `tests/archive_storage.rs`.

| Name | Independently asserted behavior |
|---|---|
| `native_credentials_migrate_out_of_sqlite_and_backup` | Exact migrated credential/active ID; sealed bytes; sanitized recovery SQLite pages and integrity; unchanged plaintext source; clear/load. |
| `portable_vault_fails_closed_when_key_is_missing` | Fernet header, exact round trip, missing-key load/save failure, unchanged ciphertext, explicit clearing. |
| `vault_cannot_be_captured_with_archive_directory` | CredentialError and no vault file created inside the archive directory. |
| `exact_payload_edits_deletions_fts_and_counts` | Exact payload and unknown fields, duplicate observations, edits, tombstone/restoration, stable rowid, revision pagination, FTS integrity and delete triggers/counts. |
| `legacy_attachment_migration_preserves_all_original_values` | Schema v10, historical names/timestamp/content, original attachment string, gzip recovery integrity, legacy revision, exact new payload, idempotence. |
| `old_history_attachments_survive_timestamp_compression` | v8 history attachment migration, exact overridden timestamp, stable rowid/content. |
| `invalid_message_rolls_back_payload_and_search_together` | Wrong-channel exception, explicit rollback after a valid write, unchanged payload/history/search/counts. |

## Reader/display mapping (04)

Each Python method below is `DisplayTests.test_<name>`; its Rust contract has
the same name without `test_`, in `tests/search_display.rs`.

| Name | Independently asserted behavior |
|---|---|
| `summary_and_readonly_surface` | Exact totals; permitted assets; unavailable collector routes; methods/headers; actual reader write refusal; unchanged database bytes and no credential/WAL/SHM creation. |
| `cursor_pages_cover_archive_without_duplicates` | Exact 80 public IDs, descending order despite backfills, no duplicates, bounded pages, advancing cursors, exhaustion. |
| `adaptive_query_matches_reference_all_pages` | Complete traversal for 13 literal/short/Unicode/escaped queries, using direct SQLite LIKE plus independent fixture expectations; normal and forced scan modes, empty partial pages. |
| `filters_and_inclusive_dates` | Exact combined filters, inclusive day boundaries, historical name/timestamp, safe image URL filtering, validated avatars, conflicting scope. |
| `validation` | Original 400/422 cases; exact application error messages and framework query parameter locations. |
| `suggestions_scoping_and_minimum` | Exact author/channel results, short-query minimum, literal percent, guild scoping, invalid kinds, numeric lookup. |
| `emoji_channel_suggestions_keep_stable_ids` | Emoji/fullwidth label normalization, unchanged display labels and stable public channel IDs across suggestions/search. |
| `saved_profile_is_readable_without_collection_routes` | Exact saved fields/details/connections, message count/avatar, observed servers with public IDs, basic fallback, missing/invalid IDs, collection routes absent, shared profile assets. |
| `missing_old_and_wal_archives` | Missing files stay absent; old schema rejected; pending WAL refused in immutable mode; v9 reads without an upgrade or changed bytes. |

Coverage after 07: **30 of 36 original Python cases** have Rust-owned contracts,
plus five infrastructure tests. The other six original cases belong to 05.
Packed/export, remaining backend, browser and performance coverage in 08–10 is
still pending. All migrated scenarios currently target Python; selecting Rust
returns an explicit unimplemented error, never a skipped case.

## Durable scrape mapping (06)

Each `DurableScrapeTest.test_<name>` maps to the identically named Rust contract
(without `test_`) in `tests/durable_scrape.rs`:

| Name | Independently asserted behavior |
|---|---|
| `checkpoint_failure_rolls_back_page_cursor_flags_and_projection` | Uncommitted rows, cursor and projection invisible; injected failure retried without another fetch; exact committed cursor, count, rows and history flag. |
| `stop_joins_inflight_commit_before_archive_clear` | Stop remains pending behind commit barrier; archive/token clears conflict; accepted write finishes; subsequent clear removes messages and scrape checkpoints. Shared pacing records survive. |
| `shutdown_joins_commit_and_recovers_committed_rows_budget_and_queue` | Shutdown joins page commit; restart pauses; exact 100 committed rows, one-message remaining budget, cursor and next queued channel. |
| `restart_restores_pending_profiles_before_remaining_page_budget` | Exact pending profile IDs survive; reservations block clearing/restart; profile request precedes remaining page with exact limit/cursor. |
| `old_checkpoint_representation_requires_original_account_and_keeps_budget` | Rust supplies literal legacy checkpoint; wrong account cannot resume or fetch; original account consumes only remaining limit. |
| `account_race_between_resume_and_worker_remains_paused` | Account changes between resume and worker; no new request; persisted job remains running/paused with original account. |
| `simultaneous_start_reserves_channels_before_checkpoint_write` | Two simultaneous HTTP starts yield exactly 200/409 and one checkpoint. |
| `shutdown_before_worker_first_turn_keeps_recoverable_checkpoint` | Immediate shutdown recovers paused zero-progress job without fetching. |
| `resume_cannot_launch_worker_after_shutdown_barrier` | Credential barrier interleaves resume and shutdown; HTTP 409, paused job, no second request. |

## Pacing mapping (07)

Each `ScrapePacingTest.test_<name>` maps to the identically named Rust contract
(without `test_`) in `tests/scrape_pacing.rs`:

| Name | Independently asserted behavior |
|---|---|
| `failed_network_request_persists_the_last_attempt_deadline` | ReadTimeout propagates; last response completion attempt is persisted in wall time; success counter stays zero. |
| `random_intervals_retries_and_100_success_break_are_shared` | Exact 104 random draws, 103 request attempts, 429 retry, 503 exclusion, both interval bounds, 60-second break after 100 successes, subscriber break event, persisted counter one. |
| `actual_random_interval_bounds_and_slow_response_are_shared` | Four concurrent calls serialize; every 0.6-second response receives an additional 0.75-second interval; actual configured draw bounds are 0.5–1.5. |
| `counter_cooldowns_and_break_deadline_survive_restart_and_job_changes` | 99→100 resets counter and persists 90-second break plus 30-second rate-limit cooldown; restart and new job retain deadlines. |
| `nonzero_counter_survives_restart` | Success increments once; 503 does not; persisted counter restored across owner replacement. |

## Packed and exporter contracts (08)

| Contract | Independent expectations |
|---|---|
| `snapshot_blocks_hashes_reports_and_settings_exclusion` | 12 archive sizes from empty through 257 messages, 16-row block boundaries, Rust zlib decoding and canonical length-prefixed SHA-256, public IDs, historical names/timestamps, FTS/SQLite integrity, report shape, settings exclusion and unchanged source bytes/mtime. |
| `standard_and_packed_queries_filters_cursors_and_unicode_match` | Exhaustive cursor traversal and direct SQLite LIKE expectations for 14 queries, Unicode/NUL/wildcards, public-ID boundaries and missing/combined filters; forced eight-row scans match standard archives. |
| `failed_builds_fresh_destinations_and_publication_races_are_safe` | Existing destination/source/partial-build refusal, invalid block sizes/source versions, failure after first compressed block, temporary/journal cleanup, non-overwriting final publication against a racer, unchanged source. |
| `export_pins_live_wal_and_default_block_policy` | Uncheckpointed WAL update included in verified hash/results; default 128-row blocks and 129-message boundary. |
| `referenced_media_export_verifies_hashes_and_excludes_unreferenced_files` | Good/corrupt/missing/unreferenced files, exact copied bytes/report, destination refusal, attachment payload hydration, HTTP headers, digest-checked serving and corrupted-file rejection. |
| `profiles_observed_servers_and_unknown_saved_fields_survive_export` | Exact extended JSON and unknown fields, explicit presence, profile presentation, observed-server public IDs/order/counts, v9 optional profile-details table. Only profile age is excluded from equality. |
| `snapshot_cli_reports_are_exclusive_and_media_requires_fresh_directory` | Actual CLI parser, aggregate report file/output equality, media report, report/source/destination collisions, existing-report refusal and existing-media refusal. |
