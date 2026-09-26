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

- **Three workspaces**: Browse for local search, Scrape for channels, DMs,
  a shared collection queue and live monitoring, and Stats for archive exploration.
- **Queue and scrape** any number of channels at once, with an optional
  per-channel message cap, live progress, a stop button, and an optional
  expanded-profile fetch for message authors.
- **Resume and update** channel archives from saved message cursors, without
  re-reading completed history on later scrape jobs.
- **Live monitor** channels and watch new messages stream in as they arrive.
- **Search** everything you have collected, filtered by server, channel,
  author, and date range, with match highlighting and pagination. Author
  suggestions are fetched as you type instead of loading the whole author list.
- **Bauhaus home** with an integrated search and expanding filter tray, indexed
  archive totals, cool outline geometry, and compact previous/next pagination.
  Search URLs preserve filters and page selection across reloads.
- **Stats**: searchable, scrollable contributor rankings; server and channel
  charts; daily, monthly, weekday, and hourly activity. Select a contributor,
  server, channel, date, or month to search its messages. Accessible data tables
  accompany every chart. Timeline windows end at the latest archived message.
- **ChatML export** — turn a conversation into a `.jsonl` file in the
  OpenAI/ChatML message format.

Message text and image links are stored in one SQLite file at `data/searchcord.db`.
Image bytes are never downloaded into the database. Archive data stays local;
Chart.js is bundled locally, so charts work without a CDN connection.

---

## Requirements

- Python 3.9 or newer
- SQLite with the FTS5 trigram tokenizer (included in the tested Python build)
- A Discord token

## Installation

```bash
git clone https://github.com/k0shir0/searchcord.git
cd searchcord
pip install -r requirements.txt
```

## Running

```bash
python app.py
```

The app starts on <http://127.0.0.1:8000> and opens your browser when it is
ready. On Windows you can double-click `start.bat` instead. A first database
upgrade can take several minutes for a large archive; the console reports
progress every 30 seconds, and the port opens after the upgrade completes.

The three production workspaces share the Bauhaus design. Earlier standalone
drafts remain at `/trials/`, with synthetic data and local preview interactions.
See [the workspace change report](docs/workspace-redesign.md) and the
[original integration and draft report](docs/bauhaus-diff-report.md).

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

For collection, use the geometric settings button beside the wordmark, paste
your token, and hit **Save & Verify**. Open **Scrape** and use **connect Discord**
to load servers with an already saved token. Switch between **channels** and
**direct messages**, filter by name, and use the labeled queue/monitor/export
controls. Live monitoring and its incoming feed remain in the same workspace.

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
full history. Later jobs fetch only messages newer than each channel's saved
cursor. If a first run is limited or stopped, a later job can resume the
remaining older history. Check **fetch profiles** to request expanded Discord
user records for authors missing from the local profile table. The toggle is
off by default; known profiles are not fetched again. Searchcord stores the
documented public user fields needed for identity and appearance, including
username, display name, avatar/banner hashes, accent color, bot flag, and
public flags.

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
├── storage.py         # SQLite schema, backup, migration
├── requirements.txt
├── start.bat          # Windows launcher
├── static/
│   ├── index.html
│   ├── app.js         # Frontend logic
│   ├── stats.js       # Contributor pagination, charts and drilldowns
│   ├── workspace.css  # Production three-view layout
│   ├── vendor/        # Pinned Chart.js and its license
│   └── style.css      # Base controls, extended by the Bauhaus styles
└── data/              # Created at runtime — gitignored, never commit
    └── searchcord.db
```

---

## Data & privacy

`data/searchcord.db` contains **your Discord token in plaintext** in the
`settings` table, alongside every message you have scraped. A one-time schema
migration creates a recovery backup named
`data/searchcord.db.pre-compact-*.bak.gz` (or `.bak` if compression fails)
that also contains the token and archived messages. The backup stays in place
after verification; remove it only after checking the upgraded archive and
keeping any recovery copy you need. Decompress `.bak.gz` before opening it with
SQLite. The `data/`
directory is gitignored for that reason. Do not commit it, do not share it,
and delete it when you are done. **Clear All Data** in the settings panel
wipes archived messages and derived metadata; deleting the file removes everything including the
token.

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

The browser receives gzip-compressed API responses when supported. Message
text remains exact and searchable; per-message compression or truncation would
add decode work and break the current substring index. The measured
3.69-million-message legacy archive shrank from 1,664,700,416 to 843,780,096
bytes (49.31%) in the active database, while retaining historical names and
exact timestamp values. Its verified compressed recovery copy is 333,110,734
bytes. An archive already converted to v7 shrank from 1,065,906,176 to
777,445,376 bytes (27.06%); v7 had already discarded its historical names.
See the [storage audit](docs/storage-audit.md) for the full timeline, field
checks, and throughput measurements.

If you ever push this database anywhere by accident, treat your token as
compromised and reset it immediately by changing your Discord password.

## Notes

- Uses your user token against Discord's HTTP API. This is against Discord's
  Terms of Service and can get your account terminated. You accept that risk
  by running it.
- Requests share a process-wide gate, honor Discord's rate-limit headers and
  back off on HTTP 429, with a retry cap.
- Search uses a SQLite trigram index for substring queries of at least three
  characters; shorter queries still scan message content.
- Stats are maintained as small counts when messages are saved; they do not
  rescan the message table each time the stats view opens.
- See [storage audit](docs/storage-audit.md) for current storage measurements and
  [throughput audit](docs/throughput-audit.md) for earlier performance research.

## License

MIT, with a wrongful use warning — see [LICENSE](LICENSE).

## Release timeline

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
