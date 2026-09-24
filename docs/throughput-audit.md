# Scrape and storage throughput audit

Measured on 2026-09-22 using disposable synthetic data and the current Windows
Python environment. Reproduce the write measurements with
`python benchmarks/throughput.py` after installing the existing requirements.
No private message bodies or token values were read for this audit.
The 2026-09-22 and early 2026-09-23 after-state measurements below are historical
snapshots. The compact storage measurements at the end describe the current schema.

## Before state

- The scraper requested up to 100 messages per page, walked backward through
  every selected channel on every run, then opened a fresh SQLite connection
  and committed each page. A fixed 400 ms delay followed full pages.
- The local database was 110,592 bytes: 0 messages, 1 settings row, 27 pages,
  18 free pages. The message table had a text primary key and four B-tree
  indexes (`channel_id`, `guild_id`, `author_id`, `timestamp`). It repeated
  `guild_name`, `channel_name`, and `author_name` in every message row. The
  current database had no duplicate message rows or repeated name bytes to
  remove because it had no messages. `dbstat` attributed one 4,096-byte page
  each to `messages`, `settings`, their primary-key indexes, and each of the
  four secondary indexes; the other pages were schema or free space.
- Author ID, content, and attachment URLs were already stored. There was no
  profile table, profile-saving path, or inference path. Inference is outside
  this app's scope, as confirmed by the user.
- The database write proxy matching the old 100-row connection/commit pattern
  measured 10,508 messages/s and 9.52 ms per batch with Python's synchronous
  SQLite driver. The actual async path is compared below. Live Discord API
  latency was not measured; the fixed 400 ms pause and full-history re-fetch
  were visible code costs, not inferred network timings.

## After state and measurements

| Synthetic workload | Before | After | Notes |
| --- | ---: | ---: | --- |
| 10,000 message writes, 100 per page | 10,628/s | 11,566/s | Median of five runs through the actual `aiosqlite` paths; 0.094 vs 0.086 ms/message. The after path includes normalized tables, query indexes, and trigram search. |
| 1,000 unknown profile saves | 126/s proxy | 1,795/s | No profile saver existed before. The proxy applies the old per-write connection pattern to profile rows; the after path uses a mocked API with zero network latency. |
| Repeat profile pass over same 1,000 IDs | N/A | 0.55 ms, 0 fetches, 0 writes | Confirms stable-ID deduplication. |
| Inference | N/A | N/A | No inference path exists in Searchcord. |

The profile proxy omits a network request and duplicate lookup; the new path
includes both, with its network response mocked. The 126-to-1,795/s comparison
measures the connection/write strategy on synthetic inputs, not a historical
production profile-saver improvement.

The two-run scrape test saved 120 messages from two pages, then requested one
`after=120` page on the next run, fetching no old messages. A later burst of
130 messages used one `after` page plus one `before` page and saved all 130.
An interrupted or capped initial scrape retains its oldest cursor and resumes
older history on a later job. A capped incremental scrape retains a pending
gap so a later job still saves every new message between the old and new
high-water IDs. Profile fetching is opt-in, queries only missing
user IDs, and can populate profiles from an existing channel archive even when
no new messages arrive.

The then-current empty database migrated with a verified backup and passed SQLite
`integrity_check`. It has 0 redundant rows or repeated name bytes removed.
Its file grew from 110,592 to 118,784 bytes (+8,192) because new tables and
indexes cost space even with no messages. In the 10,000-row synthetic workload,
normalizing names removes 185,600 repeated-name bytes from message rows, but
the total file grows from 2,191,360 to 3,268,608 bytes (+1,077,248) because
the trigram and sort indexes add read capability. These are distinct logical and physical
size measurements; there is no claim of a net file-size reduction.
That intermediate schema stored shared name values and per-message dictionary
references to retain historical display names. The compact schema below keeps
stable IDs and current display names in metadata tables.

## Database decision and limits

Keep SQLite. This app is local and has one writer at a time; WAL allows readers
while scraping, and the indexed cursor and batch writes avoid repeated history
and connection churn. The synthetic after-state file is about 326 bytes per
message, implying roughly 3.3 GB at 10 million messages or 33 GB at 100
million for similarly short content. Real content and attachments can make it
larger. This is a rough storage projection, not a 100-million-row benchmark.

The trigram FTS5 index accelerates substring search at the cost of writes and
space. A 10,000-row selective-query probe measured 3.27 ms with a table scan
versus 0.71 ms with the trigram index; its indexed write took 0.83 s versus
0.36 s without FTS. Searches under three characters, whole-archive statistics,
exact result counts, and deep offset pagination still scale with archive size.
Those read paths need separate measurement and likely cached aggregates or
cursor pagination before promising interactive latency at 100 million rows.
The later compact migration retains SQLite and the trigram search index.

## Rate limits

Discord documents `after` and `before` pagination, a 1–100 message page size,
and 429 `retry_after` values in seconds. The scraper now sends one request at
a time per process, caps its global pace at 40 requests/s, waits when a route
reports no remaining capacity, and honors the full `Retry-After` interval on
429. Incremental jobs and stable-ID profile deduplication reduce request count.
These controls were tested with mocked responses; no live Discord call was
made for the benchmark.

The pagination and rate-limit behavior follows Discord's
[message endpoint](https://docs.discord.com/developers/resources/message#get-channel-messages)
and [rate-limit documentation](https://docs.discord.com/developers/topics/rate-limits).
The search index uses SQLite's [FTS5 trigram tokenizer](https://www.sqlite.org/fts5.html#the_trigram_tokenizer).

## Large archive read-only probe (2026-09-23)

`python benchmarks/large_readonly.py PATH_TO_DB` opens the separate personal
Searchcord database with SQLite `mode=ro&immutable=1`. The probe refuses a
nonempty WAL. It did not change that database or read settings, and it printed
only counts, timing, and file sizes. It did not load message rows into Python.
The source held 3,690,778 messages in a 1,664,700,416-byte file, with 2
guilds, 2 channels, and 55,816 distinct authors. A bounded 100,000-row sample
averaged 19 characters of message content, 5 characters of attachment text,
and 46.3 characters of repeated guild, channel, and author names per row. These are sample
averages, not exact total savings.

| Read-only SQL workload on the 3.69M-row source | Legacy time |
| --- | ---: |
| Eight stats aggregates, summed | 4.50 s |
| First page by timestamp, 50 IDs | <0.001 s |
| No-match substring count, full scan | 0.98 s |
| Guild and channel option queries | 0.96 s and 1.21 s |
| Author option query, time until first row | 1.95 s |

For a scale check, the benchmark materialized **only grouped counts** in a
disposable temporary database. That took 7.58 s and 2,064,384 bytes. Equivalent
stats queries against those counts took roughly 0.02 s in total, including
counting 55,816 author groups and ranking the top three. These are SQL timings,
not full HTTP/browser latency. The app now maintains those counts during
message inserts and deletes. Author filter suggestions use an indexed prefix
lookup capped at 20 options; the browser no longer requests the full author
list. SQLite's query plan confirms the `idx_authors_name` prefix index is used
in the new schema. The old archive was not migrated for this probe, so the new
search route was not timed on its 3.69M message rows.

The 10,000-row synthetic write benchmark now measures 7,781 messages/s and a
3,350,528-byte database with historical-name references and stats counts. The
earlier normalized schema measured 11,566 messages/s and 3,268,608 bytes
without those additions. Those medians came from separate runs and are not a
controlled isolated cost estimate. The measured write rate still exceeds the
process's 40 requests/s ceiling times 100 messages/page, before accounting
for Discord network latency.

The historical-name references in that intermediate schema have since been
removed. The source archive remained read only while a disposable copy was
migrated for the measurements below.

## Compact archive measurement (2026-09-23)

`benchmarks/compact_archive.py` copied the personal archive to an ignored,
temporary workspace directory, migrated the copy with the app's actual
`init_database` function, measured it, then deleted the copy and its backup.
The personal source was opened immutable/read-only and was never edited. Only
aggregate counts, timings, and sizes were printed. The migrated copy passed
`PRAGMA integrity_check`; its row count and total content length matched the
source exactly. All source message IDs fit SQLite's signed 64-bit integer, and
their snowflake-derived UTC day and hour matched the saved timestamps for every
row.

| Full archive, 3,690,778 messages | Legacy source | Compact copy |
| --- | ---: | ---: |
| SQLite file size | 1,664,700,416 B | 1,065,906,176 B |
| Bytes per message, including indexes | 451.0 | 288.8 |
| Total message text bytes | 93,527,827 | 93,527,827 |
| Messages with any attachment / retained image link | 171,622 | 134,725 |
| Stats API call on migrated copy | historical aggregate SQL: 4.50 s | 0.023 s |
| Newest 50 IDs, SQL | <0.001 s | 0.0001 s |
| Unmatched substring count, SQL | 0.98 s scan | 0.0075 s indexed |

The compact file saved **598,794,240 bytes (35.97%)**. The stats comparison
uses the older eight-query SQL aggregate total and the new Python endpoint
timing, so it demonstrates the latency order of magnitude but is not a paired
HTTP/browser benchmark. The endpoint returns cached counts and does not scan
the message table. The final full migration took 194.1 seconds, including the
backup, index build, vacuum, integrity check, and verified-backup removal on a
disposable copy. A prior run with repeated row IDs in the secondary indexes
took 198.6 seconds and produced a 1,169,088,512-byte file. Search latency
values in the table are SQL only, not rendered browser timing.

The final file is composed mainly of the 519 MB FTS trigram index, 288 MB
message table, and three 82 MB filter indexes. SQLite's implicit rowid ordering
lets each single-column filter index serve newest-first results with a reverse
scan, confirmed by `EXPLAIN QUERY PLAN`; repeating the message ID in those
indexes cost about 103 MB in the first migrated copy. Keeping the FTS index
preserves fast substring search. Deduplicating message text would eliminate
only 19.0 MB of directly repeated text before adding a dictionary and lookup
index; a net storage or latency win from that more complex search path was not
established. The compact schema keeps message text directly in each row. Images remain URL
references, never image bytes.

A single 10,000-row synthetic write run measured 19,089 messages/s and a
1,478,656-byte compact file, compared with 10,767 messages/s and 2,191,360
bytes for the old writer in the same script. These are one-run synthetic
results, not live Discord throughput. `GZipMiddleware` compresses browser API
responses over 1,024 bytes, while excluding event streams. On the full archive,
the stats HTTP response took 0.024 seconds and fell from 2,293 to 763 bytes
on the wire with gzip (66.7% smaller). The first 50-result search response
took 0.0096 seconds; gzip reduced it from 16,879 to 2,664 bytes (84.2%
smaller). Those HTTP measurements used an in-process ASGI client, not browser
rendering or a live network connection. An earlier compact-copy run took 0.4252
seconds for the same search page because the mixed integer/text ID joins scanned
all author metadata. Casting message IDs to text at those joins made SQLite use
the metadata primary-key indexes; the measured page time dropped about 44-fold
across the two runs. No live Discord request was part of this measurement.
