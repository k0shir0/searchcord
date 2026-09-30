# Search-only storage and latency

The search server now accepts a verified, compressed schema v101 snapshot in
addition to a schema v9 collector archive. On the measured **5,574,147-message**
archive, the deployment file shrank **54.5%**, from **260.03 to 118.31 bytes per
message**. Literal substring behavior and all ten displayed message fields are
preserved. The collector archive is unchanged.

The supplied `searchcord-search-only-scale-report.md` describes the unified
collector at `79f56b2`. The existing separate frontend already had bounded
suggestions, independent counters, cursor pagination, full-text rendering and a
small search entry. This change builds on that frontend. The user's retention
requirement determines the implementation: full message text remains available,
and search continues to use SQLite's literal substring rules.

## Storage

| Measurement | Collector | Search snapshot |
| --- | ---: | ---: |
| Messages | 5,574,147 | 5,574,147 |
| Database bytes | 1,449,422,848 | 659,505,152 |
| Bytes per message | 260.03 | 118.31 |

The source measurement is the logical database page count in a pinned read
transaction, including pending WAL pages. It excludes backup files and WAL
container overhead. Snapshot size is the actual closed deployment file. Keeping
both files locally consumes their combined storage; the reduction applies to the
search server's archive.

| Snapshot component | Bytes |
| --- | ---: |
| Compressed record blocks | 225,476,608 |
| Dense message references | 76,378,112 |
| Author and source indexes | 117,506,048 |
| Contentless trigram index | 189,136,896 |
| Short-text block index | 28,753,920 |
| Dictionaries, profiles, counters and other metadata | 22,253,568 |
| **Total** | **659,505,152** |

The collector's channel, guild, author and public-ID indexes occupied
708,820,992 bytes. The snapshot shares channel/guild pairs, replaces repeated
snowflakes with dense references, and uses chronological storage rowids. A
sparse index stores one public message ID per compressed block; binary search
within that block translates dates and public cursors without an additional
per-message public-ID index.

Blocks contain 128 records, compressed with the Python standard library's zlib
at level 9. Shared name dictionaries retain historical values exactly. Trigram
postings remain separate from the compressed text. Short-text postings identify
blocks containing a character or character pair, adding 28.75 MB while avoiding
expensive archive-wide decompression for sparse short queries.

The remaining trigram postings account for about 29% of the file. Smaller storage
that drops substring indexing would require more content scanning. This format
keeps both the retained data and interactive search available.

## Correctness and publication

The exporter streams message rows in public-ID order, with bounded batches and a
consistent read-only source transaction. It excludes settings, tokens, collection
cursors and administration tables. It copies guild/channel/author metadata,
saved profiles and counters. Image bytes remain external; image URL values are
preserved exactly.

Before publishing, it runs SQLite and both FTS integrity checks and compares a
length-delimited SHA-256 digest across **every decoded message field**:
`id`, `channel_id`, `guild_id`, `author_id`, `content`, `timestamp`, `image_urls`,
`channel_name`, `guild_name`, and `author_name`. Source and restored digests matched
for all 5,574,147 records. The original database's size and modification time
were unchanged. Synthetic checks also verify pending WAL rows and unchanged
source/WAL file stamps.

Only a verified file is published. Existing destination files and reports are
refused, and a report cannot replace either archive. Interrupted or failed builds
remove their temporary file. A live source may continue collecting after the
pinned snapshot begins; those later rows require a new export.

The measured build, including verification, took **399.67 seconds**. Source,
temporary output and SQLite's vacuum work file coexist during construction;
provide space for the source plus roughly two output files. The completed file
is separate from the collector and must be served unchanged.

## Search operation

The API still returns public message IDs as cursors, newest first, with one
extra candidate establishing `has_more`. It does not compute an exact full match
count or use an increasing message offset. Only the selected page is hydrated.

Queries of at least three characters select candidates using quoted trigram
tokens, then apply the original escaped SQLite LIKE predicate to decoded text.
One- and two-character queries use the short-text block index. Small filtered
sets can use a direct scan. ASCII case folding, literal `%`/`_`/`!`, Unicode,
inclusive dates and SQLite's NUL behavior remain consistent with the source.

This follows SQLite's documented limits: contentless FTS does not supply original
text; `detail=none` limits trigram MATCH tokens to three characters, and an ESCAPE
clause prevents the trigram LIKE optimization. Candidate lookup and exact
verification therefore use separate predicates. [SQLite FTS5 documentation](https://www.sqlite.org/fts5.html#the_trigram_tokenizer).

The service caches at most 64 decoded blocks across requests, keyed by compressed
bytes so reuse remains correct across files and connections. Each connection
keeps at most 16 block references and 256 decoded names. It never loads the
whole archive into memory. The four-second SQL deadline remains.

## Measured latency

These are local measurements on the actual archive, with **five sequential
rounds**, fresh SQLite connections, a bounded decode cache and OS disk cache
not flushed. The table measures the full `Archive.search` operation, including
message hydration, for **40 messages per page**. It is not an HTTP or browser
measurement. Complete displayed fields, order, `has_more` and cursor matched;
no requests timed out.

| Query class | Collector median | Snapshot median |
| --- | ---: | ---: |
| Browse newest | 5.46 ms | 1.86 ms |
| `hello` | 11.71 ms | 3.96 ms |
| `the` | 6.94 ms | 2.18 ms |
| `release notes` | 28.30 ms | 16.49 ms |
| No match | 15.11 ms | 5.69 ms |
| Literal `%_` | 1,989.94 ms | 1.69 ms |
| Short `he` | 5.88 ms | 2.26 ms |
| Server plus `hello` | 7.32 ms | 5.03 ms |
| Channel plus `hello` | 11.16 ms | 9.90 ms |
| Author browse | 4.95 ms | 1.72 ms |
| Recent date range plus `hello` | 11.06 ms | 3.69 ms |

Compression still costs work on uncached blocks. The first server/channel text
samples took 13.40/14.78 ms versus 7.32/11.16 ms for the source. After traversing
100 cursor pages, a further snapshot page took **17.7 ms versus 10.3 ms** for the
source, with identical fields. The 100-page traversal took 1.61 seconds including
the final comparisons. Warm first-page improvements do not imply that every
new page or arbitrary query is faster.

Actual local HTTP probes against the packed file requested **50 messages**:

| Query class | HTTP median | First HTTP request |
| --- | ---: | ---: |
| Browse | 7.07 ms | 63.78 ms |
| `hello` | 11.11 ms | 20.73 ms |
| `the` | 7.99 ms | 10.66 ms |
| Phrase | 25.35 ms | 42.85 ms |
| No match | 11.50 ms | 11.50 ms |
| Literal `%_` | 4.40 ms | 5.48 ms |
| Short `he` | 7.06 ms | 8.68 ms |

The 50-message `hello` response transferred 3,407 gzip bytes. A ten-request
simultaneous `hello` burst completed without errors in 150.23 ms, with a
134.06 ms request median and 146.45 ms maximum. This is a small local concurrency
check, not a sustained-load or p95/p99 capacity claim. No 100M/1B archive,
cold-disk test, remote host or production deployment was tested.

## Collection profiles

With **fetch expanded profiles** checked, each committed Discord message page
immediately collects its missing real-user profiles before the next message
page is requested. Saved extended profiles are reused, and webhooks are skipped.
This happens during history and incremental collection.

The shared Discord gate returns successful responses immediately so they can be
saved; cooldowns apply before the next request. Profile backfill retries the
same user after rate limits, transient HTTP/network failures, invalid replies or
SQLite busy errors. Progress exposes the user, retry and wait. Stop cancels active
requests and waits, saved profiles survive, and restart skips completed users.
Forbidden/missing users are skipped; rejected credentials end the run visibly.

These paths were verified with mocked Discord responses and real synthetic
SQLite persistence. No live Discord collection or account request was made.

## Reproduce

From this checkout, using Python with the repository requirements:

```bash
python search_snapshot.py /path/to/collector/searchcord.db packed/searchcord.db --report packed/build-report.json
python search_app.py --data-dir packed --snapshot
python checks/compare_search.py /path/to/collector/searchcord.db packed/searchcord.db --rounds 5 --report search-report.json
python checks/http_search_probe.py http://127.0.0.1:8001 --report http-report.json
python -m unittest discover -s checks -v
```

For Chrome checks, build a fresh **synthetic** archive so private messages never
appear in screenshots:

```bash
python checks/snapshot_fixture.py agents/browser-fixture
python search_app.py --data-dir agents/browser-fixture/packed --snapshot --port 8017
node checks/browser_search.mjs http://127.0.0.1:8017 agents/browser-search
node checks/browser_profile_search.mjs http://127.0.0.1:8017 agents/browser-profiles
```

Validation includes **39 integrated backend tests**, **42 search browser checks**,
**27 profile browser checks**, and the collector's **17 backfill browser checks**.
Browser checks cover real synthetic API results, literal highlighting, saved
profiles, cursors, reload/back navigation, stale responses, inert archived HTML,
full long text, keyboard controls, reduced motion and 390/320px layouts. Browser
interaction timing includes driver polling and synthetic data, so it is not a
large-archive or Core Web Vitals measurement. Python/JavaScript syntax and Git
whitespace/conflict checks also pass.

Rebuild into a new directory after collecting messages or profiles, stop the
display service, then restart against that new directory. Normal read-only
display mode remains available for a live schema v9 archive. The schema v101
file belongs only to the search service.
