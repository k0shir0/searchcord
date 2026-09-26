# Browse, Scrape, and Stats

Historical report. The root `benchmarks/` and `tests/` directories were removed
on 2026-09-25. Commands below describe the earlier verification runs. See the
[current storage audit](storage-audit.md) for the schema and measurements.

This report covers the production workspace redesign after `3106e93`. The
[original integration report](bauhaus-diff-report.md) covers the home promotion,
storage foundation, and every standalone page draft created before this change.

## Pages and controls

Routes are relative to a running Searchcord server. Production views share one
HTML document and retain the configured local database path. No private machine
path or archive content is embedded in the application.

| Page or surface | Route / access | Source | Result |
| --- | --- | --- | --- |
| Browse home | `/#browse` | [index.html](../static/index.html) | Integrated search, full-width equal totals with exact numbers, then the navigation boxes and compact message results |
| Scrape | `/#scrape` | [index.html](../static/index.html) | Channel and DM source tabs, name filter, shared queue and live monitor without a separate hero banner |
| Stats | `/#stats` | [index.html](../static/index.html), [stats.js](../static/stats.js) | Archive totals, paginated contributors and six interactive charts without a separate hero banner |
| Settings | Geometric button beside wordmark | [index.html](../static/index.html), [app.js](../static/app.js) | Outline slider motif, keyboard focus, token errors |
| Collection progress | Scrape, start or view progress | [index.html](../static/index.html), [app.js](../static/app.js) | Shared accessible dialog, explicit scrape/deletion job routing |
| ChatML export | Scrape, direct messages, export | [index.html](../static/index.html), [app.js](../static/app.js) | Keyboard-selectable participant, themed dialog |
| Privacy & data | `/privacy.html` | [privacy.html](../static/privacy.html) | Documents local Chart.js and combined collection workspace |
| Progress draft | `/trials/progress.html` | [progress.html](../static/trials/progress.html) | Replaces checkmark controls with numbered steps |
| Earlier draft gallery | `/trials/` | [index.html](../static/trials/index.html) | Existing independent drafts remain available for iteration |

Old `#dms` and `#live` links resolve to Scrape. The three navigation boxes sit
below the Browse archive totals and directly below the wordmark on other views.
They have distinct lavender, blue and mint accents. Search results and incoming
messages use an avatar-left, username-and-timestamp-first layout while keeping
server and channel context visible in the same compact row. UI emoji and the
redundant local-ready message are removed; actual failures and running-job status
remain visible. Archived message content and names are preserved verbatim.

Scrape retains per-channel limits, optional profile harvesting, incremental
cursors, stopping, live monitoring, export, and explicitly confirmed deletion.
Queue selections reset after a run, and the shared progress dialog cannot be
claimed by a second collection/deletion job. Stopping routes to the active job's
endpoint, with a visible retry state when a stop request fails.

## Statistics

- Contributor pages contain 50 senders. Scroll to fetch more or use **load more**.
  A name/ID search works across the archive, not just the currently loaded page.
  Selecting a sender opens Browse with its author ID.
- Server charts support bars and a share chart. Channel charts show the top 20.
  Selecting a server or channel opens the corresponding search.
- The daily timeline supports 7, 30, 90, 365 days or all history, plus line/bar
  views. Windows end at the latest archived date so older archives remain useful.
  Missing days render as zero. Selecting a date searches that complete UTC day.
- Hourly, weekday and monthly charts show all-time activity. Selecting a month
  searches its inclusive UTC date range. Hover reveals exact counts.
- Every chart includes an accessible data table with equivalent drilldown
  buttons. If the chart script fails, tables open automatically and totals remain.

`GET /api/stats?days=90` adds channel, weekday and month aggregates plus timeline
boundaries. `days=0` means all history. Calculations use cached `stats_counts`,
without scanning all messages. `GET /api/stats/contributors` accepts `offset`,
`limit` (1 through 100), and `q` (at most 100 characters), returning `users` and
`has_more`. Ranking is deterministic by count descending, then author ID. Literal
wildcards in contributor names are escaped. Offsets describe the current archive;
rankings can move while live collection is adding messages.

Chart.js 4.4.0 is now served locally, with its MIT license. The vendored build
matches the SHA-256 of the previously pinned CDN dependency:
`0e2326c6868072bec1592760c6729043caeea2960a2b46cee6a2192aac6abff0`.

## Design rationale and sources

The [Getty Bauhaus form study](https://www.getty.edu/research/exhibitions_events/exhibitions/bauhaus/new_artist/form_color/form/)
discusses proportion, rhythm, weight, and relationships between lines and planes.
The [MoMA Bauhaus web project](https://www.moma.org/explore/inside_out/2009/11/13/bauhaus-from-weimar-to-the-web/)
uses a stable grid to organize changing interactive content. These informed the
design interpretation here: asymmetric visual weight inside a consistent layout,
rather than a row of equally sized symbols.

The secondary banners balance a large circle against a smaller triangle,
rotated square, bar, and detached rules. Each decorative outline has navy
clearance. Headings occupy a separate bay; mobile layouts place the composition
below the copy. The functional grid carries through source panels, charts and
full-row totals. Cool navy, lavender, mint, pale blue and porcelain connect all
three views. Settings uses a purpose-built inline SVG with separated controls.

Page transitions use brief opacity/transform motion, with a Web Animations
fallback. Superseded navigation settles on the latest requested view. The
[ViewTransition ready contract](https://developer.mozilla.org/en-US/docs/Web/API/ViewTransition/ready)
requires handling transitions that cannot start. Reduced motion disables page
and chart animation. Chart hover and selection use the library's
[interaction configuration](https://www.chartjs.org/docs/latest/configuration/interactions.html).

## Verification

- 28 Python tests pass: the existing migration, scraping, search and export suite,
  five statistics contracts, and two isolated Git-audit regression tests.
- 69 Chromium checks pass against the real archive copy with 3,690,778 messages.
  Tests cover
  search, autocomplete, dates, pagination, URL restoration, contributor scrolling
  and searching, table and canvas drilldowns, hover, timeline ranges and chart
  styles, navigation races, motion fallbacks, and responsive layouts.
- Browser collection tests replace Discord endpoints and event streams with
  synthetic responses. Every write is intercepted. They exercise connection,
  channel/DM queuing, limits, profiles, progress, stopping, export selection,
  deletion confirmation/job routing, live feed, stop-all and keyboard source tabs.
- Desktop, tablet and phone widths are 1440, 768 and 375 pixels. No horizontal
  overflow is observed. Both total panels have equal widths, including tests
  with 13-digit counts. Screenshots are reviewed locally and remain ignored.
- External browser traffic is blocked. Local charts work offline; a separate
  blocked-script case confirms the accessible table fallback. No uncaught browser
  exceptions remain. Real authenticated Discord operations are not performed.
- The source archive remains read-only and its SHA-256 matches the preparation
  checksum. The tested copy had credentials removed before migration. No source
  archive schema or content was changed.
- JavaScript syntax and Git whitespace checks pass. The local code-only Graphify
  graph was refreshed without an external model call; generated files stay ignored.

Reproduce against a disposable credential-free archive copy:

```powershell
$env:SEARCHCORD_DATA_DIR = 'data/test-copy'
python -m uvicorn app:app --host 127.0.0.1 --port 8767 --no-access-log
# In another terminal:
python -m unittest discover -s tests -v
node tests/browser_home.mjs http://127.0.0.1:8767 agents/workspace-browser
```

Use `CHROME_PATH` when Chromium is not installed in the usual Windows location.
The browser suite expects a populated archive, including matches for `release`.
Its JSON report contains aggregate counts and timings only. Raw screenshots and
temporary browser profiles are ignored and must not be published with real data.

## Git privacy review

The audit scans every available local Git object, including unreachable objects,
all fetched refs, and reflogs. It checks for SQLite files, private-data paths,
Discord/GitHub credential patterns, private keys, structured message exports,
and exact matches to locally saved credentials read through immutable read-only
connections. Findings contain only object IDs, paths and reason labels.

The review found no private archive or credential matches, so no history rewrite
was needed. This establishes the result for available local and fetched history;
it cannot inspect unavailable deleted remote objects or someone else's clones.
The scanner is regression-tested on synthetic reachable and unreachable objects.

```powershell
python benchmarks/audit_history.py --database data/searchcord.db
```

Only source, docs, tests and the licensed chart dependency belong in this change.
Databases, exports, browser output, credentials and Graphify output remain ignored.
The newer remote `LICENSE` update is preserved when integrating `main` before the
authorized push. Publication is a normal merge/push, without forcing remote refs.

## File-level diff

| File | Change |
| --- | --- |
| [.gitattributes](../.gitattributes) | Preserve the pinned vendor build's bytes across platforms |
| [app.py](../app.py) | Cached chart aggregates, archive-relative timeline, bounded contributor endpoint |
| [index.html](../static/index.html) | Three-view shell, shared Scrape controls, six-chart Stats view, SVG settings |
| [app.js](../static/app.js) | Transitions, collection state, text controls, modal focus, exact counters |
| [stats.js](../static/stats.js) | New pagination, chart rendering, accessible tables and search drilldowns |
| [workspace.css](../static/workspace.css) | New responsive production layout and motion |
| [chart.umd.min.js](../static/vendor/chart.umd.min.js) | Pinned local Chart.js 4.4.0 |
| [chart.LICENSE.md](../static/vendor/chart.LICENSE.md) | Dependency license |
| [privacy.html](../static/privacy.html) | Current network and deletion descriptions |
| [progress.html](../static/trials/progress.html) | Text step markers in the earlier draft |
| [audit_history.py](https://github.com/k0shir0/searchcord/blob/8669d6264e7cb1f983620a68e67acd976f699df4/benchmarks/audit_history.py) | Reproducible privacy audit without secret output |
| [test_stats.py](https://github.com/k0shir0/searchcord/blob/8669d6264e7cb1f983620a68e67acd976f699df4/tests/test_stats.py) | Pagination, filters, bounds, sparse dates, empty archive |
| [test_history_audit.py](https://github.com/k0shir0/searchcord/blob/8669d6264e7cb1f983620a68e67acd976f699df4/tests/test_history_audit.py) | Detection fixtures in temporary repositories |
| [browser_home.mjs](https://github.com/k0shir0/searchcord/blob/8669d6264e7cb1f983620a68e67acd976f699df4/tests/browser_home.mjs) | Expanded real-data browser checks |
| [browser_scrape.mjs](https://github.com/k0shir0/searchcord/blob/8669d6264e7cb1f983620a68e67acd976f699df4/tests/browser_scrape.mjs) | Synthetic collection and event-stream checks |
| [README.md](../README.md) | Current navigation, workflow and local chart dependency |
| [Design preferences](../BAUHAUS-DESIGN-PREFERENCES.md) | Current three-view, full-width, cool-geometry direction |
| [Design plan](../BAUHAUS-DESIGN-PLAN.md) | Updated production status |
| [Home integration](home-integration.md) | Current routes and verification pointers |
| [Earlier diff report](bauhaus-diff-report.md) | Explicit historical status and link to this report |
| [This report](workspace-redesign.md) | Page links, research, contracts, verification and privacy audit |
