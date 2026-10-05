# Search-only display

`search_app.py` serves `static/search/` at `http://127.0.0.1:8001`.
It is independent of the collection application's startup, migration, tokens,
background jobs and frontend bundles.

## Interface

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

The display app accepts schema v9 or a verified search snapshot and opens the
archive with `mode=ro` and `query_only`. It never imports `app.py`, runs a storage migration, reads settings,
or writes new indexes to the archive. Missing or unsupported archives produce a
useful error without creating a database. Explicit snapshot mode also enables
`immutable=1`; it is only for a closed archive and refuses a nonempty WAL.
The archive must remain unchanged throughout a snapshot session.

For a schema v9 archive, unfiltered browse uses the existing message-ID index.
Text search first inspects up to 16,384 newest eligible messages, then uses the FTS trigram index for older
matches if the page is not full. Both paths apply the same literal substring
predicate and cursor boundary. Only the selected page is joined against
`message_records`, preserving historical names and exact stored timestamps.
The next cursor is the last returned Discord ID, not SQLite's internal rowid.

Packed snapshots use compressed blocks, dense references, a contentless trigram
index and a short-text block index. Matches are checked against full decoded text;
Public IDs still define dates and cursors.

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
