Searchcord native applications

Windows

Extract the whole ZIP and open Start Searchcord. The browser opens when the
archive is ready. To browse an archive separately, open Start Searchcord Search
and visit http://127.0.0.1:8001.

To update, close both apps and open Update Searchcord. It checks GitHub Releases
and installs a newer version when available. Archives, cached media and saved
accounts stay in place. Keep the launchers and the app folder together.

For the first upgrade from an older Windows setup, close Searchcord and extract
the release into the existing Searchcord folder. Open Start Searchcord from
there. This keeps the default data folder in use. If you configured
SEARCHCORD_DATA_DIR, the launchers continue to use it.

Linux, macOS and custom archives

Extract the archive for your system. On Linux and macOS, run the executables
directly. For a custom archive on Windows, pass the same absolute path arguments
to the launchers. The archive can stay in its existing folder.

  ./searchcord --data-dir /absolute/path/to/collection-folder
  ./searchcord-search --db /absolute/path/to/archive.db
  ./searchcord-snapshot /absolute/source.db /absolute/new-snapshot.db --with-media
  ./searchcord-package-search /absolute/new-viewer-folder

The collector uses http://127.0.0.1:8000 and the viewer uses port 8001. Pass
--host or --port to change the binding. The collector accepts --no-browser.
Direct binary launches resolve relative archive paths beside the executable.
The packaged Windows launchers resolve them beside the launchers and default
to data/searchcord.db.
The viewer can read standard v9/v10 archives and packed v101 snapshots. Use the
standard archive for collection. Export snapshots to a new destination.

Archive recovery

When moving from the legacy Python app, stop it before opening the archive with
the native collector. Keep a recovery copy of the archive, including media, and
the external credential vault and key. Windows vaults are tied to the same user.
An older archive upgrade can create an additional recovery backup beside the
database. Keep older archives and backups private because they can contain saved
credentials. To roll back a schema upgrade, restore the archive and vault copies
before opening them with the previous app.

Recovered jobs stay paused until resumed with the original account. Check
interrupted invite joins in Discord before retrying. Run one collector per archive.

Keep archives private. The applications bind to loopback and have no built-in
login. A hosted viewer needs access control and HTTPS provided by its host.
More information at https://github.com/k0shir0/searchcord#readme
