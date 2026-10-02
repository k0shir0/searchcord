# Search-only deployment

This package serves the frontend in `static/search` and reads one SearchCord
archive. It includes no collector, Discord login, settings or deletion routes.
A Python server is required. Uploading the HTML and a database to a static host
will not make the database searchable.

## Run an existing archive

Use Python 3.12 or newer. From the package root:

```bash
python -m venv .venv
# Windows: .venv\Scripts\activate
# Linux / macOS: source .venv/bin/activate
python -m pip install -r deploy/search/requirements.txt
python search_app.py --db /path/to/archive.db
```

Open <http://127.0.0.1:8001>. Alternatively, put `searchcord.db` in `data/`
and run `python search_app.py`, or pass `--data-dir /path/to/data`.
Relative paths resolve against the package root. `--db` and `--data-dir`
are mutually exclusive. `SEARCHCORD_DB` also accepts an explicit file path;
`--db` takes precedence over that environment variable.

The reader accepts SearchCord schema v9 or packed schema v101, not arbitrary
SQLite databases. A missing or incompatible archive produces an error without
creating or upgrading it. Normal read-only mode can follow a live v9 archive;
its directory may need writable SQLite WAL sidecars.

## Make a smaller deployment file

Run this in the full repository, choosing a new destination:

```bash
python search_snapshot.py data/searchcord.db packed/searchcord.db --report packed/build-report.json
```

The exporter preserves the displayed message fields and verifies their hashes.
It excludes settings and tokens, but includes messages and saved profiles.
Review the included data before sharing it. Image files remain on Discord;
expired attachment links are not refreshed by the search service.

Copy the verified file to the host and run:

```bash
python search_app.py --db /path/to/packed/searchcord.db --snapshot
```

Snapshot mode requires a closed, unchanging file with no pending WAL. To update,
stop the display service, point it to a newly verified file, and restart it.
Keep data outside `static/`. Only the search file is needed on the display host.
The original collector can stay on your own machine.

## Host behind a reverse proxy

Keep the application on loopback and put HTTPS and access control at the proxy.
The search application itself has no login. Anyone who can reach its API can
read included messages and profiles.

For example, on a small Linux server with a closed snapshot:

```bash
SEARCHCORD_DB=/srv/searchcord/searchcord.db SEARCHCORD_IMMUTABLE=1 \
  .venv/bin/uvicorn search_app:app --host 127.0.0.1 --port 8001 \
  --workers 2 --limit-concurrency 16
```

This is a starting configuration, not a measured capacity guarantee. Each worker
has its own connection and decoded-block caches. Measure memory and latency
before increasing concurrency. Do not run the collector with multiple workers.

## Container

From the package root:

```bash
docker build -f deploy/search/Dockerfile -t searchcord-search .
docker run --rm -p 127.0.0.1:8001:8001 \
  -v /absolute/packed:/app/data:ro searchcord-search
```

The container runs as an unprivileged user. The mounted directory must contain
`searchcord.db` and be readable by that user. Use a closed snapshot. The
build context explicitly allows only the search runtime files; local archives,
worktrees, credentials and collector assets are excluded. The Dockerfile is
supplied for deployment convenience; check its build on your host.
