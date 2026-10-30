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

Next: implement 02, then 03 and 04 on separate task branches. Commit references
and validation evidence will be recorded as each chunk is integrated locally.
