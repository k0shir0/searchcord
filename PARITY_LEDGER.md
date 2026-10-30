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
| 05–10 | Permissions, durable jobs, pacing, packed/export, remaining routes, browser/performance | Future test-first chunks | Pending; milestone not yet reached |
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

Coverage after 04: **16 of 36 original Python cases** have Rust-owned contracts,
plus five infrastructure tests. The other 20 original cases belong to 05–07.
Packed/export, remaining backend, browser and performance coverage in 08–10 is
still pending. All migrated scenarios currently target Python; selecting Rust
returns an explicit unimplemented error, never a skipped case.
