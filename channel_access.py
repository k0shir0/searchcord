"""Verify archive access, including channels with visible but unreadable history."""

from fastapi import HTTPException


VIEW_CHANNEL = 1 << 10
READ_MESSAGE_HISTORY = 1 << 16
ADMINISTRATOR = 1 << 3
MANAGE_THREADS = 1 << 34
READABLE = VIEW_CHANNEL | READ_MESSAGE_HISTORY
MESSAGE_TYPES = {0, 5, 10, 11, 12}
THREAD_TYPES = {10, 11, 12}
THREAD_PARENTS = {0, 5, 15, 16}


def permissions_for(guild, member, channel, channels):
    """Discord's everyone, combined-role, then member overwrite precedence.

    Threads inherit their parent's permissions. Private-thread membership still
    needs a message probe; an empty successful probe alone cannot prove history
    access because Discord also returns [] when READ_MESSAGE_HISTORY is absent.
    """
    user_id = member["user"]["id"]
    roles = {role["id"]: int(role["permissions"]) for role in guild["roles"]}
    permissions = roles[guild["id"]]
    for role_id in member["roles"]:
        permissions |= roles[role_id]
    if user_id == guild["owner_id"] or permissions & ADMINISTRATOR:
        return -1
    if channel["type"] in THREAD_TYPES:
        channel = channels[channel["parent_id"]]
    overwrites = channel["permission_overwrites"]
    role_ids = set(member["roles"])
    everyone = next((o for o in overwrites if o["id"] == guild["id"] and o["type"] == 0), None)
    if everyone:
        permissions = (permissions & ~int(everyone["deny"])) | int(everyone["allow"])
    allow = deny = 0
    for overwrite in overwrites:
        if overwrite["type"] == 0 and overwrite["id"] in role_ids:
            allow |= int(overwrite["allow"])
            deny |= int(overwrite["deny"])
    permissions = (permissions & ~deny) | allow
    own = next((o for o in overwrites if o["id"] == user_id and o["type"] == 1), None)
    if own:
        permissions = (permissions & ~int(own["deny"])) | int(own["allow"])
    return permissions


async def readable_channels(guild_id, token, discord, *, include_threads=False):
    async def fetch(path, *, optional=False, **kwargs):
        response = await discord("GET", path, token, **kwargs)
        if optional and response.status_code in (403, 404):
            try:
                if response.json().get("code") == 40002:
                    raise HTTPException(403, "Complete account verification in Discord before checking channels.")
            except (ValueError, AttributeError):
                pass
            return None
        if response.status_code != 200:
            raise HTTPException(response.status_code,
                                f"Could not verify channel access (HTTP {response.status_code}). Try again.")
        try:
            return response.json()
        except ValueError:
            raise HTTPException(502, "Discord returned invalid channel access data. Try again.") from None

    raw = await fetch(f"/guilds/{guild_id}/channels")
    if not isinstance(raw, list):
        raise HTTPException(502, "Discord returned an invalid channel list.")
    if not raw:
        return []
    guild = await fetch(f"/guilds/{guild_id}")
    user = await fetch("/users/@me")
    try:
        user_id = user["id"]
    except (KeyError, TypeError):
        raise HTTPException(502, "Discord returned invalid account data.") from None
    member = await fetch(f"/guilds/{guild_id}/members/{user_id}")
    try:
        if guild["id"] != guild_id or member["user"]["id"] != user_id:
            raise ValueError
        by_id = {channel["id"]: channel for channel in raw}
        permitted = {channel["id"]: permissions_for(guild, member, channel, by_id)
                     for channel in raw}
    except (KeyError, ValueError, TypeError):
        raise HTTPException(502, "Discord returned incomplete channel permissions. Try again.") from None

    # Guild channel lists omit threads, including forum/media posts. Discover
    # active and archived threads without requiring membership in private ones.
    parents = [c for c in raw if include_threads and c["type"] in THREAD_PARENTS
               and permitted[c["id"]] & READABLE == READABLE]
    threads = {}
    if parents:
        active = await fetch(f"/guilds/{guild_id}/threads/active", optional=True)
        if active is not None:
            try:
                threads.update({t["id"]: t for t in active["threads"]})
            except (KeyError, TypeError):
                raise HTTPException(502, "Discord returned an invalid thread list.") from None
    for parent in parents:
        paths = [f"/channels/{parent['id']}/threads/archived/public"]
        if parent["type"] == 0:
            paths.append(f"/channels/{parent['id']}/users/@me/threads/archived/private")
            if permitted[parent["id"]] & MANAGE_THREADS:
                paths.append(f"/channels/{parent['id']}/threads/archived/private")
        for path in paths:
            params = {"limit": 100}
            while True:
                page = await fetch(path, optional=True, params=params)
                if page is None:
                    break
                try:
                    batch = page["threads"]
                    if not isinstance(batch, list):
                        raise ValueError
                    threads.update({t["id"]: t for t in batch})
                    if not page["has_more"]:
                        break
                    before = (batch[-1]["id"] if "/users/@me/" in path
                              else batch[-1]["thread_metadata"]["archive_timestamp"])
                    if not before or before == params.get("before"):
                        raise ValueError
                    params = {"limit": 100, "before": before}
                except (KeyError, IndexError, ValueError, TypeError):
                    raise HTTPException(502, "Discord returned incomplete archived threads. Try again.") from None

    categories = {c["id"]: c["name"] for c in raw if c["type"] == 4}
    candidates = {c["id"]: c for c in raw if c["type"] in MESSAGE_TYPES}
    candidates.update(threads)
    result = []
    for channel in sorted(candidates.values(), key=lambda c: (c.get("parent_id") or "", c.get("position", 0), c["id"])):
        try:
            if permissions_for(guild, member, channel, by_id) & READABLE != READABLE:
                continue
        except KeyError:
            # A thread whose parent isn't visible cannot be archived.
            if channel["type"] in THREAD_TYPES and channel.get("parent_id") not in by_id:
                continue
            raise HTTPException(502, "Discord returned incomplete channel permissions. Try again.") from None
        except (ValueError, TypeError):
            raise HTTPException(502, "Discord returned invalid channel permissions. Try again.") from None
        probe = await fetch(f"/channels/{channel['id']}/messages", optional=True, params={"limit": 1})
        if probe is None:
            continue
        if not isinstance(probe, list):
            raise HTTPException(502, "Discord returned an invalid channel access probe. Try again.")
        parent_id = channel.get("parent_id")
        result.append({"id": channel["id"], "name": channel["name"], "type": channel["type"],
                       "category": categories.get(parent_id, by_id.get(parent_id, {}).get("name", "")),
                       "category_id": parent_id, "nsfw": channel.get("nsfw", False)})
    return result
