<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="static/brand/searchcord-logo-dark.png">
    <img src="static/brand/searchcord-logo.png" alt="SearchCord" width="660">
  </picture>
</p>

<p align="center">Archive Discord messages. Search them locally. Share a separate, read-only viewer.</p>

<p align="center">
  <a href="#get-started">Get started</a> ·
  <a href="#search-only">Search only</a> ·
  <a href="#your-data">Your data</a> ·
  <a href="#release-timeline">Release timeline</a>
</p>

| Workspace | What you can do |
| --- | --- |
| Browse | Search message text, filter by server, channel, author or date, and open saved profiles. |
| Scrape | Join servers, scan readable channels, pause/resume collection, monitor messages and save profiles. |
| Stats | Explore activity and contributors, then open the matching messages. |
| Search only | Run the viewer with an existing archive, without Discord credentials or collection controls. |

## Get started

Searchcord runs as a native application on Windows, Linux and macOS.
Collection requires a Discord token. Searching an existing archive does not.
Read the [use warning in the license](LICENSE) before collecting data.

Build from source with Rust and a C compiler. Cargo uses the pinned toolchain
and dependency lockfile, including SQLite with FTS5 trigram support:

```bash
git clone https://github.com/k0shir0/searchcord.git
cd searchcord
cargo build --release --locked --workspace
./target/release/searchcord --data-dir "$PWD/data"
```

Open <http://127.0.0.1:8000>. On Windows, run `start.bat` after building.
A first upgrade of a large archive may take several minutes. The browser opens
when the database is ready.

The native application does not require Python. Windows launchers use the binary
beside the launcher, then `target\release`, and support paths with spaces. They
keep the archive in the launcher's `data` folder unless `SEARCHCORD_DATA_DIR`
is set. Direct binary launches resolve relative archive paths beside the binary;
use an absolute `--data-dir` or `--db` to select an existing archive explicitly.
The collector supports `SEARCHCORD_HOST`, `SEARCHCORD_PORT` and
`SEARCHCORD_NO_BROWSER=1`, or `--host`, `--port` and `--no-browser`.

Both startup commands show a white terminal banner with large mint Searchcord
lettering. Wider windows use a larger seven-row block font with clearer letter
shapes. In supported interactive terminals it stays at the top, while startup
and runtime logs scroll below it. The banner fits the terminal's starting width
and height; narrow or short windows use a smaller layout. Scrolling returns to
normal on exit. Redirected output, `TERM=dumb` and `NO_COLOR` use a plain ASCII
header without terminal controls. Server worker processes keep ordinary log
output so they do not redraw or reset their shared console. The collector logs
its save policy, request/retry intervals,
mandatory breaks and live polling interval once at startup. Saving happens after
each fetched page. Scrape requests wait a random 0.5–1.5 seconds after the previous
scrape response, with a random 60–90-second break after every 100 successful responses.

The collector opens the operating system's default browser after its health
check succeeds. No browser executable paths are required. If launching fails,
the app keeps running and prints its URL for manual opening.

## Use SearchCord

1. Open the settings gear, name your Discord token and select **save & use token**.
2. Open **Scrape**, select **connect Discord**, then choose channels or direct messages.
3. Add conversations to the queue and select **start scraping**. Leave the message
   limit blank for full history. Later runs resume or collect newer messages.
4. Open **Browse** to search. Choose filter suggestions or paste Discord IDs.
   Date filters use UTC and include the selected end date.

Click a message author to open their saved profile. **Fetch expanded profiles**
saves missing profile details during collection; **collect missing profiles**
and **refresh stale profiles** update authors already in the archive. Both can be stopped.

Scrape also accepts invite links or codes in a stoppable server-owned queue,
with join attempts at least 30 seconds apart. Double-click a server, press
**Shift+Enter**, or choose **scan & scrape** to queue readable channels and
accessible threads automatically. Automatic scans skip names containing `bot`;
those readable channels remain available for manual collection.

**Pause scraping** saves the current in-flight page before pausing. **Resume
scraping** continues the same job, channel, cursor and remaining message limit.
Reloading the page recovers the job. After a server restart, saved scrape and
invite queues and live monitors recover paused. Select their original saved
account and resume them when ready; no upstream action restarts automatically.
Request pacing and mandatory breaks are shared across scrape jobs and cannot
be bypassed by pause/resume or starting another job.

Search results load one page at a time. Collection retries temporary failures
and keeps saved messages when stopped. Re-fetched IDs update searchable content
and preserve observed revisions; confirmed deletions retain an archive tombstone.

Browse uses message-ID cursors to avoid counting every matching message. It shows
the current page and whether more results are available, rather than an exact
match total. Existing counted page links still work. Reloaded or shared cursor
links preserve their page; when earlier cursors are unavailable, **first page**
returns to the beginning. Queries accept up to 200 characters.

## Search only

Run the separate frontend in `static/search` with an existing SearchCord archive:

```bash
./target/release/searchcord-search --db /path/to/archive.db
```

Open <http://127.0.0.1:8001>. You can also use
`--data-dir /path/to/folder` when the file is named `searchcord.db`.
Windows users can pass the same arguments to `start-search.bat`.
Without either option, the reader opens `data/searchcord.db`.

For a smaller file to deploy, export to a **new** destination:

```bash
./target/release/searchcord-snapshot "$PWD/data/searchcord.db" "$PWD/packed/searchcord.db"
./target/release/searchcord-search --data-dir "$PWD/packed" --snapshot
```

The export preserves message text, names, timestamps, image links and saved
profiles. It excludes saved tokens and collection settings. After collecting
more data, export a new file and restart the viewer against it.
Use the original database for collection.

To prepare a standalone folder containing only the search service and its assets:

```bash
./target/release/searchcord-package-search dist/searchcord-search
```

Copy that folder and your chosen database to a server running the same operating
system and architecture. Run `./searchcord-search --db /path/to/archive.db` there.
The folder contains the native viewer, approved assets, licenses and a Dockerfile.
It contains no collector, Python runtime, credentials or archive. A static host
cannot run the service on its own.

For the included non-root Linux image, first build a static Linux viewer and
package it on Linux:

```bash
RUSTFLAGS='-C target-feature=+crt-static' cargo build --release --locked --target x86_64-unknown-linux-gnu -p searchcord-viewer
./target/x86_64-unknown-linux-gnu/release/searchcord-package-search dist/searchcord-search
docker build -t searchcord-search dist/searchcord-search
docker run --rm -p 8001:8001 -v /absolute/archive-folder:/app/data:ro searchcord-search
```

The mounted folder must contain `searchcord.db`. Use `--snapshot` only for an
offline copy with no active WAL. Omit it when reading an archive being collected.

## Upgrade and rollback

Stop the previous collector before starting the native application. Keep a copy
of the archive folder, including `media`, and the separate credential vault and
portable key before upgrading. Windows DPAPI vaults remain tied to the same user.
Point the native collector at the original standard archive. Collection schema
v10, read schemas v9/v10, packed schema v101 and saved job checkpoints are retained.
Packed snapshots remain read-only and must not replace the collection archive.

Recovered scrapes, monitors and invite queues stay paused. Select the original
account before resuming. Interrupted invite joins remain uncertain; check Discord
before retrying them. DM cleanup does not restart automatically.

To roll back, stop the native collector, restore the saved archive and vault
copies, and run the previous application from a separate folder. The Python
baseline is available at revision `481f62c6d1e111ca6f5771554a0e89ecd42b088e`;
it requires Python 3.12 and its `requirements.txt`. Never run two collectors on
the same database. An older-schema upgrade also creates a verified recovery
backup beside the archive; retain it until the upgraded data has been checked.

## Your data

- The collector stores messages, profiles and credential metadata in `data/searchcord.db`.
  Saved credentials live in a separate encrypted vault outside the archive
  directory. Windows protects it with the current user's DPAPI identity; other
  systems use a separate private encryption key. Keep archives and backups private.
- To change the collector's location, set `SEARCHCORD_DATA_DIR` before launching.
  The search service also accepts `SEARCHCORD_DB` for a specific file.
  Relative paths resolve against the application folder.
- Both apps start on loopback and have no built-in login. Put access control and
  HTTPS in front of a hosted viewer. Share only data you are entitled to share.
- New collection retains full received message JSON and attachment metadata.
  The collector also saves bounded local copies of attachments, avatars and
  banners beside the database in `media/`. Missing or oversized downloads are
  recorded explicitly. Export with `--with-media` to preserve available binaries
  in the viewer; refresh inaccessible remote links through the collector.
- For smaller media, set `SEARCHCORD_MEDIA_MODE=compact` for lossy WebP copies,
  or `none` to stop new media downloads. Set `SEARCHCORD_PROFILE_IMAGES=shapes`
  to keep geometric message avatars and skip profile pictures/banners.
- An older archive upgrade keeps a recovery backup. Allow extra disk space and
  check the upgraded archive before removing a backup.
- **Clear All Data** removes archived messages and derived metadata after
  confirmation. Stop active and paused scrapes, live monitors and DM cleanup
  first. Profile jobs stop during clearing; saved credentials remain.
- **remove all saved tokens** removes the sealed credential vault and saved
  credential settings after a second-click confirmation. Stop scrapes, DM
  cleanup and live monitors first; invite joining and profile backfill stop
  during removal. Messages and unrelated settings stay.
  Save a new token to connect again. Closing Settings cancels confirmation.

Old databases, recovery backups and SQLite free/WAL pages may still contain
credentials saved before this upgrade. They are not securely erased by migration
or token removal. Keep those copies private and rotate retired credentials.

Recovery backups are named `searchcord.db.pre-compact-*.bak.gz` (or `.bak` if
compression fails). Newly generated recovery copies remove credential settings
and retain private messages; older copies may contain plaintext tokens.
Migration checks SQLite and full-text integrity. Fields already
discarded by an older migration can only be recovered from an original archive
or backup. Test upgrades on a separate copy first.

## Release timeline

Major development milestones in the current `main` history. Dates use
America/Chicago and reflect commits, not deployment dates. As of 2026-10-09,
no versioned [GitHub Releases](https://github.com/k0shir0/searchcord/releases)
have been published.

| Date | Update |
| --- | --- |
| 2026-11-01 | Native collector, search viewer and archive tools, using existing archives and the same browser interface. |
| 2026-10-09 | Durable paused job recovery, observed message revisions, local attachments and protected saved credentials. |
| 2026-10-09 | Invite queues, readable server/thread scans and reload-recoverable scrape pause/resume, with enforced request pacing and breaks. |
| 2026-10-07 | Portable Windows startup, terminal banner and saved-token removal. |
| 2026-10-02 | Faster cursor-based search and standalone viewer packaging with Docker support. |
| 2026-09-30 | Compressed search archives that exclude saved tokens. |
| 2026-09-29 | Immediate profile collection while scraping and stoppable, retryable profile backfill. |
| 2026-09-28 | Separate read-only viewer, archived profile cards and more reliable scraping. |
| 2026-09-26 | Multiple saved accounts, confirmed archive cleanup and expanded server/contributor rankings. |
| 2026-09-25 | Reduced archive storage and compressed recovery backups. |
| 2026-09-24 | Unified scraping tools and activity charts. |
| 2026-09-23 | New search layout, filters, resumable scraping and DM message cleanup. |
| 2026-07-23 | Unified web app with live monitoring and ChatML export. |
| 2025-09-20 | Initial scraper and viewer. |

## License

[MIT, with a wrongful use warning](LICENSE).
