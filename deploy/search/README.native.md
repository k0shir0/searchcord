# Native read-only Searchcord

This folder runs independently with the native viewer. Python and a collector
are not required. Copy an approved v9/v10 or v101 archive to `data/searchcord.db`,
or run `./searchcord-search --db /absolute/path/archive.db`. Windows users can
run `start-search.bat --db "C:\Path With Spaces\archive.db"`.

Open http://127.0.0.1:8001. `--host` and `--port` change the listener.
`--db` and `--data-dir` are mutually exclusive. Paths are relative to the binary's
folder. `SEARCHCORD_DB`, `SEARCHCORD_DATA_DIR` and `SEARCHCORD_IMMUTABLE=1`
retain their meanings. `--snapshot` requires a closed file without pending WAL.
Live standard archives use normal read-only mode. Missing files remain missing.

The viewer exposes saved messages/profiles without authentication. Put HTTPS and
access controls at your reverse proxy when sharing access. It never collects or
refreshes upstream data. Keep the database and sibling `media/` outside `static/`.
To update a snapshot, stop the viewer, replace the approved deployment copy, and
restart. Keep the source archive and the previous verified deployment for rollback.

Create a standalone folder from a checkout after building the release binaries:

```
cargo build --release --locked -p searchcord-viewer -p searchcord-snapshot
target/release/searchcord-package-search "new viewer folder"
```

The packager refuses existing destinations and copies an explicit asset list,
licenses, this guide and the native binary. It never copies an archive or vault.
The binary embeds the same approved assets so requests cannot traverse the folder.
The Python deployment scripts remain reference/development tooling during migration.

For the Linux container, first compile a static Linux viewer (not a Windows or
macOS binary). From the checkout on Linux x86-64:

```
RUSTFLAGS='-C target-feature=+crt-static' cargo build --release --locked --target x86_64-unknown-linux-gnu -p searchcord-viewer
target/x86_64-unknown-linux-gnu/release/searchcord-package-search "linux viewer folder"
cd "linux viewer folder"
docker build -t searchcord-search-native .
docker run --rm -p 127.0.0.1:8001:8001 -v /absolute/approved-data:/app/data:ro searchcord-search-native
```

The scratch image runs as 65534:65534 and contains the prebuilt viewer, approved
assets, licenses and guide. It contains no Python, collector, credentials or
archives. The mounted archive and optional media directory must be readable and
traversable by that identity. Snapshot media is private to its owner by default;
grant access only to the approved deployment copy, or use the owner's numeric
container identity. Verify a media response as well as search before deployment.

This read-only milestone does not complete the collector or performance gates.
