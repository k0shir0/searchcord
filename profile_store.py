"""Shared, read-only presentation of archived Discord profiles."""
from contextlib import closing
from datetime import datetime, timezone
import json
import re
import sqlite3
import unicodedata

from fastapi import HTTPException
from media_store import local_url, media_status

STALE_AFTER_SECONDS = 24 * 60 * 60


def user_id(value):
    value = str(value)
    if not re.fullmatch(r"[0-9]{1,19}", value) or not 0 < int(value) <= 9223372036854775807:
        raise HTTPException(400, "Enter a valid Discord user ID.")
    return value


def search_label(value):
    """Ignore emoji and ornamental separators without changing stored names."""
    value = unicodedata.normalize('NFKC', value or '').casefold()
    return ' '.join(''.join(c if unicodedata.category(c)[0] in 'LNM' else ' ' for c in value).split())


def image_url(uid, value, kind='avatars', size=256):
    if not isinstance(value, str) or not re.fullmatch(r'(?:a_)?[A-Za-z0-9]+', value):
        return None
    extension = 'gif' if value.startswith('a_') else 'png'
    return f'https://cdn.discordapp.com/{kind}/{uid}/{value}.{extension}?size={size}'


def presence_fields(payload):
    """Only expose status when a saved response explicitly contains presence."""
    presence = payload.get('presence')
    if not isinstance(presence, dict):
        return None, None
    status = presence.get('status')
    if status not in ('online', 'idle', 'dnd', 'offline'):
        status = None
    activities = presence.get('activities')
    if not isinstance(activities, list):
        return status, None
    for activity in activities:
        if (isinstance(activity, dict) and activity.get('type') == 4
                and isinstance(activity.get('state'), str)):
            return status, activity['state']
    return status, None


def read_profile(db, uid, archive_path=None):
    uid = user_id(uid)
    db.row_factory = sqlite3.Row
    author = db.execute('SELECT name FROM authors WHERE id=?', (uid,)).fetchone()
    row = db.execute('SELECT * FROM profiles WHERE user_id=?', (uid,)).fetchone()
    if not author and not row:
        raise HTTPException(404, 'This user is not in the archive.')
    basic = dict(row) if row else {}
    extended = None
    if db.execute("SELECT 1 FROM sqlite_master WHERE name='profile_details' AND type='table'").fetchone():
        extended = db.execute('SELECT payload,fetched_at FROM profile_details WHERE user_id=?', (uid,)).fetchone()
    payload = json.loads(extended['payload']) if extended else {}
    user = payload.get('user') or {}
    details = payload.get('user_profile') or {}
    online_status, custom_status = presence_fields(payload)
    count = db.execute("SELECT count FROM stats_counts WHERE kind='author' AND key=?", (uid,)).fetchone()
    fetched_at = extended['fetched_at'] if extended else basic.get('fetched_at')
    age = None
    if fetched_at:
        try:
            observed = datetime.fromisoformat(fetched_at.replace('Z', '+00:00'))
            if observed.tzinfo is not None:
                age = max(0, int((datetime.now(timezone.utc) - observed).total_seconds()))
        except (ValueError, TypeError):
            pass
    avatar = image_url(uid, user.get('avatar') or basic.get('avatar_hash'))
    banner = image_url(uid, details.get('banner') or user.get('banner') or basic.get('banner_hash'), 'banners', 1024)
    # PRAGMA also works for synthetic in-memory fixtures without a disk cache.
    archive_path = archive_path or db.execute('PRAGMA database_list').fetchone()[2]
    media = {kind: media_status(archive_path, url) if archive_path else {'status': 'unavailable', 'reason': 'not_collected'}
             for kind, url in (('avatar', avatar), ('banner', banner)) if url}
    return {
        'id': uid, 'username': user.get('username') or basic.get('username') or (author['name'] if author else uid),
        'display_name': user.get('global_name') or basic.get('global_name') or (author['name'] if author else uid),
        'avatar_url': local_url(archive_path, avatar) if avatar and archive_path else avatar,
        'banner_url': local_url(archive_path, banner) if banner and archive_path else banner,
        'media': media,
        'bio': details.get('bio') or user.get('bio') or '',
        'online_status': online_status, 'custom_status': custom_status,
        'pronouns': details.get('pronouns') or '',
        'accent_color': details.get('accent_color') or user.get('accent_color') or basic.get('accent_color'),
        'badges': payload.get('badges') or [], 'connections': payload.get('connected_accounts') or [],
        'bot': bool(user.get('bot', basic.get('bot'))), 'public_flags': user.get('public_flags', basic.get('public_flags')),
        'created_at': datetime.fromtimestamp(((int(uid) >> 22) + 1420070400000)/1000, timezone.utc).isoformat(),
        'fetched_at': fetched_at, 'profile_age_seconds': age,
        'profile_stale': age is None or age >= STALE_AFTER_SECONDS,
        'observation_kind': 'profile_fetch' if extended else 'message_author',
        'presence_observed_at': fetched_at if online_status or custom_status else None,
        'extended': bool(extended), 'scraped': bool(row), 'message_count': count[0] if count else 0,
        'premium_since': payload.get('premium_since'),
        # Keep all returned public profile fields accessible without inventing missing values.
        'details': payload,
    }


def profile_servers(db, uid, q='', offset=0, limit=30):
    uid = user_id(uid)
    db.row_factory = sqlite3.Row
    db.create_function('search_label', 1, search_label, deterministic=True)
    pattern = '%' + search_label(q).replace('%', '!%').replace('_', '!_') + '%'
    rows = db.execute('''SELECT CAST(m.guild_id AS TEXT) AS id,
        COALESCE(g.name, 'Unknown server') AS name, m.messages,
        CAST(m.latest_id AS TEXT) AS latest_id
        FROM (SELECT guild_id, COUNT(*) AS messages, MAX(id) AS latest_id
              FROM messages WHERE author_id=? AND guild_id IS NOT NULL
              GROUP BY guild_id) m
        LEFT JOIN guilds g ON g.id=CAST(m.guild_id AS TEXT)
        WHERE search_label(g.name) LIKE ? ESCAPE '!' OR CAST(m.guild_id AS TEXT)=?
        ORDER BY messages DESC,m.guild_id LIMIT ? OFFSET ?''',
        (int(uid), pattern, q.strip(), limit+1, offset)).fetchall()
    return {'servers': [dict(row) for row in rows[:limit]], 'has_more': len(rows)>limit, 'offset': offset,
            'observation_kind': 'archived_messages', 'current_membership': None}


def from_file(path, function, *args, **kwargs):
    from pathlib import Path
    with closing(sqlite3.connect(Path(path).resolve().as_uri()+'?mode=ro', uri=True, timeout=5)) as db:
        return function(db, *args, **kwargs)
