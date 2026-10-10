# Collection controls

The collector's **Scrape** workspace owns source selection, the collection queue,
invite joining and live monitoring. An existing saved archive remains searchable
without connecting Discord.

## Join servers

The invite field accepts `discord.gg/code`, `discord.com/invite/code`, legacy
`discordapp.com/invite/code` links and bare invite codes. Separate multiple items
with spaces, commas, semicolons or newlines. Duplicate codes are removed, and a
queue accepts up to 200 unique invites.

The account selected when the queue starts is used throughout that queue.
Attempts are at least 30 seconds apart, including failed attempts and retries.
Discord rate limits can extend the wait. Invalid or expired invites are reported
individually; successful joins refresh the server list. Account restrictions,
verification and server screening are reported for action in Discord.

**Stop joining** cancels pending work. Completed joins remain, and a request
already sent may have completed before cancellation. Check Discord before
retrying an interrupted request. Progress survives navigation and reloads while
the server runs. Restarting the server restores saved invite queues paused;
select the original account and explicitly resume joining. An interrupted request
may have joined before its checkpoint was saved; check Discord before retrying.
Removing all saved tokens stops an active invite queue before deleting credentials.

## Browse readable channels or scan a server

Single-click a server to browse channels. Searchcord checks account permissions
for both viewing the channel and reading message history, then probes access.
An empty readable channel remains available. View-only, denied and inaccessible
private channels are omitted. Thread access inherits parent permissions.

Double-click a server, press **Shift+Enter** on its row, or choose **scan & scrape**
to include accessible active and archived threads, including forum/media posts.
Ordinary browsing does not walk thread archives. Temporary access-check failures
produce an error instead of starting a scrape from a silently incomplete list.

A scan ignores the visible name filter, retains manually queued sources and adds
each channel once. Automatic additions skip channel names containing `bot`,
without regard to case. Readable bot channels remain manually selectable.
Collection uses the existing per-channel limit and expanded-profile choice.

Scanning during an active scrape adds pending sources. The active batch keeps
its original source list, progress rows, cursor and remaining limit. When it
succeeds, only its sources leave the visible collection queue; accepted pending
sources then start automatically. Multiple server scans can add sources.
Repeated activation of a server already being scanned does not start a duplicate
scan.

Stopping or a scrape error preserves queued work and suspends automatic
continuation, including scans already in flight. Clearing an idle queue removes
its current sources. A scan that finishes after a stop or clear can still add
sources, but cannot restart collection automatically. Choose **start scraping**
to run the retained queue, or explicitly scan again to request collection.

Changing the saved account immediately discards browser-owned pending sources
and invalidates old scan results, even while token verification is still running.
An already-requested scrape start finishes binding to its original saved account
before the account setting changes. Rapid account changes run in request order;
starts stay blocked until the latest change finishes. An active scrape keeps its
original account and its batch visible for pause/resume/stop. Scans under the
newly selected account can add new pending sources. Removing every saved token
also clears the visible sources after collection jobs have stopped. If removal
fails, credentials and selected sources are kept. DM cleanup blocks scrape starts;
scans accepted during cleanup can start when it finishes, including when cleanup
reports an error, unless collection was stopped or cleared in the meantime.

Keep the page open until pending scans start. Pending sources and scans live in
the browser; the active scrape runs and recovers on the server. Reload restores
the server-owned batch and progress, but does not retain browser pending work.
New starts remain blocked until the server's active-job snapshot is recovered.
If reconnection fails, reload to retry before starting another scrape.

### Browser queue ownership

`static/collection-queue.js` owns source acceptance, the immutable active batch,
scan generations and continuation policy. Its interface accepts queue actions,
scan loaders, account changes and server snapshot recovery. `view()` supplies
read-only sources, pending sources, active batch and allowed controls. The module
reserves a batch synchronously before its start adapter makes an HTTP request.
The adapter reports each terminal scrape outcome through `finish(success)`;
successful outcomes remove that batch, while unsuccessful outcomes preserve work
and invalidate automatic starts from older scans.

DOM rendering, HTTP requests and EventSource progress remain adapters in
`static/app.js` and `static/collection.js`. This seam gives callers leverage
without requiring them to coordinate shared policy flags. The module's depth
comes from keeping acceptance and event ordering in one implementation, providing
locality for delayed-scan and completion rules. It is not a persistent browser
job system; saved execution, account requirements and exact cursor recovery stay
server-owned.

## Pause, resume and stop

**Pause scraping** finishes the current request and commits its message page and
cursor before blocking further requests. **Resume scraping** continues the same
channel, pagination position, remaining per-channel limit and queued sources.
Paused channels stay reserved against overlapping scrape jobs.

Reloading the page reconnects to an active or paused job, including its saved
progress and options, while the server remains running. **Stop scraping** works
while paused, during retry waits and during mandatory breaks. Stopping preserves
committed messages and durable channel cursors. After restarting the server,
saved scrape jobs recover paused with their queue, cursor and remaining limits.
Select the original account and resume. Live monitors also recover paused.

Expanded profiles are collected after their message page is committed and before
the next page. An in-flight profile request finishes its save before a pause
takes effect. Pause/stop also remain usable during profile retries and cooldowns.

## Request pacing and recovery

Scrape requests, their retries and expanded-profile requests made by a scrape
wait a random **0.5–1 second** after the previous scrape request finishes.
After every **100 successful Discord responses**, further scrape requests wait
at least **60 seconds** from the last response. Empty successful pages count;
HTTP errors do not. The counter and deadlines are shared across scrape jobs and
persisted for restart recovery, so changing channels, pause/resume and stop/start
do not reset them. Discord cooldowns and retry backoff can extend the wait.

The progress panel shows mandatory breaks. A paused scrape does not hold the
shared Discord request gate, so unrelated account/source operations can continue.
The separate invite queue retains its own 30-second joining interval.

Temporary Discord/network failures and SQLite busy errors retry the same
operation with a 2–60 second backoff, honoring longer upstream cooldowns. A save
retry finishes before pausing so a fetched page is not discarded. Permanent
access errors are shown for the affected channel. Message IDs prevent duplicate
records when pages overlap or a collection is replayed.

Live monitoring remains a separate polling workflow. These scrape pause and
mandatory-break controls do not change its three-second polling interval.
