# SQLite storage and throughput audit

Measured on 2026-09-25 against an archive of **3,690,778 messages**. The original
personal database was opened read-only. All migrations, browser checks, and
experiments used separate ignored copies. No private message, token, database,
or browser capture is part of this repository.

## Size timeline

| Date and measured state | Active SQLite file | Change from the original | What it held |
| --- | ---: | ---: | --- |
| 2026-09-23, original legacy archive | 1,664,700,416 B | Baseline | Message text, IDs, per-message names, original timestamps, raw attachment metadata. |
| 2026-09-23, earlier v7 compact archive | 1,065,906,176 B | −598,794,240 B (35.97%) | Earlier release removed historical names/timestamp strings and retained image links. This is a separate, historically lossy path. |
| 2026-09-24, v8 audit copy from the original | 1,008,660,480 B | −656,039,936 B (39.41%) | Dense search row IDs, all original historical fields. This was an audit intermediate, not a release. |
| 2026-09-25, v9 from the original | **843,780,096 B** | **−820,920,320 B (49.31%)** | Exact text, IDs, per-message names and timestamps, and image links. Raw attachment metadata was removed. |

The final file averages 228.6 bytes per message, including all indexes and
metadata. The intermediate v8 file makes the new changes easy to isolate:
dropping raw attachment metadata saved 48,578,560 B in a full-size trial.
Reconstructing timestamps from message IDs saved another 116,301,824 B after
accounting for the small override flag. Those changes saved 164,880,384 B
together. The legacy archive contained 43,121,748 raw attachment bytes and
118,078,786 timestamp-string bytes before SQLite page and row overhead.

The v7 archive is smaller than the v9 archive because its earlier migration
already discarded historical names and raw timestamps. A fresh v7-to-v9 copy
measures **777,445,376 B**, or 27.06% below its own 1,065,906,176 B baseline.
Its missing historical fields cannot be reconstructed. The original-to-v9 path
above preserves the requested historical names and exact timestamps.

## What occupies the final file

| Component in the v9 legacy-derived file | Bytes |
| --- | ---: |
| Message rows, including exact text and image links | 300,077,056 |
| FTS5 trigram posting data | 114,638,848 |
| Channel, server, and author filter indexes | 289,091,584 |
| Historical name references and timestamp flags | 64,114,688 |
| Unique public message ID index | 63,025,152 |
| Historical name dictionary and its unique index | 2,232,320 |
| Other tables, indexes, and SQLite structure | 10,600,448 |

The dominant storage is query access, not filler rows. The old FTS posting data
was 519,434,240 B; assigning small internal row IDs brought it to 114,638,848 B.
FTS5 stores posting IDs as differences encoded with variable-length integers,
which explains the large measured saving on this archive of sparse Discord IDs.
The app still stores the exact public ID and uses it for ordering and date
filters. SQLite describes this in its [FTS5 doclist format](https://www.sqlite.org/fts5.html#the_key_doclist_format).

The original archive repeated 216,985,114 bytes of server, channel, and author
names across messages. The new schema keeps historical name values in a shared
dictionary and records compact references for every old message. Current names
remain in their metadata tables; future name changes cannot silently change a
message's historical name.

The source's original timestamps had two UTC text shapes. A full SQL comparison
proved that **all 3,690,778** values reconstruct exactly from their message IDs,
including 3,730 timestamps without fractional seconds. The view returns those
exact strings. Other archives may contain unusual formats or NULLs; the
migration stores and verifies those exceptions instead of changing their values.
The raw legacy attachment string is the one intentionally discarded active
field. The app still retains and checks every image link it uses. The recovery
backup retains the raw original field.

`channel_authors` now uses `WITHOUT ROWID`, shrinking its table and primary-key
index from 5,275,648 B to 2,469,888 B on this archive. SQLite documents the
single-tree benefit for composite keys in its
[WITHOUT ROWID guide](https://www.sqlite.org/withoutrowid.html). Three compound
indexes keep newest-first channel, author, and server filters fast after older
messages are collected out of order. FTS5 already uses `detail=none` and
`columnsize=0`, SQLite's compact options for indexed substring matching.

## Recovery copies and total disk use

Migration first creates a consistent SQLite backup with the
[online backup API](https://www.sqlite.org/backup.html). After the active file
passes SQLite integrity and FTS external-content checks, Searchcord compresses
the backup with gzip, reads it back, and compares SHA-256 digests byte for byte
before removing the uncompressed copy. Failed conversion or compression leaves
the raw recovery file in place. A verified recovery copy remains after upgrade.

The original 1,664,700,416 B backup compressed to **333,110,734 B** in the
full-size verification. The active 843,780,096 B file plus that recovery copy
totals **1,176,890,830 B**, 487,809,586 B (29.30%) below the original single
file. These are steady-state sizes. Migration temporarily needs space for the
original, backup, journal, new tables, and `VACUUM` work. The compressed backup
contains private messages, old attachment fields, and the saved token; handle
it like the database. Decompress a `.bak.gz` file before opening it with SQLite.

The already-indexed v7 source compresses less: its recovery copy was
643,191,886 B. Its active file plus backup totals 1,420,637,262 B, temporarily
354,731,086 B (33.28%) above the v7 starting file. The active database alone
is 27.06% smaller. After checking the migrated app and keeping any recovery
copy needed, the owner can remove that backup separately to realize the net
disk saving. The migration does not silently delete a recovery copy to make
its storage result look better.

## Data checks and application verification

Both full-size paths retained all 3,690,778 messages. The legacy migration
compared every retained original message field with the active view or table:
IDs, text, per-message names, and original timestamps had **zero mismatches**.
Image links reconstructed from raw attachments also had **zero mismatches**.
The compact migration compared every original v7 message field with zero
mismatches. Pre-existing application metadata tables matched in both directions.
The original source archive's checksum remained unchanged.

Migration checks counts and field values inside a transaction before dropping
the old message table. IDs must round-trip exactly through integer storage;
unknown message columns and unrepresentable IDs cause rollback. A recovery
backup is made first. SQLite `integrity_check` and FTS5's external-content
[integrity check](https://www.sqlite.org/fts5.html#the_integrity_check_command)
run after repacking. A second startup skips migration. Local regression checks
covered exact historical values, unusual timestamps, NULLs, Unicode, rollback,
out-of-order inserts, index updates, deletion, and backup failure behavior.

All 69 browser checks passed on the final credential-free v9 full-size copy.
They exercised real search and filters, URL restoration, autocomplete,
six charts, contributor drilldowns, responsive layouts, reduced motion, and
error states. Discord writes and destructive actions were intercepted.

## Throughput: what costs time

The application writes up to 100 messages per Discord page through one SQLite
transaction. A controlled local test ran three 10,000-message passes through
the actual save function for each configuration. Its median rates were:

| Synthetic write configuration | Messages per second |
| --- | ---: |
| Full v9 app | 8,961 |
| FTS insert trigger disabled | 16,249 |
| Cached-statistics insert trigger disabled | 11,214 |
| Three filter indexes removed | 11,991 |

These are isolated variants, not additive savings or live Discord rates. FTS
index maintenance is the largest local write cost. Removing it would make
substring search scan the message table; cached counts and filter indexes serve
other interactive views. Normal collection is more likely constrained by API
requests: the application paces requests at no more than 40 per second, so even
100 messages per page yields a theoretical 4,000 messages per second before
network time, smaller pages, Discord route limits, or retries. Discord documents
its per-route and global limits in the [rate-limit guide](https://docs.discord.com/developers/topics/rate-limits).
Incremental cursors prevent repeated full-history downloads, a larger gain than
removing the local search structures.

Search results are still fast on the full archive, but common-term counts and
deep offset pagination can grow with matching rows. The final browser run
observed 12 ms for the first results, 64 ms for a text search, 34 ms for
statistics, and 45 ms for contributor paging. These are individual local
observations, not load-test guarantees. The v9 migration of the 3.69-million
message legacy copy took 347 seconds before backup compression; the verified
gzip step took about 46 seconds on the same machine.

## Is this the smallest possible database?

No absolute minimum is established. This is the smallest tested layout here
that retains the requested values and the app's indexed search behavior. A
separate 8 KiB SQLite page-size trial saved 3,358,720 B (0.40%) on a near-final
copy. Changing page size on existing WAL databases requires a journal-mode
transition and another full rewrite, so that small gain is not included in the
automatic migration. SQLite documents the [page-size constraint](https://www.sqlite.org/pragma.html#pragma_page_size).

Dropping any one of the three filter indexes would save about 96 MB on this
archive, but makes its filter slow on selective searches. This archive had zero
message-to-channel server-ID mismatches, so its server IDs could in principle
be looked up through the channel table. That would allow removing the 96,362,496
B server index and the repeated message field here. The application also handles
other archives, DMs, and out-of-order histories. A guild-only search across many
channels would then need to merge or sort their results, and a channel metadata
change could rewrite historical membership unless exceptions were stored. The
current archive has only two channels, so its fast lookup trial is not enough
to establish a safe general replacement. This candidate is left out of v9.

Removing FTS would save about 115 MB but lose indexed substring search. A
previous read-only audit found about 19 MB of directly repeated message text;
a text dictionary would add lookups and an index and has no established net win.
None of these candidates is claimed as a safe saving. SQLite's
[FTS5 guide](https://www.sqlite.org/fts5.html#the_trigram_tokenizer)
describes the index options and their query trade-offs.

The app uses a configurable data directory and relative default path. No
machine-specific database path was added. The root `tests/` and `benchmarks/`
directories were removed as requested; disposable checks ran locally outside
the shipped tree. The earlier [throughput audit](throughput-audit.md) remains
as a historical record of the previous schemas.
