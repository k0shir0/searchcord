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

Major milestones by the date they reached `main` (America/Chicago).

| Date | Update |
| --- | --- |
| 2026-11-05 | The collector, search viewer and archive tools moved to Rust. Existing archives and the browser interface carry forward. Live monitoring now recovers paused after a restart. [#27](https://github.com/k0shir0/searchcord/pull/27) |
| 2026-11-05 | Added compact image storage, geometric avatars and tools to shrink an existing media cache. [#8](https://github.com/k0shir0/searchcord/pull/8) |
| 2026-10-11 | Collection queues stay tied to the selected account, and scrape progress reflects messages saved to the archive. [#4](https://github.com/k0shir0/searchcord/pull/4) [#5](https://github.com/k0shir0/searchcord/pull/5) |
| 2026-10-09 | Added invite queues, automatic channel and thread discovery, and pause and resume across reloads or restarts. The archive keeps observed edits and deletions, caches media locally and protects saved tokens in an encrypted vault. [Collection](https://github.com/k0shir0/searchcord/commit/9b497d1) [Archive](https://github.com/k0shir0/searchcord/commit/1c9363c) |
| 2026-10-07 | Added portable Windows launchers and a way to remove all saved tokens. [#2](https://github.com/k0shir0/searchcord/pull/2) |
| 2026-10-02 | Search pages load faster on large archives. The read-only viewer can run as a standalone Docker service. [#1](https://github.com/k0shir0/searchcord/pull/1) |
| 2026-09-30 | Added compact search snapshots that can be shared without saved credentials. [Commit](https://github.com/k0shir0/searchcord/commit/e5a7935) |
| 2026-09-28 | Added a separate read-only viewer, saved profile cards and collection of missing profile details. [Viewer](https://github.com/k0shir0/searchcord/commit/844f0cb) [Profiles](https://github.com/k0shir0/searchcord/commit/cf4dea7) |
| 2026-09-26 | Added multiple saved Discord accounts, archive cleanup with confirmation, and server and contributor rankings. [Accounts](https://github.com/k0shir0/searchcord/commit/c32e670) [Rankings](https://github.com/k0shir0/searchcord/commit/4db997b) |
| 2026-09-25 | Reduced archive disk usage and added compressed recovery backups. [Commit](https://github.com/k0shir0/searchcord/commit/bb9f4bd) |
| 2026-09-24 | Brought collection tools into one workspace and expanded interactive activity charts. [Commit](https://github.com/k0shir0/searchcord/commit/2695a19) |
| 2026-09-23 | Introduced the current search layout and filters. [Commit](https://github.com/k0shir0/searchcord/commit/7e89725) |
| 2026-07-23 | Replaced the original scripts with a web app for search, collection, live monitoring, DM cleanup and ChatML export. [Commit](https://github.com/k0shir0/searchcord/commit/80df94c) |
| 2025-09-20 | First Discord scraper and archive search scripts. [Scraper](https://github.com/k0shir0/searchcord/commit/2c4f19a) [Search](https://github.com/k0shir0/searchcord/commit/4aab66b) |

## License

[MIT, with a wrongful use warning](LICENSE).
