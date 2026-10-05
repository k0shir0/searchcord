# Collection automation verification

Verified locally on 2026-10-01. No private credentials, server/channel names,
or message contents are recorded here.

## Result

- 57 Python tests passed in the working tree: 53 tracked tests, plus the four
  pre-existing tests in the untracked `checks/test_scrape_retry.py`.
- 27 checks passed in real Chrome against the actual FastAPI routes, scrape
  worker, SSE progress, and a disposable SQLite archive with synthetic Discord
  responses. The real interval between two invite joins was **30.031 seconds**.
- Python compilation, JavaScript syntax checks, and `git diff --check` passed.
- A live Discord check saved exactly **four messages across two channels**
  with a cap of two per channel. SQLite integrity returned `ok`; zero credential
  settings were saved. That temporary archive was removed after the check.
- The refreshed credential authenticated, but the live join returned HTTP 403
  with Discord error **340015**. Successful live joining is **unverified**, as
  accepted by the user. The account restriction is handled by stopping the
  whole invite queue and showing an Account Standing notice. Its interpretation
  is recorded in the [Discord API types](https://github.com/discordjs/discord-api-types/blob/main/rest/common.ts).
- The running personal collector was not restarted or edited. Test credentials
  stayed in process memory; test data was not written to either main archive.

## Coverage

The browser run exercised native double-click, Shift+Enter, and the scan button;
automatic scraping and SQLite persistence; mixed-case `bot` exclusion; manual
bot queuing; readable empty channels; private, view-only, and denied channels;
thread discovery only during scans; automatic continuation behind an active
scrape; stop with queue preservation; failed scans without partial automatic
scrapes; existing limits and profile controls; mixed invite formats and duplicate
codes; invalid input without a join; navigation and reload recovery; stopping
the next pending invite; successful-join server-list refresh; DM controls;
1440, 768, 375, and 320px layouts; and no uncaught browser errors.

Backend cases also cover permission overwrite precedence, owner/administrator
access, inherited thread permissions, paginated thread deduplication, transient
probe failures, same-invite rate-limit retries, cooldown preservation across
stop/start, verification and account restrictions, and safe progress responses.

## Reproduce

Use a Python environment with the project requirements installed:

```powershell
agents\app-venv\Scripts\python.exe -m unittest discover -s checks -v
agents\app-venv\Scripts\python.exe -m py_compile app.py channel_access.py invite_api.py
node --check static/app.js
node --check static/collection.js
node --check checks/browser_collection.mjs
git diff --check
```

The browser fixture uses no real Discord account or production database. Start
it in one terminal, then run the browser check in another. Chrome/Chromium and
Node with a built-in `WebSocket` are required; set `CHROME_PATH` if needed.

```powershell
agents\app-venv\Scripts\python.exe checks/collection_fixture.py --port 8016
node checks/browser_collection.mjs http://127.0.0.1:8016 agents/collection-browser
```

Reports and screenshots belong in the ignored `agents/` directory. Stop the
fixture after the check. It creates a disposable archive in the system temporary
directory and removes it on normal shutdown. The live check is intentionally
private and is not part of the tracked test suite.

## Runtime limits

Invite progress survives page navigation and reloads while the application stays
running; restarting the application ends its in-memory invite queue. Pending
scans behind an active scrape continue from the page, so keep it open until they
start. Active scrape jobs already run on the server. Discord verification,
account restrictions, and server screening can require user action outside
Searchcord; they are reported rather than bypassed.

## Scrape pause and pacing, 2026-10-04

- 66 Python tests passed, including nine new pause/pacing cases and the four
  pre-existing untracked retry tests. Checks use synthetic Discord responses
  and disposable SQLite archives.
- The full Chrome run passed 36 checks, covering existing server scans, invite
  queues, DM controls, in-flight page persistence, pause without further requests,
  reload recovery, exact resume pagination and remaining limits, pause/resume
  during a mandatory break, and layouts from 1440px down to 320px. No uncaught
  browser errors occurred. A focused run also verified that empty successful
  pages count toward the break.
- The mandatory break measured **60.000 seconds** between request starts in the
  full run and **60.016 seconds** in the focused run.
  The browser fixture seeds the success counter at 99 to exercise the real
  production 60-second wait. A separate fake-clock test runs all 100 successful
  responses, excludes failed responses, checks randomized intervals on retries,
  and verifies that different jobs share the count.
- Real-time concurrent-job tests confirm at least 0.5 seconds after a slow
  response finishes before another scrape request starts. Profile requests made
  by the scrape use the same gate. Pause/stop remain responsive during fetch
  retries, profile retries, and breaks; pausing does not reset a cooldown.
- Multiple progress subscribers each receive a snapshot and pause events, and
  disconnecting removes their subscriptions. Paused channels remain reserved
  against overlapping jobs. Python compilation, JavaScript syntax checks, and
  `git diff --check` passed.
- These new controls were not tested against a live Discord account. No running
  personal collector or production archive was changed or restarted.

To run only the scrape control browser checks against a fresh fixture:

```powershell
$env:SCRAPE_CONTROL_ONLY = '1'
node checks/browser_collection.mjs http://127.0.0.1:8016 agents/scrape-control-browser
Remove-Item Env:SCRAPE_CONTROL_ONLY
```

Restarting the server ends a paused in-memory job. Queue its channels again to
resume from the saved database cursors. Page reloads preserve the active job
while the server runs. Randomized intervals and the success counter are shared
across scrape jobs for that server session.
