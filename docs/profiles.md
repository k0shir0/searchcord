# Archived profiles

Click an author's name in Browse results to open their archived profile. The card
uses a Discord-style banner, overlapping avatar, identity, bio, dates, badges and
connections in Searchcord's palette. Its server panel is **observed archive
history**, not a claim about live membership or mutual servers with your account.
Servers are searchable, scrollable, paginated and sorted by saved message count
descending. Selecting one searches that author's messages in that server.
The avatar is displayed from its Discord CDN URL and opens that URL when clicked;
Searchcord saves no image bytes to disk. The bio is labelled **About Me**.

Basic saved profiles remain readable. **Scrape extended profile** (or **Refresh
profile**) explicitly fetches the selected user with the currently selected
Discord account. Near the top of Scrape, **backfill saved profiles**
visits authors already in the archive in bounded batches. Stop preserves finished
work. Restart skips saved extended profiles. Neither action fetches automatically
when you open a card. Rate limits, timeouts, malformed responses, server errors,
and SQLite busy errors retry the same user with a 2–60 second backoff, honoring
longer Discord cooldowns. Progress identifies the current user and wait. Stop
interrupts an active request or cooldown. HTTP 403/404 skip that unavailable
user; HTTP 401 stops bulk work so account authorization can be corrected.
Successful profiles commit before the next request's cooldown begins.
Existing profile data is kept on failed requests. Unknown/unavailable fields are
not invented and presence is not inferred.

The card shows online and custom status as a snapshot only if the saved response
explicitly contains presence data. Otherwise both fields say **Not returned by
Discord**. The profile lookup used for backfill normally omits presence; Discord
delivers it through [Gateway presence events](https://discord.com/developers/docs/events/gateway-events#presence-update),
with visibility restrictions. Backfill therefore cannot promise live or historical
status for every saved user ID.

`profile_details` is an additive table alongside the schema v9 `profiles` table.
It stores the complete returned profile JSON and its fetch timestamp, preserving
fields outside the initial card renderer. Supported card fields include avatar,
banner, bio, pronouns, connected accounts, badges, creation date from user ID,
saved date and subscriber date when returned. Clearing the archive also removes
this table's rows and stops bulk profile collection. Existing optional profile
harvesting during message collection requests these extended details immediately
after each saved message page, before requesting another page or channel. It
checks unique real authors from that page, skips webhooks and already saved
extended profiles, and rechecks under the shared lock to avoid duplicate fetches
between collection jobs and backfill. Backfill handles older missing authors
separately. A transient failure never advances past the failed user.

The public user-profile endpoint can return only what the active account is
allowed to see. The upstream [client API documentation](https://discordpy-self.readthedocs.io/en/latest/api.html#discord.Client.fetch_user_profile)
describes its access limitations. A successful basic user lookup alone cannot
provide the full card. This prototype does not try to bypass account access.
Saved CDN links display avatars and banners; image bytes are not stored locally.

`profile_store.py` provides read functions shared with the display frontend;
`profile_api.py` owns collection operations and job lifetime. No new package or
external rendering dependency is required. Channel suggestions now present a
name without leading emoji/separators while retaining the stable channel ID.
Stored names and visible message source names are unchanged.

Verification uses synthetic data, with mocked Discord responses for successful
extended fetch, preservation on failure and resumable bulk collection:

```bash
python -m unittest discover -s checks -v
python checks/profile_fixture.py agents/profile-fixture
# Start the collection app in a separate terminal with SEARCHCORD_DATA_DIR
# set to the absolute path of agents/profile-fixture, on port 8002.
node checks/browser_profiles.mjs http://127.0.0.1:8002 agents/profile-browser
```

The browser suite expects the synthetic Alice/Bob archive created above. The
fixture builder refuses to overwrite an existing database.
It checks card navigation, server paging/search, mobile fit, missing-account
errors, profile message drilldown and explicit backfill controls. No personal
tokens or live Discord requests are used in automated tests.
