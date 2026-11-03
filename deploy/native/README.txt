Searchcord native applications

This folder is for the operating system and architecture in its archive name.
No Python runtime is required. On Windows, use start.bat or start-search.bat.
On Linux and macOS, run the executables directly:

  ./searchcord --data-dir /absolute/path/to/collection-folder
  ./searchcord-search --db /absolute/path/to/archive.db
  ./searchcord-snapshot /absolute/source.db /absolute/new-snapshot.db --with-media
  ./searchcord-package-search /absolute/new-viewer-folder

The collector uses http://127.0.0.1:8000 and the viewer uses port 8001.
Pass --host or --port to change the binding. The collector accepts --no-browser.
Relative archive paths resolve beside the executable. Collection uses a standard
v10 archive. Standard v9/v10 archives and packed v101 snapshots can be searched.
Snapshots are read-only. Export to a new destination; existing files are refused.

Stop the old collector before upgrading. Back up the archive folder, media and
external credential vault/key. Recovered jobs stay paused until resumed with
the original account. Check interrupted invite joins in Discord before retrying.
To roll back, stop Searchcord and restore the saved archive and credential copies
before running the previous version. Never run two collectors on one database.

Keep archives private. The applications bind to loopback and have no built-in
login. A hosted viewer needs access control and HTTPS provided by its host.
Operating instructions: https://github.com/k0shir0/searchcord#readme
