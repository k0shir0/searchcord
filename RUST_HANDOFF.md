# Rust migration handoff

Integration branch: `rewrite/rust`. Production reference: `origin/main` at
`481f62c6d1e111ca6f5771554a0e89ecd42b088e`. Keep `main` unchanged.
See [the migration plan](SEARCHCORD_RUST_MIGRATION_PLAN.md) and
[the parity ledger](PARITY_LEDGER.md).

## Baseline prerequisite (01)

The supplied workspace had no checkout and the remote had no Rust branches or
handoff. Cloned the repository and established the local integration branch at
the reviewed baseline before starting requested chunks 02–04.

Validation on Linux x86-64, Python 3.12.14, Node 24.19.0:

- `python -m unittest discover -s tests -t . -v`: all 36 pass.
- `node --test tests/test_collection_queue.mjs`: all 25 pass.
- `node --check` on every `static/**/*.js`: pass.

Python parity dependency resolution is pinned in `requirements-parity.txt`,
including platform markers for Windows. Production requirements are unchanged.
Rust is pinned to 1.90.0; Cargo.lock will pin the harness dependency graph in 02.
CI checks Windows, Linux and macOS; remote runs remain pending until pushed.

Local setup: create a Python venv, install `requirements-parity.txt`, put its
Python on PATH (or set `SEARCHCORD_PARITY_PYTHON` to its absolute executable).
Tests use disposable data and credentials outside their archive directory.

## Chunk 02 — parity harness

Prerequisite commit: `809bc39`. Task branch: `rewrite/rust-02-test-harness`.
The Cargo workspace currently contains only the test package; production packages
will be introduced by their implementation chunks. Rust 1.90.0 and Cargo.lock
pin the toolchain and dependency graph. Bundled SQLite FTS5 trigram works.

The Python adapter only calls existing functions, wraps transaction boundaries,
and hosts the actual viewer with Uvicorn. Rust creates fixtures, sends real HTTP
requests and owns all assertions. The audit hook blocks external connects, DNS
and subprocess spawning; numeric loopback is available for Windows asyncio.
The Rust HTTP client only constructs numeric loopback URLs and never follows
redirects. Every test owns a new directory/process and joins the process on drop.
Traces include commands, replies, HTTP requests/responses and parsed SSE frames.

Validation: `cargo test --workspace --locked` (5 passed),
`cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets --locked -- -D warnings`.
All original baseline checks remain required in CI. Windows/macOS CI is configured
but has not run here. No production Rust capability or performance claim is made.

Next: 03 archive-storage assertions, then 04 reader/display assertions.
