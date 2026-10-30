# Reproducible Rust migration baselines

The performance runner owns deterministic inputs and output assertions in Rust.
The Python workers call the production exporter/writer and forbid networking.
Viewer measurements use the actual read-only Uvicorn HTTP service over numeric
loopback. No Discord requests, account credentials or private archives are used.

## Setup and functional checks

Use Rust 1.90.0 and the locked Python dependencies:

```sh
python -m venv .venv
.venv/bin/python -m pip install -r requirements-parity.txt
python crates/searchcord-parity/driver/install_webdriver.py
```

The installer downloads Chrome for Testing and its matching ChromeDriver,
both version **154.0.8037.57**. It prints `SEARCHCORD_WEBDRIVER` and
`SEARCHCORD_CHROME_BINARY`; export those paths in your shell. In GitHub Actions
it writes them to `GITHUB_ENV`. Chrome and its driver remain outside shipped
artifacts and the repository. Windows/macOS use the same browser version.

```sh
SEARCHCORD_PARITY_PYTHON=/absolute/path/to/python cargo test --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Browser gates fail explicitly if WebDriver or Chrome is missing. They never skip.
They drive both unchanged frontends through W3C WebDriver; Chrome disables
background traffic and resolves only loopback. The Python service independently
rejects upstream connections. Synthetic HTML-looking message content is checked
for literal display. Browser processes/sessions and all test archives are joined
and removed after each test.

## Python baseline

On Linux x86-64, run:

```sh
SEARCHCORD_PARITY_PYTHON=/absolute/path/to/python cargo run --release --locked \
  -p searchcord-parity --bin parity-benchmark -- 100000 /new/path/python-baseline.json
```

The report path must be new. Every database and save input is disposable; the
report contains aggregate numbers and a fixture digest only. The source recipe
is `v1-public-id-backfill`: four servers, sixteen channels, sixty-four authors,
one-second public-ID spacing, even rows followed by odd backfills, deterministic
Unicode/text/wildcard content and one rare message per 997 rows. A separate
reproducibility contract checks its row counts, order and canonical digest.

The baseline runs three fresh export processes, standard and packed viewers with
one and sixteen concurrent HTTP workers, and three fresh local-save processes
with 10,000 complete synthetic payloads. Each export uses the existing 128-row
block and zlib level 9 settings, full canonical hash verification and SQLite/FTS
integrity checks. Rust checks the expected row digest, exact result IDs and field
counts; no correctness step is disabled. Save measurements use the production
page transaction including metadata/cursor/count/FTS updates and exclude Discord
pacing. The HTTP workload requests forty complete `common` search results,
checks their public-ID order, records a fresh request and eight warm requests per
concurrent worker, and samples process peak RSS after all requests.

Measurements include wall time, messages/second, snapshot size, process peak RSS,
fresh HTTP latency and warm p50/p95 latency. Linux `ru_maxrss` is converted from
KiB to bytes. Viewer RSS comes from a fresh Python process for each format and
concurrency group, so an earlier exporter cannot inflate its peak. HTTP latency
includes serialization and transport. The reference source is the unchanged
Python production tree from `481f62c6d1e111ca6f5771554a0e89ecd42b088e`.

The recorded [Linux baseline](benchmarks/python-linux-v1.json) uses Python
3.12.14, an AMD EPYC 9V74 host and a two-CPU allocation. The 100,000-message source
digest is `567e0d5a4443edd5f48615695d2034cee49f2923b633e18f8b5b25b50d1e6c58`.
It is a development baseline, not performance qualification. Shared-host load
can vary; the raw measurements are retained instead of selecting a best run.

## Final paired qualification (36)

Use this same fixed recipe, options and Linux reference machine for both
backends. Alternate Python/Rust release runs for at least five pairs, repeat in
two sessions, and preserve every raw report with machine/toolchain/reference
metadata. Rust backend selection must fail until its measured capability is
implemented; do not substitute Python or silently skip a workload.

Require snapshot throughput at least 1.20 times Python or packed-viewer peak RSS
at most 0.80 times Python under the sixteen-worker workload. Other latency,
throughput, memory and snapshot-size metrics may regress by at most ten percent.
The improvement must repeat in both sessions. Functional/interchange/browser
qualification and Windows/macOS packaging remain separate required gates.

## Initial native measurements (18)

Build the release viewer/exporter and development runner, then select the measured
backend explicitly. Python only generates disposable fixtures and collects the
native export child's resource usage; it is not part of the native application.

```sh
cargo build --release --locked -p searchcord-viewer -p searchcord-snapshot -p searchcord-parity
SEARCHCORD_PARITY_PYTHON=/absolute/python target/release/parity-benchmark 100000 /new/rust.json rust
SEARCHCORD_PARITY_PYTHON=/absolute/python target/release/parity-benchmark 100000 /new/python.json python
```

The [initial native report](benchmarks/rust-linux-initial-v1.json) and
[fresh Python comparison](benchmarks/python-linux-initial-v2.json) use the same
100,000-message digest, fields and verification. Median export throughput is
14,466 versus 16,229 messages/second; output sizes are 9,457,664 versus 9,007,104
bytes. Packed-viewer peak RSS with sixteen HTTP workers is 20,549,632 versus
59,289,600 bytes; warm p95 is 48.0 versus 100.0 ms. These are initial development
measurements, not a passed performance gate: export throughput needs attention,
the runs were sequential rather than five alternating pairs in two sessions,
and local save performance remains explicitly unimplemented in the native report.
Native export timing includes process startup; the Python function timing starts
after imports. Python viewer RSS includes its reference adapter. Qualification
must resolve these boundaries and repeat the full agreed procedure in chunk 36.

Native packaging has a Rust smoke test that launches the approved standalone
folder with spaces in its name and Python absent from PATH. Linux validation
compiled a static PIE (no ELF interpreter), built `Dockerfile.native` from that
folder, and ran the scratch image as 65534:65534. Actual HTTP verified summary,
full search fields, approved assets, verified media and absent collector routes.
Exporting the clean image confirmed its 17 files include no Python, collector,
archives or vaults. A temporary named volume held the synthetic archive; it was
removed after the test. CI repeats standalone tests on all platforms and the
static-image build/run on Linux.
