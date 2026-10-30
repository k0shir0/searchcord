# Rust parity ledger

Python reference: `481f62c6d1e111ca6f5771554a0e89ecd42b088e`.
Rust owns scenarios, fixtures and assertions; Python is a temporary call adapter.
No production Rust backend exists yet. Selecting it must fail explicitly.

| Chunk | Source coverage | Rust owner/contracts | Status/evidence |
|---|---|---|---|
| 01 | Original 36 Python cases, browser queue, JS syntax | Baseline / cross-platform CI | Linux: 36 Python + 25 Node pass; JS syntax pass. |
| 02 | Disposable archives, process control, HTTP, SQLite, traces, network isolation | `searchcord-parity/tests/harness.rs`: `disposable_archives_control_reference_and_verify_fts5_trigram`, `real_http_event_and_command_traces_are_collected`, `driver_blocks_live_network_and_path_escape`, `backend_selection_rejects_unfinished_rust_instead_of_skipping`, `dropping_harness_joins_reference_and_removes_temporary_archives` | Linux: all five pass; formatting and Clippy pass. SSE fixture verifies transport capture only; production job event contracts remain in 06–09. |
| 03 | Seven `tests/test_archive_storage.py` cases | `searchcord-parity/tests/archive_storage.rs` | Pending |
| 04 | Nine `tests/test_search_display.py` cases | `searchcord-parity/tests/search_display.rs` | Pending |
| 05–10 | Permissions, durable jobs, pacing, packed/export, remaining routes, browser/performance | Future test-first chunks | Pending; milestone not yet reached |
| 11–38 | Production Rust implementations and qualification | Future packages | Unimplemented |
