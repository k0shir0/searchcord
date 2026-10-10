# Collection terms

- **Collection queue**: the visible list of selected sources, including the active
  batch and browser-owned pending sources. Each source appears once by stable ID.
- **Source**: a readable channel, thread, direct message or group conversation
  selected for collection, with its ID, display name and server context.
- **Active scrape**: a server-owned execution with saved progress, options,
  pagination cursor and remaining limit. A browser start reserves an immutable
  active batch while waiting for the server's job ID.
- **Active batch**: the fixed source list belonging to one active scrape. Later
  scans cannot change its progress rows or source list.
- **Pending source**: a queued source outside the active batch, retained in the
  current browser page until another scrape starts or the queue/account changes.
- **Pending scan**: a browser-owned request for readable server sources. It is
  tied to the account generation that requested it; its completion may add sources
  but cannot override a subsequent stop, clear or scrape failure.
- **Saved account**: a stored Discord credential selected for upstream requests.
  An active scrape retains its original saved account even after selection changes.
