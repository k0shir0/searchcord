# Searchcord Rust Migration Plan

**Suggested filename:** `SEARCHCORD_RUST_MIGRATION_PLAN.md`

## 1. Goal and reviewed baseline

Rewrite Searchcord’s backend and tools in Rust, beginning with its tests. Preserve existing functionality, archive compatibility, and browser behavior throughout the migration.

Decisions agreed for this plan:

- Keep the existing HTML, CSS, and JavaScript frontend.
- Migrate the entire backend, including collection, credentials, background jobs, exports, and packaging.
- Keep migration work off `main` until full functionality is preserved.
- Merge changes from `main` into the Rust integration branch; rebase individual task branches onto that integration branch.
- Require Windows, Linux, and macOS validation.
- Require at least **20% faster snapshot-export throughput or 20% lower packed-viewer peak memory**, alongside functional parity.

Reviewed repository: [k0shir0/searchcord](https://github.com/k0shir0/searchcord), `main` at [`481f62c6d1e111ca6f5771554a0e89ecd42b088e`](https://github.com/k0shir0/searchcord/commit/481f62c6d1e111ca6f5771554a0e89ecd42b088e).

The review covered all 17 production Python modules, maintained tests, first-party browser scripts, templates, styles, documentation, launchers, CI, and packaging. Logos and vendored Chart.js remain existing assets.

Current maintained coverage contains **36 Python tests across five files**, plus the Node browser queue suite. Snapshot export and several collector features need additional maintained coverage.

**Verification status:** source review completed; tests and benchmarks were not executed during this planning session. This document is supplied inline because Plan Mode prevents file creation. No branches or repository files have been changed.

## 2. Architecture and compatibility rules

### Rust structure

Use a Cargo workspace with five packages:

| Package | Responsibility |
|---|---|
| `searchcord-archive` | Standard and packed archive readers, packed codec, search, profile presentation, media validation |
| `searchcord-snapshot` | Snapshot generation, verification, reports, referenced-media export |
| `searchcord-viewer` | Read-only HTTP service, search assets, standalone packaging |
| `searchcord-collector` | Writable storage, migrations, credentials, Discord transport, jobs, full application HTTP service |
| `searchcord-parity` | Rust contract tests, fixtures, test drivers, browser checks, performance harness |

Use Tokio/Axum for asynchronous HTTP, `rusqlite` with bundled SQLite and verified FTS5 trigram support, Serde for JSON, and compatible zlib/SHA-256 implementations. Pin the exact Rust toolchain and dependency resolution when establishing the workspace.

Run SQLite work outside asynchronous executor threads. Give collector writes a bounded writer queue with explicit completion acknowledgements. Cancellation must join accepted writes before reporting completion or closing storage.

Provide these native executables:

- `searchcord`: collector application.
- `searchcord-search`: read-only viewer.
- `searchcord-snapshot`: existing snapshot CLI functionality.
- `searchcord-package-search`: standalone viewer packaging.

The viewer must build and run without the collector or credential dependencies.

### Interfaces that must remain compatible

- Preserve HTTP methods, routes, request parameters, defaults, response fields, status codes, redirects, and frontend-consumed error messages.
- Preserve SSE event names, payloads, initial snapshots, terminal events, reconnect behavior, and bounded buffering.
- Preserve collection schema **v10**, supported older migration inputs, standard read schemas **v9/v10**, and packed schema **v101**.
- Preserve existing job checkpoint JSON and media manifest formats.
- Preserve public Discord IDs as strings in responses. Internal row numbers and packed references must remain internal.
- Preserve stored timestamps, historical names, unknown message/profile fields, revisions, and deletion tombstones.
- Preserve environment variables, viewer flags, snapshot flags, default ports, loopback binding, and application-relative path resolution.
- Keep the existing frontend and its assets unchanged during backend migration.

New Rust library interfaces should expose typed archive operations—search, profiles, observed servers, summaries, and suggestions—without exposing web-framework types.

Compatibility means equivalent behavior and data. SQLite file bytes, compression output bytes, generated UUID values, and measured durations need not be identical. Their relationships and meaning must still be verified.

### Branch workflow

1. Create `rewrite/rust` from the latest `origin/main`.
2. Create each task branch from that integration branch, using names such as `rewrite/rust-01-test-harness`.
3. Complete and validate one numbered chunk before integrating it into `rewrite/rust`.
4. Between chunks, merge the latest `origin/main` into `rewrite/rust`.
5. Rebase unfinished task branches onto the updated integration branch.
6. Resolve upstream behavior changes by updating the parity ledger and reference tests before continuing.
7. Target task PRs at `rewrite/rust`. Open the final PR against `main` only after every acceptance gate passes.

Do not rewrite `main` or force-push the integration branch.

## 3. Ordered implementation chunks

Each numbered row is a separate task branch and reviewable work unit. Follow the order below. Production Rust migration begins only after the test-first milestone passes.

### A. Establish Rust tests first

The existing Python application remains the reference implementation.

Rust must own test scenarios and assertions. A temporary Python test adapter may invoke existing functions and install test doubles; it must not contain duplicate business logic or hide assertions inside calls to the old test suite.

| ID | Work | Definition of done |
|---|---|---|
| 01 | Establish branch, handoff document, baseline, and CI | Integration branch exists; document is committed there; current Python/Node checks run successfully; dependency versions and baseline SHA are recorded. Update the restrictive `.gitignore` allowlist for Rust source, fixtures, workflows, and migration documents while keeping archives, credentials, builds, and scratch output ignored. |
| 02 | Build the Rust parity harness | Rust can create disposable archives, control a reference Python process, send HTTP requests, inspect SQLite, and collect request/event traces. Every test uses isolated paths. Test drivers cannot contact live Discord. |
| 03 | Port archive-storage regressions | All seven storage tests have corresponding Rust-owned assertions covering credentials, legacy attachments, exact payload observations, edits, tombstones, FTS/count consistency, and rollback. Original tests still pass. |
| 04 | Port reader and display regressions | All nine display tests run from Rust against Python, including full cursor traversal, literal text matching, date boundaries, validation, profiles, suggestions, unavailable archives, and read-only behavior. |
| 05 | Port permissions and discovery regressions | All six access tests have Rust equivalents covering overwrite precedence, owner/admin access, parent permissions, readable empty channels, thread pagination, and transient probe failures. |
| 06 | Port durable scrape regressions | All nine durable tests have Rust equivalents with controlled commit barriers. Assert rollback, stop/shutdown joining writes, paused recovery, remaining limits, pending profiles, original-account requirements, and concurrent starts. |
| 07 | Port pacing regressions | All five pacing tests have Rust equivalents using controlled clocks and random draws. Verify intervals after responses, retries, the 100-success break, shared counters, cooldowns, and restart recovery. |
| 08 | Add packed reader and exporter contracts | Cover standard/packed result equivalence, block boundaries, public-ID translation, short queries, Unicode, NUL, literal wildcard characters, historical fields, snapshot hashes, existing-destination refusal, failed builds, WAL reads, reports, and media export. |
| 09 | Cover remaining backend behavior | Add Rust contracts for settings, both vault formats, invites, profile collection/backfill, live monitoring, DM cleanup, ChatML, statistics, message refresh, clear operations, HTTP validation, and startup/shutdown. Every route and persisted job kind has a ledger entry and test ownership. |
| 10 | Establish browser and performance baselines | Keep the Node queue suite. Add Rust-driven WebDriver smoke tests for both existing frontends against mocked Python services. Create reproducible performance fixtures and record Python measurements. All 36 original Python cases are mapped to Rust cases. |

**Test-first milestone:** Rust tests exercise the existing application successfully, missing feature coverage has been added, and the baseline benchmark procedure is reproducible. No production Rust replacement is required to reach this milestone.

### B. Packed reader, exporter, and read-only backend

Primary source boundaries: `archive_reader.py`, `snapshot_codec.py`, `search_snapshot.py`, and `search_app.py`.

| ID | Work | Definition of done |
|---|---|---|
| 11 | Implement packed decoding | Rust reads existing v101 metadata and compressed blocks, reconstructs exact fields, and translates public IDs across block boundaries. Cache sizes are bounded; malformed data returns controlled errors. |
| 12 | Implement packed search | Match existing short-text/trigram candidate selection, filters, ordering, cursor boundaries, and bounded scan continuation. SQLite remains the authority for LIKE semantics. Exhaustive fixture traversal matches the reference without duplicates or lost results. |
| 13 | Implement standard archive reads | Read v9/v10 without creating, upgrading, or indexing databases. Preserve transactions, live-WAL visibility, four-second SQL deadlines, and immutable-mode WAL refusal. Standard and packed reader suites pass against Rust. |
| 14 | Implement shared profile and media reads | Match saved profile presentation, Unicode label normalization, observed-server ordering, attachment hydration, local-media URLs, digest checks, and media response behavior. No credential access or upstream requests occur. |
| 15 | Implement snapshot database export | Produce Python-readable v101 snapshots with the existing default block size and compression policy. Verify canonical field hashes, SQLite/FTS integrity, metadata, attachment records, and exclusion of settings and credentials. Test both Python-to-Rust and Rust-to-Python reading. |
| 16 | Implement export publication, media, and reports | Preserve fresh-destination requirements, exclusive temporary builds, failure cleanup, verified non-overwriting publication, referenced-media copying, and report fields. Source archives remain unchanged. |
| 17 | Implement read-only HTTP service | Existing frontend runs against Rust with matching APIs, validation, assets, privacy page, compression, headers, and media responses. Collector/settings/deletion routes remain unavailable. Reader/display contracts pass over real HTTP. |
| 18 | Package and benchmark the native viewer | Standalone folders contain the viewer, approved assets, licenses, and deployment instructions. Build a non-root Linux image from a prebuilt static viewer and approved assets, excluding collector code, Python, credentials, and archives. Record initial Rust performance results. |

**Read-only milestone:** the Rust reader, exporter, and viewer are usable independently. The Python collector remains available on the integration branch.

### C. Writable storage and collector services

Primary boundaries include `storage.py`, `credential_store.py`, `durable_scrape.py`, the profile/invite modules, and the collector behavior currently assembled in `app.py`.

| ID | Work | Definition of done |
|---|---|---|
| 19 | Implement writable message storage | Preserve current payloads, distinct observed revisions, restoration after deletion, immutable identifiers, metadata, FTS triggers, counts, and transaction rollback. Rust-written v10 archives remain readable by Python. |
| 20 | Implement portable credentials | Read and write the existing Fernet vault format, external path derivation, permissions, validation, verified replacement, and missing-key failure behavior. Python and Rust can read each other’s synthetic vaults. |
| 21 | Implement Windows credentials | Preserve existing DPAPI headers, current-user protection, vault path derivation, and portable-vault reading. Bidirectional Python/Rust tests pass on Windows without interactive prompts or plaintext tokens in SQLite. |
| 22 | Implement startup migrations and recovery backups | Match recognized legacy schemas, timestamp/name reconstruction, attachment preservation, sanitized recovery copies, compression verification, integrity checks, idempotent startup, and rejection of unsupported formats. Test entirely on disposable copies. |
| 23 | Implement Discord transport and shared pacing | Match request headers, shared request serialization, cooldowns, rate-limit retries, scrape intervals, mandatory breaks, and persisted timing recovery. Paused jobs must not hold the request gate. All pacing contracts pass. |
| 24 | Implement source discovery | Match account validation, guilds, DMs/group DMs, permissions, probes, active/archived/private thread discovery, pagination, and error propagation. Existing JavaScript retains automatic bot-name filtering and queue ownership. |
| 25 | Implement profile collection | Match manual fetch/refresh, complete saved JSON, stale/missing backfill, retry classification, shared profile exclusion, cancellation, and commit-before-next-request behavior. Failed requests preserve saved profiles. |
| 26 | Implement durable job ownership | Match existing checkpoint JSON, channel reservations, copied snapshots, bounded subscriptions, event projection, retention, account binding, and paused restart recovery. Control and checkpoint tests pass independently of page fetching. |
| 27 | Implement scrape pagination and page commits | Preserve history/newer/gap stages, per-channel limits, duplicate handling, pending profiles, and history-complete flags. Message page, cursor changes, and job projection commit together; failures change none of them. |
| 28 | Complete scrape lifecycle | Wire pause/resume/stop, shutdown, retries, media collection, progress SSE, and recovery. A fetched page finishes its save before pause; stop/shutdown join accepted writes. The complete durable suite passes against Rust. |
| 29 | Implement invite queues | Preserve parsing/deduplication, queue limits, original-account binding, 30-second spacing, cooldowns, blocked-account states, stoppability, and paused recovery. Interrupted joins remain uncertain and are not automatically replayed. |
| 30 | Implement live monitoring | Preserve three-second polling, newest-page observations, gap draining, atomic monitor checkpoints, edit/deletion confirmation, initial SSE state, stop behavior, and paused recovery. Generic 404 responses cannot create tombstones. |
| 31 | Implement DM cleanup | Delete only the selected account’s messages. Count 204 as deleted and Discord Unknown Message `10008` separately as already missing. Preserve retries, cancellation, progress, tombstones, and non-restarting shutdown behavior. |
| 32 | Implement ChatML and statistics | Preserve chronological turn grouping, blank-message handling, participant defaults, prompt substitution, streaming downloads, cached totals, rankings, tie ordering, date ranges, and drilldown response shapes. |
| 33 | Complete collector HTTP and administration | Implement every remaining route, including counted-search compatibility, image refresh, message archive/refresh, settings, token removal, and archive clearing. Preserve confirmation flags, writer barriers, conflicts, and cleanup failure reporting. |
| 34 | Complete native startup and launchers | Preserve ports, environment configuration, path behavior, terminal banner/fallbacks, browser opening after matching-instance readiness, and terminal restoration. Existing Windows launcher names invoke native binaries and support paths with spaces. |

**Collector milestone:** all backend functionality runs in Rust, the existing frontend works unchanged, and no Python production process is required.

### D. Qualify and prepare the final change to main

| ID | Work | Definition of done |
|---|---|---|
| 35 | Run complete cross-language qualification | Every backend contract passes against Rust and the pinned Python reference. Test database interchange, snapshots in both directions, vaults on their supported platforms, persisted job recovery, actual HTTP/SSE, and unchanged browser flows. No unimplemented capability is skipped. |
| 36 | Meet the performance gate | Run the fixed paired benchmark procedure. Achieve the agreed 20% improvement and remain within the regression limits below. If unsuccessful, optimize the measured bottleneck in separate branches, then rerun qualification. |
| 37 | Retire Python production dependencies | Remove superseded Python runtime/tooling from shipped artifacts. Retain access to the pinned Python reference for development parity tests. Update installation, deployment, recovery, launcher, and rollback documentation; verify native artifacts on all required platforms. |
| 38 | Synchronize and prepare the final main PR | Merge current `main` into the integration branch, incorporate any new behavior, and repeat all final gates. Prepare a PR containing the completed parity ledger, test evidence, benchmarks, migration instructions, and rollback procedure. Individual chunks never merge directly into `main`. |

## 4. Verification and acceptance gates

### Required test design

Use two backend selections in the Rust harness: `python` and `rust`. Each scenario must contain independent expected assertions; comparison alone is insufficient.

During migration, run the complete Python-reference suite and every completed Rust suite. Record unfinished Rust capabilities explicitly. Selecting an unfinished suite against Rust must fail clearly rather than silently skip it.

Use controlled clocks, random sources, transport responses, and commit barriers for job tests. Normalize only unavoidable nondeterminism, such as elapsed measurements and generated-ID mappings. Never normalize away message IDs, stored timestamps, missing fields, ordering, or lost events.

Required scenario groups:

- **Search:** empty/common/rare queries; one/two-character queries; Unicode; NUL; `%`, `_`, and escape characters; combined filters; inclusive UTC dates; out-of-order backfills; empty partial pages; cursor exhaustion.
- **Storage:** old-format upgrades; exact historical values; repeated observations; changed payloads; tombstones/restoration; FTS integrity; transaction failures; Python/Rust interchange.
- **Snapshots:** boundary blocks; empty archives; canonical hashes; source WAL visibility; interruption; destination races; media corruption; settings exclusion; reports.
- **Jobs:** concurrent starts; account changes; pause during fetch/save/profile work; stop during retry/break; shutdown before worker startup; restart with pending work; stalled subscribers.
- **Administration:** clear/removal confirmations and conflicts; joined writers; preserved credentials or messages as appropriate; partial media-cleanup failures.
- **Browser:** search and shared links; profiles; suggestions; queue recovery; account switching; invites; monitoring; cleanup; exports; statistics.

Run Rust formatting, Clippy, Rust tests, existing Node queue tests, and JavaScript syntax checks in CI. Keep the original Python regressions until their replacements and final qualification pass.

### Performance procedure

Use deterministic synthetic archives: **100,000 messages for routine checks** and **1,000,000 messages for qualification**. Include realistic varied text, short queries, sparse filters, profiles, and attachments.

Measure on one recorded Linux x86-64 reference machine:

- Snapshot throughput, peak process memory, and output size.
- Standard/packed search latency and viewer memory.
- Fresh-process and warmed operation.
- One and sixteen concurrent viewer requests.
- Local message-save throughput, excluding intentional Discord pacing.

Use release Rust builds, the pinned Python reference, identical fixtures/options, and alternating paired runs. Record at least five pairs in each of two qualification sessions.

Acceptance requires:

- Snapshot messages/second **≥1.20× Python**, **or** packed-viewer peak memory **≤0.80× Python** under the fixed sixteen-request workload.
- No greater than **10% regression** in other measured latency, throughput, peak-memory, or snapshot-size metrics.
- The improvement repeats in both sessions.
- Full correctness, verification, pacing, compression settings, and returned fields remain enabled.

Performance results cannot substitute for feature parity. Cross-platform functional and packaging gates remain mandatory even though the quantitative benchmark uses the reference Linux machine.

### Definition of done for every chunk

A chunk is complete only when:

1. Its stated behavior and tests pass.
2. Existing required checks remain green.
3. Its completed Rust contracts pass against the reference expectations.
4. The parity ledger records source coverage, test names, evidence, and remaining dependencies.
5. The handoff records task/integration commits, commands run, results, and next chunk.
6. The change is integrated into `rewrite/rust`, with `main` untouched.

## 5. Handoff and final completion

Maintain a parity ledger beside this document. Each feature entry records its Python source boundary, Rust owner, contract tests, implementation status, and evidence. Include framework-provided HTTP behavior such as validation, method rejection, trailing-slash redirects, and collector documentation routes when inventorying the public surface.

The rewrite is complete when:

- Every backend feature and tool has a Rust implementation.
- Existing supported data remains usable, including vaults and paused checkpoints.
- The unchanged frontends pass their workflows against Rust.
- Native collector, viewer, exporter, and packaging artifacts work without Python.
- Windows, Linux, and macOS checks pass.
- The performance gate passes.
- The final branch includes current `main` behavior and has a documented rollback path.

Keep the Python baseline available for comparison and rollback. Do not introduce a new archive format merely to simplify the rewrite. Launch-command changes from Python scripts to native executables are intentional; document their direct equivalents.

**Starting instruction for another chat:**

> Read this migration plan and inspect the repository’s current branch, guidance, and parity ledger. Complete the next unfinished numbered chunk on a task branch from `rewrite/rust`. Preserve the existing application and all compatibility gates. Record validation and commit references, integrate only into the Rust branch when the chunk’s definition of done passes, and leave `main` unchanged.