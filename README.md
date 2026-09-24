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

- **Browse** your servers and channels, or your DMs and group DMs.
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
- **Stats** — totals, top senders, messages per server, activity over the
  last 30 days, and a by-hour histogram.
- **ChatML export** — turn a conversation into a `.jsonl` file in the
  OpenAI/ChatML message format.

Message text and image links are stored in one SQLite file at `data/searchcord.db`.
Image bytes are never downloaded into the database. Archive data stays local;
the UI also loads Chart.js from a CDN for its charts.

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

The app starts on <http://127.0.0.1:8000> and opens your browser. On Windows
you can double-click `start.bat` instead.

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

For collection, open **settings**, paste your token, and hit **Save & Verify**.
Expand **Archive channels** under Browse to choose servers and channels. Use
**connect Discord** to load servers with an already saved token. DMs, Live, and
Stats remain available from the view tabs.

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

To scrape: click channels to add them to the queue, optionally set a
"msgs back" limit, then **start scraping**. Leave the limit blank to pull the
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
│   └── style.css
└── data/              # Created at runtime — gitignored, never commit
    └── searchcord.db
```

---

## Data & privacy

`data/searchcord.db` contains **your Discord token in plaintext** in the
`settings` table, alongside every message you have scraped. A one-time schema
migration temporarily creates a `data/searchcord.db.pre-compact-*.bak` backup
that also contains the token and archived messages. After the compact database
passes SQLite integrity checking, the app removes that backup. If migration or
backup removal fails, the backup remains for recovery. The `data/`
directory is gitignored for that reason. Do not commit it, do not share it,
and delete it when you are done. **Clear All Data** in the settings panel
wipes archived messages and derived metadata; deleting the file removes everything including the
token.

The first start after upgrading makes the backup, converts message IDs and
related IDs to compact integers, removes repeated per-message names and text
timestamps, keeps image attachment URLs only, and builds the search index and
cached stats. Message IDs encode the timestamp used by search and charts. A
3.69-million-message archive took about 3 minutes to migrate on the tested
machine. Allow several gigabytes of temporary disk headroom while the original,
backup, migration journal, and compact file coexist. Later starts skip migration.
If a backup remains after a successful startup, check the startup message and
remove it only after confirming that the compact archive works for you.

Search results and the live feed send short local image links. Opening an image
redirects to its stored Discord URL; if its signature has expired, Searchcord
fetches that message once to get a fresh link. Refresh requires a valid token
and access to the original message. Discord's [signed attachment URLs](https://github.com/discord/discord-api-docs/blob/main/developers/reference.mdx#signed-attachment-cdn-urls)
expire, so link-only storage cannot preserve an image after its message becomes
unavailable. Non-image attachments are not stored in new archives.

The browser receives gzip-compressed API responses when supported. Message
text remains exact and searchable; per-message compression or truncation would
add decode work and break the current substring index. The measured compact
archive is 1,065,906,176 bytes, down 35.97% from 1,664,700,416 bytes. See the
[throughput audit](docs/throughput-audit.md) for the full measurement.

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
- See [throughput audit](docs/throughput-audit.md) for the measured write and
  storage trade-offs, DB engine decision, and scale limits.

## License

MIT, with a wrongful use warning — see [LICENSE](LICENSE).
