# Searchcord

A local, self-hosted tool for archiving and searching Discord messages. Runs
entirely on your machine as a single FastAPI app with a browser UI — scrape
channels into SQLite, then search, filter, and chart what you collected.

> **Read the [wrongful use warning](LICENSE) before running this.** This tool
> archives other people's private conversations, automating a user account
> against Discord's API violates their Terms of Service, and the risk is
> entirely yours. Use it on your own history or with the consent of the people
> involved.

---

## Features

- **Archived profiles**: click a result author for their saved Discord-style card,
  avatar, bio, connections and searchable server history. Fetch one extended
  profile from the card, or use **backfill saved profiles** near the top of Scrape to backfill
  existing authors with explicit start/stop controls. Rate limits and temporary
  failures wait and retry the same user; unavailable users are skipped. See
  [profiles](docs/profiles.md).

- **Three workspaces**: Browse for local search, Scrape for channels, DMs,
  a shared collection queue and live monitoring, and Stats for archive exploration.
- **Queue and scrape** any number of channels at once, with an optional
  per-channel message cap, live per-channel counts and stopping in the queue, and an optional
  expanded-profile fetch for message authors.
- **Join servers** from a single invite or a list of links/codes in Scrape.
  Duplicate codes are removed, join attempts are at least 30 seconds apart,
  and the server keeps the stoppable queue running through page navigation or reloads.
- **Scan and scrape** a server by double-clicking it, using Shift+Enter, or
  selecting **scan & scrape**. Only channels with readable message history
  are listed. Scans add readable channels and accessible active/archived threads
  to the existing queue, skip names containing `bot` regardless of case, and
  start automatically with your existing limit and profile choice.
- **Resume and update** channel archives from saved message cursors, without
  re-reading completed history on later scrape jobs.
- **Live monitor** channels and watch new messages stream in as they arrive.
- **Search** everything you have collected, filtered by server, channel,
  author, and date range, with match highlighting and pagination. Author
  suggestions are fetched as you type instead of loading the whole author list.
- **Bauhaus home** with an integrated search and expanding filter tray, indexed
  archive totals, cool outline geometry, and compact previous/next pagination.
  Search URLs preserve filters and page selection across reloads.
- **Stats**: one scrollable view, starting with searchable server and contributor
  leaderboards, then archive totals, server and channel charts, and daily,
  monthly, weekday, and hourly activity. Select a contributor, server, channel,
  date, or month to search its messages. Accessible data tables stay visible
  beneath every chart. Timeline windows end at the latest archived message.
- **ChatML export** — turn a conversation into a `.jsonl` file in the
  OpenAI/ChatML message format.

Message text and image links are stored in one SQLite file at `data/searchcord.db`.
Image bytes are never downloaded into the database. Archive data stays local;
Chart.js is bundled locally, so charts work without a CDN connection.

---

## Requirements

- Python 3.9 or newer
- SQLite with the FTS5 trigram tokenizer (included in the tested Python build)
- A Discord token for collection; search-only display needs no token

## Installation

```bash
git clone https://github.com/k0shir0/searchcord.git
cd searchcord
pip install -r requirements.txt
```

## Running

### Search-only display

Run the separate frontend against an existing schema v9 archive:

```bash
python search_app.py --data-dir /path/to/archive-directory
```

Open <http://127.0.0.1:8001>. `start-search.bat` accepts the same arguments on
Windows. The frontend keeps the existing search, filters, profiles and theme.
Results load 50 at a time with public message-ID cursors, literal highlighting,
full text, and inclusive UTC dates. Saved profiles are read locally; profile
collection stays in the main application.

For a smaller deployment file, export the collector archive into a **new** directory:

```bash
python search_snapshot.py /path/to/collector/searchcord.db packed/searchcord.db --report packed/build-report.json
python search_app.py --data-dir packed --snapshot
```

This schema v101 search snapshot preserves message text, IDs, historical names,
exact timestamps, image links and saved profiles. It compresses blocks of 128
messages and keeps separate substring indexes, including short queries. The
verified 5,574,147-message archive uses **118.31 bytes/message instead of 260.03**,
a **54.5% reduction**. Settings, tokens and collection cursors are omitted.
Existing output files and reports are never overwritten. The exporter opens the
source read-only and pins a coherent read transaction, including pending WAL data.

SQLite's optional `dbstat` extension supplies the per-table storage breakdown.
If unavailable, the exporter still measures total bytes and verifies all data;
the report's `source_pages` and `snapshot_pages` fields are `null`.

The packed file stays unchanged while being served. Export a new snapshot after
collecting more messages or profiles, then restart the display app with its new
directory. Use the original schema v9 file for the collection app.

Normal display mode also supports a schema v9 archive being updated by a collector.
`--snapshot` enables SQLite immutable mode for a closed, unchanging archive and
rejects a nonempty WAL. Neither display mode runs migrations, reads saved tokens,
or exposes collection, settings, deletion or export routes. The service binds to
loopback; API responses use `no-store`. Valid saved avatars load lazily from
Discord, and saved image links open their original URLs without credential-based
refresh. Those URLs may have expired.

See [search storage and latency measurements](SEARCH-ONLY-SCALE-RESULTS.md) for
the storage breakdown, first-request costs, HTTP and browser checks, and repeatable
commands.

### Collection and archive administration

```bash
python app.py
```

The app starts on <http://127.0.0.1:8000> and opens your browser when it is
ready. On Windows you can double-click `start.bat` instead. A first database
upgrade can take several minutes for a large archive; the console reports
progress every 30 seconds, and the port opens after the upgrade completes.
If the port is already in use, this launch reports an error instead of opening
another Searchcord instance.

It binds to loopback only. There is **no authentication** — anyone who can
reach the port gets your token and your entire archive — so only change the
host if you understand that:

```bash
SEARCHCORD_HOST=0.0.0.0 SEARCHCORD_PORT=8000 python app.py
```

## First run

Search your local archive straight from the home page, without connecting to
Discord. Select server, channel, and author suggestions or paste their IDs.
Date filters use UTC and include the entire selected end date. Clear filters
keeps the text query. Queries and filter IDs appear in the browser URL.

For collection, use the outline gear beside the wordmark, give each Discord
token a name, and choose **save & use token**. The saved-token menu lets you
switch accounts without re-entering credentials. Open **Scrape** and use
**connect Discord** to load servers with the selected token. Switch between **channels** and
**direct messages**, filter by name, and use the labeled queue/monitor/export
controls. Live monitoring and its incoming feed remain in the same workspace.

**Join servers** accepts `discord.gg/code`, `https://discord.com/invite/code`,
legacy `discordapp.com/invite/code` links, and bare invite codes. Separate a list
with spaces, commas, semicolons, or new lines. The account selected when the
queue starts is used for the whole queue. Each attempt is at least 30 seconds
after the previous one; Discord rate limits can extend that wait and retry the
same invite. Invalid/expired invites are shown individually. Successful joins
refresh the server list. **Stop joining** cancels pending work; completed joins
remain. A request already sent to Discord may have completed before a stop,
so check Discord before retrying a stopped request. Navigation and reloads
recover progress; restarting the application stops the in-memory invite queue.
Account verification, an invalid token, or server screening can require action
in Discord. Searchcord reports these states and does not solve or bypass verification.
An account restriction on joining servers stops the whole invite queue with an
Account Standing notice; refreshing a token does not remove that restriction.

Single-click a server to browse its channels. Searchcord checks the selected
member's roles and channel overwrites for both `VIEW_CHANNEL` and
`READ_MESSAGE_HISTORY`, then probes one message without saving it. An empty
readable channel stays in the list; a view-only or denied channel does not.
This follows [Discord's permission precedence](https://docs.discord.com/developers/topics/permissions).
Scans also discover active threads and archived public/private threads accessible
to the account, including forum and media posts. Ordinary browsing checks current
channels without walking thread archives. Thread access inherits parent
permissions and is checked before listing; archive pages are paginated.
Temporary access-check failures show an error so a partial list does not silently
become a complete automatic scrape.

Double-click a server to scan and start collection without selecting individual
channels. Keyboard users can press **Shift+Enter** on a server; **scan & scrape**
provides the same action for touch. A scan ignores the current name filter,
adds each channel once, and keeps manually queued sources. Names containing
`bot` are excluded from automatic additions; readable bot channels remain
available for manual queuing. Another scan during an active job waits in the
queue and starts after that job finishes successfully. Stopping or an error
preserves the queue and pauses automatic continuation. Keep the page open for
continuation of scans queued behind an active job; the active scrape itself
already runs on the server. Changing accounts discards scans still in flight.

### Data directory

The default database is `data/searchcord.db`, resolved relative to the application
directory, independently of the working directory. To use another archive, set
`SEARCHCORD_DATA_DIR` to its directory before starting the server. Relative paths
are resolved against the application directory. No private machine path is
embedded in the application. For example, in PowerShell:

```powershell
$env:SEARCHCORD_DATA_DIR = 'data/my-archive'
python app.py
```

Opening an older archive runs the storage migration described below. For testing
an existing archive, use a separate copy first. Keep database directories and
exports out of version control.

To scrape: use **queue** beside a channel or conversation, optionally set a
**Messages per channel** limit, then **start scraping**. Leave the limit blank to pull the
full history. The queue shows the current channel, messages saved per channel,
the total, and a stop button while the job runs. You can keep using the rest of
the workspace without closing a progress dialog. Later jobs fetch only messages
newer than each channel's saved cursor. If a first run is limited or stopped, a
later job can resume the remaining older history. Check **fetch expanded
profiles** to save each newly encountered user's extended profile immediately
after its message page is committed, before requesting another page. The toggle
is off by default; saved extended profiles are not fetched again, and webhooks
are skipped. Earlier missing profiles can be collected with **backfill saved
profiles**. Profile waits show their retry/cooldown and can be stopped without
losing saved work. Searchcord stores the
documented public user fields needed for identity and appearance, including
username, display name, avatar/banner hashes, accent color, bot flag, and
public flags.

Temporary Discord/network failures and SQLite busy errors pause the scrape and
retry the same page with a 2–60 second backoff (longer when Discord asks for it).
The progress panel shows the retry, and **stop scraping** still works during the
wait. The terminal logs the job, channel ID, cursor, retry count, and failure
type without printing tokens or message contents. Permanent access errors are
shown for that channel; saved cursors and the queue remain available to resume.

Message IDs are unique in the archive. If Discord returns an overlapping page,
existing IDs are ignored by SQLite; only newly inserted messages increase the
scrape count, archive totals, and search index. An extra database lookup or
message cache is not needed for this check.

Archives created before the cursor migration resume older history from their
oldest stored message. The old schema did not record whether a scrape finished,
so the next job makes one older-history request even for an archive that was
already complete. A stopped or capped job also retains a pending gap of new
messages so the next job can fill it without walking completed history.

---

## Project structure

```
searchcord/
├── app.py             # FastAPI backend — API, scraper, live poller, export
├── channel_access.py  # Readable-channel checks and scan thread discovery
├── invite_api.py      # Server-owned invite queue and progress
├── storage.py         # SQLite schema, backup, migration
├── search_app.py      # Separate read-only search frontend
├── search_snapshot.py # Lossless packed snapshot exporter and reader
├── requirements.txt
├── start.bat          # Windows launcher
├── static/
│   ├── index.html
│   ├── app.js         # Frontend logic
│   ├── collection.js  # Invite controls and automatic server scans
│   ├── stats.js       # Contributor pagination, charts and drilldowns
│   ├── workspace.css  # Production three-view layout
│   ├── vendor/        # Pinned Chart.js and its license
│   └── style.css      # Base controls, extended by the Bauhaus styles
└── data/              # Created at runtime — gitignored, never commit
    └── searchcord.db
```

---

## Data & privacy

`data/searchcord.db` contains **your saved Discord tokens in plaintext** in the
`settings` table, alongside every message you have scraped. Only token names
and IDs are returned to the browser for the saved-token menu; new tokens are
sent to the local app when you save them. A one-time schema
migration creates a recovery backup named
`data/searchcord.db.pre-compact-*.bak.gz` (or `.bak` if compression fails)
that also contains the token and archived messages. The backup stays in place
after verification; remove it only after checking the upgraded archive and
keeping any recovery copy you need. Decompress `.bak.gz` before opening it with
SQLite. The `data/`
directory is gitignored for that reason. Do not commit it, do not share it,
and delete it when you are done. **Clear All Data** asks for a second click
inside the button before it wipes archived messages and derived metadata.
Saved tokens remain; deleting the database file removes them too.

The search-only frontend owns the **privacy & data** page and footer link.
The page currently contains only its title and a
return link to the Searchcord home page that also works when opened as a local
file. This README holds the current data-handling details; the page is
not a completed privacy policy.

The first start after upgrading makes the backup and assigns small internal row
numbers to reduce search-index storage. Discord message IDs stay unchanged and
still order results correctly when older history is collected later. Message
text, IDs, and historical per-message names are checked before the old table is
replaced. Original timestamps are reproduced exactly from message IDs where
possible; exceptions remain stored. Older raw attachment metadata is removed
from the active database, while image links used by the app remain available.
SQLite and full-text-index integrity checks run after migration. Allow several
gigabytes of temporary disk headroom while the original, backup, migration
journal, and compact file coexist. Later starts skip migration. Historical
names or attachment fields already discarded by an earlier compact migration
can only be recovered from an original archive or older backup.

Search results and the live feed send short local image links. Opening an image
redirects to its stored Discord URL; if its signature has expired, Searchcord
fetches that message once to get a fresh link. Refresh requires a valid token
and access to the original message. Discord's [signed attachment URLs](https://github.com/discord/discord-api-docs/blob/main/developers/reference.mdx#signed-attachment-cdn-urls)
expire, so link-only storage cannot preserve an image after its message becomes
unavailable. Non-image attachments are not stored in new archives.

The search snapshot contains the preserved message data and saved profiles, so
it remains private archive data even though it excludes saved tokens. It is a
separate deployment file; it does not replace or migrate the collection archive.
API responses use gzip when supported. Message blocks are compressed at rest,
and index candidates are checked against full decoded content with the original
literal substring rules. The server retains a bounded cache of 64 decoded blocks
across requests, plus small per-connection caches.

If you ever push this database anywhere by accident, treat your token as
compromised and reset it immediately by changing your Discord password.

## License

MIT, with a wrongful use warning — see [LICENSE](LICENSE).

## Release timeline

### 2026-10-01

- Added a stoppable invite queue with 30-second spacing and reload recovery.
- Filter collection sources by readable history, including accessible threads.
- Added double-click server scanning and automatic queue continuation, with
  case-insensitive `bot` exclusions and existing scrape options preserved.

### 2026-09-29

- Added verified, compressed search-only snapshots and short-query indexing,
  reducing the measured archive to 118.31 bytes per message.
- Save missing extended profiles as each message page is collected, retry the
  same user through temporary failures, and keep backfill stoppable during
  requests and Discord cooldowns.

### 2026-09-25

- Reduced full-archive storage by packing the search index, reconstructing exact
  timestamps, compressing verified recovery backups, and removing unused raw
  attachment metadata while retaining historical per-message names, IDs, text,
  and image links.

### 2026-09-24

- Unified channels, DMs, the collection queue, and live monitoring in the new
  Bauhaus Scrape workspace.
- Added searchable contributor rankings and six interactive, offline-ready
  charts with search drilldowns and accessible data tables.

### 2026-09-23

- Promoted the Bauhaus home design with integrated search, expanding filters,
  real archive totals, and shareable search URLs.
- Added resumable incremental collection, optional profile harvesting, indexed
  substring search, and cached statistics for large archives.

### 2026-07-23

- Replaced the separate scraper and search scripts with one self-hosted FastAPI
  application and SQLite archive, including live monitoring and ChatML export.

### 2025-09-20

- Introduced the Discord scraper and Flask search interface for per-channel
  JSON datasets, message search, and paginated conversation results.
