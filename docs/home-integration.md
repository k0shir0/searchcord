# Bauhaus home integration

Historical report. The root `benchmarks/` and `tests/` directories were removed
on 2026-09-25. Commands below describe the earlier verification runs. See the
[current storage audit](storage-audit.md) for the schema and measurements.

The approved trial is now the application home. Search, filters, counts, results,
settings, and navigation use the existing FastAPI service. Trial sample messages
are not part of the production page. The normalized storage, cursor recovery,
profile harvesting, benchmarks, and throughput notes already present in the
working tree are included as the backend foundation for this integration.

## Control contracts

| Surface | Endpoint / behavior |
| --- | --- |
| Archive totals, server/channel suggestions | `GET /api/search/filters?include_users=false` |
| Author suggestions | `GET /api/search/authors?q=...`, at most 20 suggestions |
| Search and pagination | `GET /api/search`, 50 messages per page |
| Text, server, channel, author | `q`, `guild_id`, `channel_id`, `author_id` |
| UTC dates | `date_from`, `date_to`; inclusive selected end day |
| Stored profile avatars | Bounded profile join for returned authors; geometric fallback |
| Image links | Existing `/api/images/{message_id}/{index}` redirect |
| Settings | Existing settings, token validation, and archive-clear endpoints |
| Channel and DM collection | `#scrape`, explicitly connect Discord, shared queue |
| Live monitoring | Channel monitor controls and live feed within `#scrape` |
| Statistics | `#stats`, cached aggregates and paginated contributors; see [workspace report](workspace-redesign.md) |

Suggestions contain stable IDs to distinguish identical server/channel names.
Unresolved text is rejected visibly instead of silently widening the query.
Pagination retains the submitted query, and stale search responses cannot replace
newer results. URL parameters restore the search across reloads. Clear filters
preserves the text query. Collapsed filters are inert, Escape closes the tray,
and reduced-motion preferences disable decorative motion.

The default data directory is application-relative `data/`. Set
`SEARCHCORD_DATA_DIR` to select a different directory; relative overrides are also
application-relative. No private absolute path is committed.

## Verification

The following records the original home integration. See the
[workspace report](workspace-redesign.md) for the expanded current test results.

- 21 Python tests pass, including HTTP pagination bounds, date validation,
  indexed avatar lookup, migration, search, stats, export, and scraper regressions.
- 29 Chromium checks pass against an isolated copy of the full personal archive:
  3,690,778 messages, successful migration, `integrity_check=ok`. The source was
  opened immutable/read-only and its SHA-256 matched before and after preparation.
- Credentials were removed from the copy before migration. External browser
  traffic was blocked. No remote scraping, deletion, or live collection was run.
- Browser checks include real query highlighting, ID and suggestion filters,
  same-day UTC dates, author autocomplete, pagination, URL restoration, empty and
  invalid states, settings focus, view navigation, and offline chart fallback.
- Visual/layout checks: 1440px desktop, 768px tablet, 375px mobile, expanded and
  collapsed tray, and reduced motion. No horizontal overflow at checked widths.
- One warm-cache browser run: first page 11 ms, second page 12 ms, text query
  75 ms, filters 9 ms, authors 9 ms, stats 30 ms. These are local observations,
  not general performance guarantees.

Run `python -m unittest discover -s tests -v` and `node --check static/app.js`.
For browser checks, start the app on a disposable real-data copy with no saved
credentials, then run `node tests/browser_home.mjs http://127.0.0.1:8766`.
Set `CHROME_PATH` if Chromium is not at the normal Windows Chrome location.
The script writes screenshots and aggregate results under ignored `agents/`.

Live Discord operations, attachment refresh, and CDN avatar success paths are
not part of the real-data browser test. Mocked backend tests cover scraping and
expired image refresh. The current browser suite also exercises collection with
synthetic responses, while all archive reads use the isolated real-data copy.
Chart.js is now bundled locally, with data tables available if it cannot load.
