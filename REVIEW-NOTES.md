# Overnight branch review, 2026-10-02

Reviewed `improve/search-performance-readme` at `3343b15` against `main` at
`6ee9255`. The branch contains two commits, 19 changed files, 596 added lines,
317 removed lines, and no deleted files. Of the removed lines, 288 were README
text. Full file-by-file findings and the original patch are in
[the HTML report](review/2026-10-02/index.html).

## Review corrections

- Preserve the collector's cursor page number on reload. When a shared link lacks
  earlier cursor history, offer an explicit first-page action.
- Allow only the search runtime into the Docker build context and image, rather
  than sending local worktrees and copying the entire collector frontend.
- Reuse the existing optional `dbstat` diagnostics in the benchmark so stock
  SQLite builds can still measure latency without that extension.
- Restore recovery-backup details and links to the existing technical docs.
- Add a packaged-server process test and an offline collector browser test.
  Existing viewer browser checks can block external HTTPS with
  `SEARCHCORD_OFFLINE_TEST=1`; pointer checks wait for layout to settle.

## Verification

- The unmodified branch passed 44 backend tests and the existing cursor DOM test.
- The review passes 45 backend tests, including launching the standalone package
  from outside the repository and checking that its archive stays unchanged.
- 89 Chrome checks passed: collector 20, packaged viewer 42, packed profile
  viewer 27. These use synthetic data and block external resources. No real
  credentials, account connections, joins, scraping, or backfill were used.
- A real browser reproduced the cursor reload failure before the fix and passed
  the same check afterward.
- The 100,000-message benchmark checks identical result IDs and profile totals.
  Common-query median improved from 80.797 to 4.231 ms, and profile history from
  92.945 to 19.247 ms. Rare and absent queries were slower by about 7-8 ms.
  Three rounds, warm OS cache, direct Python calls, not HTTP or capacity claims.
- Docker's daemon is unavailable here, so an actual container build/run remains
  unverified. The standalone Python package passed process and browser tests.
- The HTML report passes 14 additional browser checks, including both logo
  choices, dark/light previews, PNG exports, 320px layout and Chrome's actual
  `Arial-BoldMT` font report. These are separate from the 89 application checks.

## Reproduce offline

Use a Python environment with the repository requirements plus `httpx`, and
Node/Chrome for browser checks. Choose new fixture and package directories.

```powershell
python -m unittest discover -s checks -v
node checks/test_browse_cursor.mjs
python checks/snapshot_fixture.py agents/review-fixture
python deploy/search/package.py agents/review-bundle
python checks/benchmark_review.py agents/review-benchmark --rows 100000 --rounds 3 --report agents/benchmark.json
```

Start the collector against **only the synthetic fixture**, in one terminal:

```powershell
$env:SEARCHCORD_DATA_DIR = (Resolve-Path agents/review-fixture/source).Path
python -m uvicorn app:app --host 127.0.0.1 --port 8026 --no-access-log
```

Start the packaged viewer in another terminal with an absolute archive path:

```powershell
$archive = (Resolve-Path agents/review-fixture/packed/searchcord.db).Path
python agents/review-bundle/search_app.py --db $archive --snapshot --port 8028
```

Then run:

```powershell
$env:SEARCHCORD_OFFLINE_TEST = '1'
node checks/browser_review.mjs http://127.0.0.1:8026 agents/review-evidence/collector
node checks/browser_search.mjs http://127.0.0.1:8028 agents/review-evidence/viewer
node checks/browser_profile_search.mjs http://127.0.0.1:8028 agents/review-evidence/profiles
```

Stop those test processes afterward. No personal archive is needed.

## Branding and integration

Two preview logos use the same cleaned symbol and **real Arial Bold** lettering.
The symbol colors are menu blue `#8abde8` and deeper blue `#315a87`; `Cord` uses
the existing mint `#8dd8b0`. Browser/SVG masks apply exact flat colors. Helvetica
and Univers were not installed; neither their font files nor a substitute
labelled as those fonts is bundled. Generated lettering is not used.

Public searches found similar stock chat/search symbols, but did not establish
an exact company match. Exclusivity is unresolved. See the linked comparisons
in the report. These are visual trials, not a claim of trademark clearance.

The user selected option A and authorized publishing the reviewed branch to
main on 2026-10-02. The README now uses menu blue with real Arial Bold and mint
Cord, with light/dark PNG variants to preserve the approved typography on every
platform. The in-app hero keeps its existing theme and layout. See
`static/brand/README.md` for the asset palette and source.

After applying A, all 45 backend tests and the cursor DOM regression check
passed again. Chrome passed 16 report/branding checks, including loading the
README's actual picture markup in both light and dark color schemes.

The separate, unmerged
`codex/scrape-automation` branch (`7043918`) is not part of this diff; this branch
does not remove it, and merging this branch alone will not install its features.
