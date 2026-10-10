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

Scanning during an active collection adds pending sources and starts them after
the current job succeeds. Stopping, clearing the queue or an error suspends that
automatic continuation and preserves remaining work. Keep the page open until
pending scans start; the active scrape itself runs on the server. Changing
accounts discards scans still in flight.

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
