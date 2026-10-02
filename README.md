<p align="center">
  <img src="static/brand/searchcord-logo.png" alt="SearchCord" width="660">
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
| Scrape | Queue channels or DMs, resume collection, monitor new messages and save profiles. |
| Stats | Explore activity and contributors, then open the matching messages. |
| Search only | Run the viewer with an existing archive, without Discord credentials or collection controls. |

## Get started

Use **Python 3.12 or newer** with SQLite's FTS5 trigram support.
Collection requires a Discord token. Searching an existing archive does not.
Read the [use warning in the license](LICENSE) before collecting data.

```bash
git clone https://github.com/k0shir0/searchcord.git
cd searchcord
python -m venv .venv
```

Activate the environment:

```bash
# Linux / macOS
source .venv/bin/activate
```

```bat
:: Windows Command Prompt
.venv\Scripts\activate
```

Install and start:

```bash
python -m pip install -r requirements.txt
python app.py
```

Open <http://127.0.0.1:8000>. Windows users can also run `start.bat`.
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
saves missing profile details during collection; **backfill saved profiles**
collects details for authors already in the archive. Both can be stopped.

Search results load one page at a time. Collection retries temporary failures
and keeps saved messages when stopped. Message IDs prevent duplicate records.

## Search only

Run the separate frontend in `static/search` with an existing SearchCord archive:

```bash
python search_app.py --db /path/to/archive.db
```

Open <http://127.0.0.1:8001>. You can also use
`--data-dir /path/to/folder` when the file is named `searchcord.db`.
Windows users can pass the same arguments to `start-search.bat`.
Without either option, the reader opens `data/searchcord.db`.

For a smaller file to deploy, export to a **new** destination:

```bash
python search_snapshot.py data/searchcord.db packed/searchcord.db
python search_app.py --data-dir packed --snapshot
```

The export preserves message text, names, timestamps, image links and saved
profiles. It excludes saved tokens and collection settings. After collecting
more data, export a new file and restart the viewer against it.
Use the original database for collection.

To prepare a standalone folder containing only the search service and its assets:

```bash
python deploy/search/package.py dist/searchcord-search
```

Copy that folder and your chosen database to the server. Follow the included
[deployment guide](deploy/search/README.md) for setup and container commands.
The viewer needs a Python server; a static host cannot run it on its own.

## Your data

- The collector stores messages, profiles and saved tokens in `data/searchcord.db`.
  Tokens are stored in plaintext. Keep the database and its backups private.
- To change the collector's location, set `SEARCHCORD_DATA_DIR` before launching.
  The search service also accepts `SEARCHCORD_DB` for a specific file.
  Relative paths resolve against the application folder.
- Both apps start on loopback and have no built-in login. Put access control and
  HTTPS in front of a hosted viewer. Share only data you are entitled to share.
- Images remain on Discord. The database stores links, not image files.
  The collector can refresh expired links when your token still has access;
  the search-only viewer cannot.
- An older archive upgrade keeps a recovery backup. Allow extra disk space and
  check the upgraded archive before removing a backup.
- **Clear All Data** removes archived messages and derived metadata after
  confirmation. Saved tokens remain.

## Release timeline

These are dated project updates. See [GitHub Releases](https://github.com/k0shir0/searchcord/releases)
for published release packages.

| Date | Update |
| --- | --- |
| 2026-10-02 | Faster Browse pagination and profile history queries, a standalone search package, refreshed setup guide and SearchCord logo. |
| 2026-09-30 | Verified compressed search snapshots, short-query indexes and support for stock SQLite exports. |
| 2026-09-29 | Save profiles during collection, retry temporary failures and stop profile backfill safely. |
| 2026-09-28 | More resilient scrape retries, resume-safe queues and duplicate checks. |
| 2026-09-25 | Smaller archive storage with verified backups and preserved message history. |
| 2026-09-24 | Unified Scrape workspace, contributor rankings and activity charts. |
| 2026-09-23 | Bauhaus search layout, expanding filters and resumable collection. |
| 2026-07-23 | Combined the scraper and viewer in one FastAPI app with SQLite. |
| 2025-09-20 | First Discord scraper and Flask viewer for JSON archives. |

## License

[MIT, with a wrongful use warning](LICENSE).
