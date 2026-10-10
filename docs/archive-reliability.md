# Archive reliability

Collection archives now use schema v10; the read-only viewer accepts v9, v10 and
verified packed snapshots. Test an upgrade on a private copy before starting
the collector against an important archive. The code upgrade does not itself
open or migrate a database until the collector starts.

## Credentials and recovery copies

Credentials are sealed in an external per-archive vault. SQLite retains saved
account labels and IDs, without token values. Windows DPAPI binds the vault to
the current operating-system user on that machine. Moving to another Windows
machine normally requires re-entering credentials or restoring the original
DPAPI keys. On other platforms, Fernet uses a separate
owner-only key file. Restore the vault and its OS identity or key independently;
a database backup alone cannot restore collector login.

`SEARCHCORD_CREDENTIAL_FILE` and `SEARCHCORD_CREDENTIAL_KEY_FILE` override the
external vault/key paths. Keep both outside archive, export and shared folders.
Losing the key prevents decryption; missing or invalid keys fail closed.
New recovery backups omit credential settings. Historical database free pages,
WAL and existing backups can still contain earlier plaintext. This upgrade
does not securely erase them. Keep them private and rotate credentials when
retiring old copies.

## Message and media fidelity

Every new successful message observation saves the complete decoded Discord
message JSON, including non-image attachments and embeds. Re-fetched IDs update
current searchable text while preserving distinct observed payloads. Confirmed
upstream absence adds a tombstone and retains the archived contents. Read the
payload and bounded revision pages through `/api/messages/{id}/archive`; refresh
a known message through `/api/messages/{id}/refresh` using the collector account.

History covers observations. It cannot reconstruct changes between observations,
messages never exposed to the account, or original fields discarded by earlier
versions. Old selective rows remain incomplete until re-fetched. Available legacy
attachment values survive migration. A latest-page monitor can observe recent
edits/deletions; this is not a full-account continuous change log.

Local media lives in the database's sibling `media/` directory. HTTPS downloads
accept Discord's CDN/media hosts, without credentials, proxies or redirects.
Files have a 25 MiB size limit, a 15-second request limit and a shared 30-second
page download budget with four workers. Digest checks run off the event loop;
an in-flight check can finish after the budget, but no new download starts once
it expires. Saved files are verified by SHA-256; failures and
limits are recorded rather than presented as successful copies. A complete
message payload does not imply all of its binaries were downloaded.

`python search_snapshot.py SOURCE.db NEWDIR/searchcord.db --with-media` copies
only available referenced media into a fresh `NEWDIR/media/`. The snapshot
preserves displayed messages, profiles and attachment metadata, without settings,
full message JSON or revision history. Keep the original collector archive for
those records. Share the verified snapshot and its media directory together.
The exporter still includes all archived messages and saved profiles; review
the dataset before sharing it.

## Collection and cleanup

Scrape pages, channel cursors and job checkpoints commit together. Saved queues,
remaining limits and shared pacing deadlines survive restart. Scrapes, invite
queues and live monitors recover paused, requiring their original saved account
to resume. An invite request interrupted after sending may have joined before
the restart; check Discord before explicitly resuming. DM deletion jobs never
restart automatically. Profile backfill can be rerun and skips completed work.

Archive clearing requires confirmation and rejects active or paused scrapes,
live monitors and DM cleanup. It blocks new writers while stopping profile jobs
and clearing messages, revisions, profiles, cursors and owned media-cache files.
Saved credentials and unrelated files remain.

DM cleanup counts only HTTP 204 as a confirmed deletion. HTTP 404 with Discord's
Unknown Message code `10008` is reported separately as already absent. Unknown
Channel and generic 404 responses remain failures and do not create tombstones.
Failed responses and exhausted transient retries
remain failures; reported confirmed totals never include them.

## Search and deployment limits

Cursor search first uses indexed/adaptive queries. If SQL reaches its deadline,
the reader retries over a bounded message-ID window. A partial page may contain
no matches while older history remains; its continuation cursor advances over
the examined range. Continue with `scan=true` and the returned `next_cursor`.
The interface labels that action **Search older**. This preserves eventual
coverage without promising uniform latency for every phrase or sparse filter.

Legacy counted searches retain a SQL deadline. ChatML exports stream database
batches and retain only adjacent conversation turns; one very large merged turn
can still occupy memory. An interrupted streamed download is incomplete.

Container checks and concurrent-request measurements use isolated synthetic
fixtures. They establish build/runtime behavior on the measured host, not a
production capacity guarantee for a larger archive or a different VPS. Keep
the original collector single-process and use the read-only viewer for hosting.
