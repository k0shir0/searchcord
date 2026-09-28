# Search-only display

`search_app.py` serves `static/search/` at `http://127.0.0.1:8001`.
It is independent of the collection application's startup, migration, tokens,
background jobs and frontend bundles. The branch starts from `main` at `d2ea53c`.

## Interface

The Swiss direction uses left-aligned type, a shared column grid, clear rules,
restrained color and a single primary action. The existing `searchcord` wordmark
and “What are you looking for?” hero remain. Navy, mint, lavender and pale-blue
outlines keep the established theme. SVG circles, triangle and square use navy
clearance strokes; the grid continues through their transparent interiors.
No generated bitmap assets, framework runtime, remote fonts or CDN dependencies
are needed.

The controls, counters, message rows and pagination reuse the collection
frontend's `bauhaus.css` component styles. The search-only composition remains,
but the added eyebrow, tagline, search hint, technical results summary and
numbered message list have been removed at the user's request.

Focus or click the search field to expand its filter tray; Escape and outside
focus/click close it. Changing a filter or clearing filters immediately runs the
search, matching the collection frontend. Enter submits the main search and
scrolls to results. Suggestions remain bounded and delayed; IDs disambiguate
names. Dates include the complete selected end date in UTC. Short queries work
again, with the same SQL deadline protecting expensive scans.

Results use the original compact article rows: circular avatars, author and local
time, server/channel and author ID on the heading line, followed by full message
text. Real Discord avatars load lazily when the archive has a valid avatar hash;
otherwise the original geometric avatar fallback appears. Archived content is
inserted as text nodes with literal highlighting. There are no numbered list
markers or truncation disclosures. The frontend requests 50 messages per page.
It retains cursor pagination and request cancellation for performance, and a
screen-reader-only status announces completion without showing technical timing.

The two archive totals use the original equally sized bordered, filled boxes,
number font, weights and captions. On mobile, the search button stacks beneath
the input and filters use the original responsive grid. Reduced motion disables
transitions. The existing privacy page and its stylesheet now live under
`static/search/`; its footer link is on this frontend only, and its return link
returns to this search page. The privacy page is still the existing title-only
placeholder, not new policy text.

## Read path

The display app requires schema v9 and opens the archive with `mode=ro` and
`query_only`. It never imports `app.py`, runs a storage migration, reads settings,
or writes new indexes to the archive. Missing or unsupported archives produce a
useful error without creating a database. Explicit snapshot mode also enables
`immutable=1`; it is only for a closed archive and refuses a nonempty WAL.
The archive must remain unchanged throughout a snapshot session.

Unfiltered browse uses the existing message-ID index. Text search first inspects
up to 16,384 newest eligible messages, then uses the FTS trigram index for older
matches if the page is not full. Both paths apply the same literal substring
predicate and cursor boundary. Only the selected page is joined against
`message_records`, preserving historical names and exact stored timestamps.
The next cursor is the last returned Discord ID, not SQLite's internal rowid.

This avoids gathering every common-term match before displaying the newest page.
The extra candidate establishes whether Next is available; no full match count
is calculated. Cached archive counters provide the two homepage totals. Each
request runs in a read transaction so the candidate and metadata reads agree.

SQLite's [trigram documentation](https://sqlite.org/fts5.html#the_trigram_tokenizer)
notes that indexed LIKE needs a literal run of at least three characters and
does not accelerate LIKE with ESCAPE. The fallback therefore selects a literal
run for the FTS predicate and applies an escaped exact substring predicate on
messages. `%` and `_` in the user's query are literal characters. Queries without
three consecutive non-wildcard characters use a literal content scan, bounded
by the same SQL deadline. LIKE retains SQLite's default ASCII case-insensitive behavior;
non-ASCII case variants are not promised to match.

SQL has a four-second progress-handler deadline. A sparse filter or uncommon
phrase can still require more index work, so the interface asks for narrower
filters if the deadline is reached. This is a bounded interactive search, not
a promise that every possible query finishes in a few milliseconds. Aborting a
browser fetch prevents stale display; an already executing SQL query may continue
until it finishes or reaches its deadline.

## Validation

Run synthetic API and storage correctness checks:

```bash
python -m unittest discover -s checks -p "test_*.py" -v
```

Run browser checks against a running display server (Node with built-in WebSocket
and local Chrome required; set `CHROME_PATH` if it is not in the standard Windows
location):

```bash
node checks/browser_search.mjs http://127.0.0.1:8001 agents/search-browser
```

The browser checks exercise real searches without printing archived values.
Screenshots of message results use synthetic content. Home screenshots contain
aggregate counts only. Generated screenshots, profiles and logs stay ignored
under `agents/`.

Benchmark a closed archive without printing message text, names or IDs:

```bash
python checks/benchmark_search.py /path/to/searchcord.db --rounds 5
```

This compares the old candidate/sort query (without its additional full count
or metadata hydration) with the complete new search operation, verifies identical
first-page IDs, and checks that archive size and modification time are unchanged.
Timings are local measurements, not a cold-disk or multi-user load guarantee.

### Measured on 2026-09-28

The provided local fixture contains **3,691,078 messages**, schema v9, and
844,763,136 bytes. Five sequential benchmark rounds returned identical first-page
IDs and left its size and modification time unchanged.

| Search | Old candidate query median | New full search median |
| --- | ---: | ---: |
| Browse newest | 0.3 ms | 2.2 ms |
| `hello` | 259.0 ms | 8.1 ms |
| `the` | 1,544.6 ms | 3.7 ms |
| `release notes` (no matches) | 9.1 ms | 18.4 ms |
| `zzzzunlikelymatch` (no matches) | 3.5 ms | 12.7 ms |

The bounded scan adds about 9 ms to these no-match searches in exchange for
substantially faster common-term searches. Initial uncached probes of the old
candidate query took 2.8–3.9 seconds; those are not used for the warm comparison
above. The new full search includes metadata hydration; the old candidate
measurement excludes it and its separate result count.

Separate five-request HTTP probes measured approximately 7 ms for browse,
12 ms for `hello`, 8 ms for `the`, and 17–23 ms for the no-match examples after
warmup. Desktop Chrome loaded the page in 158 ms in a recorded run; browser
interaction timing includes test-driver polling and should not be confused with
SQL or HTTP timings. Results depend on disk cache and machine load.

After the parity correction, seven synthetic test cases and 42 browser assertions pass. Coverage includes
out-of-order history backfills, all cursor pages, literal wildcard handling,
historical names, inclusive dates, filter scoping, read-only enforcement,
unsupported archives, browser back/reload, stale responses, hostile HTML,
full message rendering, automatic filter application, privacy navigation, mobile
fit, reduced motion and requests restricted to local assets and Discord avatars.
