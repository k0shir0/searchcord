# Bauhaus integration and page drafts

Historical report for the initial home promotion. The subsequent
[workspace redesign report](workspace-redesign.md) documents the current
three-view application, local charts, additional verification, and publication.
The no-push statement and separate DMs/Live views below describe that earlier commit.

The trial is now the real home page. Home controls use the archive API, with
portable data-directory configuration and no embedded private machine paths.
Home geometry uses only cool colors. The other screens have separate visual
drafts ready for iteration.

Work remains on the existing local `main` branch. Home integration was committed
as `7e89725` (`promote bauhaus home, wire archive search, validate real data`).
The subsequent draft commit contains the gallery, seven secondary page sketches,
and this report. Nothing was pushed or published.

## Every changed or new page

Routes below are relative to a running Searchcord server. Source links open the
corresponding repository file. Home requires the server; standalone drafts also
work as local files except for their links back to the running application.

| Page | Route | Source | State |
| --- | --- | --- | --- |
| Home / search | `/` or `/#browse` | [index.html](../static/index.html) | Production, real endpoints |
| Existing DMs | `/#dms` | [index.html](../static/index.html) | Functional existing view in adapted shell |
| Existing Live | `/#live` | [index.html](../static/index.html) | Functional existing view in adapted shell |
| Existing Stats | `/#stats` | [index.html](../static/index.html) | Real counts, on-demand chart dependency |
| Privacy & data | `/privacy.html` | [privacy.html](../static/privacy.html) | Production data-handling notes |
| Draft gallery | `/trials/` | [trials/index.html](../static/trials/index.html) | Links all designs |
| DMs draft | `/trials/dms.html` | [dms.html](../static/trials/dms.html) | Conversation filter, queue toggles |
| Live draft | `/trials/live.html` | [live.html](../static/trials/live.html) | Feed and monitoring, local pause control |
| Stats draft | `/trials/stats.html` | [stats.html](../static/trials/stats.html) | Metrics, outlined bars, period selector |
| Settings draft | `/trials/settings.html` | [settings.html](../static/trials/settings.html) | Token-control sketch, no persistence |
| Archive Channels draft | `/trials/archive.html` | [archive.html](../static/trials/archive.html) | Channel queue and collection options |
| ChatML Export draft | `/trials/export.html` | [export.html](../static/trials/export.html) | Role selection and synthetic JSON preview |
| Scrape Progress draft | `/trials/progress.html` | [progress.html](../static/trials/progress.html) | Progress, saved cursor state, stop sketch |
| Previous privacy scaffold | `/trials/privacy-policy.html` | [privacy-policy.html](../static/trials/privacy-policy.html) | Links to the new production notes |
| Original home reference | `/trials/bauhaus-blue-purple.html` | [bauhaus-blue-purple.html](../static/trials/bauhaus-blue-purple.html) | HTML unchanged; shared CSS now has cool geometry |

The seven new secondary drafts use synthetic content only. Their controls do not
call the API, scrape, export files, delete messages, or store credentials. The
production DMs/Live/Stats and collection workflows have not been replaced by
these sketches.

## Behavior changes

- Search moves from a separate tab to the home banner. Focus opens the filters;
  Escape, outside click, or leaving the control area closes them. Hidden fields
  are inert, and motion respects the user's reduced-motion setting.
- Server and channel suggestions include stable IDs to avoid collisions between
  repeated names. Author suggestions remain bounded at 20 results. Invalid
  free text produces a visible validation state instead of an unfiltered query.
- Dates are inclusive UTC day filters. Reversed dates and invalid pagination
  receive explicit errors. Search results paginate at 50 rows, using previous,
  current-page, and next controls.
- Submitted filters and page are reflected in the URL and restored on reload.
  Pagination uses the submitted filters. New searches cancel old fetches and
  ignore stale responses. Clear filters retains the text query.
- Real result rows show escaped/highlighted text, identity and location metadata,
  timestamps, and existing image links. Indexed profile avatars use a bounded
  lookup; a deterministic geometric avatar is the fallback.
- Home counts come from indexed archive metadata. Searching requires no Discord
  token or validation call. Explicitly connect Discord in Archive channels when
  you want to collect messages or monitor channels.
- Settings is reachable in the header, with focus management and Escape support.
  Archive clearing refreshes home metadata, and scrape completion refreshes counts.
- Chart.js loads only when visiting Stats. If it cannot load, real counters still
  render and each chart has a visible offline fallback.
- `SEARCHCORD_DATA_DIR` selects an archive directory. Its default and relative
  overrides resolve against the application directory, independent of the
  process working directory. This change does not select the private source
  archive by default or migrate it in place.

## Design changes

`static/bauhaus.css` is the shared promoted design source. The original trial's
stylesheet imports it. `static/home.css` adapts the existing app's shell and
controls without replacing its functional secondary views. Home uses navy,
porcelain, lavender, mint, and pale blue, with separated whole outline shapes.
The previous glowing active-tab underline is removed.

`workspace-drafts.css` supplies the secondary draft layout. Circle, triangle,
square, and detached rules occupy separate spaces; narrow layouts stack cleanly.
Stats and Export add muted gold. Transitions are short, with no continuous
decorative animation. The drafts are individually editable HTML pages.

## Validation evidence

| Check | Result |
| --- | --- |
| Python regression suite | 21 tests passed |
| Production browser suite | 29 checks passed |
| Draft browser checks | 71 checks passed across 9 pages and 3 widths |
| Responsive widths | 1440, 768, 375 pixels |
| Real archive | 3,690,778 messages in an isolated local copy |
| Copied archive migration | Completed, row count retained, integrity check `ok` |
| Original archive | Opened immutable/read-only, before/after SHA-256 unchanged |
| Credentials | Removed from test copy before migration |
| JavaScript syntax | Production and draft scripts pass `node --check` |
| Git hygiene | No database, private record, credential, or browser profile staged |

The production browser checks exercise text search and highlighting, server and
channel suggestions, author autocomplete, combined ID and same-day date filters,
pagination, URL restoration, empty results, invalid filters, reversed dates,
settings focus, view navigation, chart-load failure, responsive layout, reduced
motion, and absence of uncaught browser exceptions. Screenshots were inspected
for desktop and mobile composition and control clearance.

The final warm-cache local browser measurements were 13 ms for the first page,
12 ms for page two, 64 ms for a text search, 8 ms for filter metadata, 8 ms for
author suggestions, and 30 ms for Stats. These are observations on this machine,
not broad performance claims.

External requests were blocked during browser checks. No real Discord scraping,
live collection, deletion, or credential validation was attempted. Successful
CDN avatar/chart loading and remote attachment refresh were not tested in that
browser run. Mocked backend tests cover scraping and expired-image refresh.

See [home-integration.md](home-integration.md) for endpoint contracts and commands.
The local review server uses the ignored `data/home-e2e` copy. The default app
continues to use `data/` unless `SEARCHCORD_DATA_DIR` is set.

## Scope of the diff

The comparison base is `bb33b9b`, the starting commit. The checkout already had
uncommitted normalized storage, cursor recovery, profile harvesting, search
performance, benchmark, test, README, and style work. Those existing changes were
retained, verified, and committed with the home integration because the current
endpoints depend on them. They are not all newly authored homepage work.

File-by-file accounting follows below.

| File | Added | Removed | Change |
| --- | ---: | ---: | --- |
| [.gitignore](../.gitignore) | 3 | 0 | Retained local-data and generated-artifact exclusions. |
| [BAUHAUS-DESIGN-PLAN.md](../BAUHAUS-DESIGN-PLAN.md) | 23 | 1 | Promotion status, draft inventory, and review boundaries. |
| [BAUHAUS-DESIGN-PREFERENCES.md](../BAUHAUS-DESIGN-PREFERENCES.md) | 12 | 0 | Current cool home palette and production control requirements. |
| [README.md](../README.md) | 95 | 12 | Current workflows, relative data configuration, and draft discovery. |
| [app.py](../app.py) | 621 | 152 | Retained backend foundation; data-directory configuration, bounded search pagination, date validation, profile avatar lookup. |
| [benchmarks/compact_archive.py](../benchmarks/compact_archive.py) | 113 | 0 | Existing disposable-copy migration and aggregate performance probe. |
| [benchmarks/large_readonly.py](../benchmarks/large_readonly.py) | 110 | 0 | Existing read-only aggregate archive benchmark. |
| [benchmarks/throughput.py](../benchmarks/throughput.py) | 145 | 0 | Existing synthetic write-throughput benchmark. |
| [docs/bauhaus-diff-report.md](../docs/bauhaus-diff-report.md) | 166 | 0 | This full linked change and validation report. |
| [docs/home-integration.md](../docs/home-integration.md) | 62 | 0 | Home control contracts, test evidence, reproduction notes. |
| [docs/throughput-audit.md](../docs/throughput-audit.md) | 200 | 0 | Retained storage and performance audit from earlier backend work. |
| [static/app.js](../static/app.js) | 454 | 190 | Home search, ID-safe filters, URL state, stale-response handling, result renderer, settings, chart fallback. |
| [static/bauhaus.css](../static/bauhaus.css) | 296 | 0 | Promoted shared design, cool outline geometry and clearance. |
| [static/home.css](../static/home.css) | 68 | 0 | Production shell adaptation, responsive controls, compact counts, cool palette. |
| [static/index.html](../static/index.html) | 85 | 76 | Real home composition and retained functional app views. |
| [static/privacy.html](../static/privacy.html) | 13 | 0 | Local data handling, external requests, retention, and deletion notes. |
| [static/style.css](../static/style.css) | 30 | 0 | Retained existing style changes used by collection/profile controls. |
| [static/trials/archive.html](../static/trials/archive.html) | 93 | 0 | New archive design draft with synthetic content. |
| [static/trials/bauhaus-trials.css](../static/trials/bauhaus-trials.css) | 2 | 289 | Imports the shared production design instead of duplicating it. |
| [static/trials/dms.html](../static/trials/dms.html) | 111 | 0 | New dms design draft with synthetic content. |
| [static/trials/export.html](../static/trials/export.html) | 79 | 0 | New export design draft with synthetic content. |
| [static/trials/index.html](../static/trials/index.html) | 63 | 24 | Complete linked design gallery. |
| [static/trials/live.html](../static/trials/live.html) | 94 | 0 | New live design draft with synthetic content. |
| [static/trials/privacy-policy.html](../static/trials/privacy-policy.html) | 14 | 17 | Replaces placeholder copy with a link to the production data notes. |
| [static/trials/progress.html](../static/trials/progress.html) | 77 | 0 | New progress design draft with synthetic content. |
| [static/trials/settings.html](../static/trials/settings.html) | 81 | 0 | New settings design draft with synthetic content. |
| [static/trials/stats.html](../static/trials/stats.html) | 107 | 0 | New stats design draft with synthetic content. |
| [static/trials/workspace-drafts.css](../static/trials/workspace-drafts.css) | 103 | 0 | Shared secondary draft theme, separated forms, responsive layouts, reduced motion. |
| [static/trials/workspace-drafts.js](../static/trials/workspace-drafts.js) | 58 | 0 | Synthetic local interactions only; no network or persistence. |
| [storage.py](../storage.py) | 268 | 0 | Retained compact schema, FTS, migration, historical names, counts, and cursor foundation. |
| [tests/browser_home.mjs](../tests/browser_home.mjs) | 141 | 0 | Reproducible browser test on an isolated archive; aggregate output only. |
| [tests/test_backend.py](../tests/test_backend.py) | 449 | 0 | Retained storage, scrape, search, export, and migration regression coverage. |
| [tests/test_home.py](../tests/test_home.py) | 59 | 0 | New HTTP boundary, date, avatar, home, and privacy coverage. |

Counts compare the final files against `bb33b9b`; they include retained pre-existing work.
