-- Standard collection schema v10, matching storage.py. No credentials.
CREATE TABLE authors (
                id TEXT PRIMARY KEY, name TEXT NOT NULL,
                last_message_id TEXT NOT NULL DEFAULT '0');

CREATE TABLE channel_authors (
                channel_id TEXT NOT NULL, author_id TEXT NOT NULL,
                PRIMARY KEY (channel_id, author_id)) WITHOUT ROWID;

CREATE TABLE channels (
                id TEXT PRIMARY KEY, guild_id TEXT, name TEXT NOT NULL);

CREATE TABLE collection_jobs (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL,
                state_json TEXT NOT NULL, updated_at REAL NOT NULL);

CREATE TABLE guilds (id TEXT PRIMARY KEY, name TEXT NOT NULL);

CREATE TABLE legacy_message_attachments (
                message_id INTEGER PRIMARY KEY, attachments TEXT NOT NULL);

CREATE TABLE message_history (
                rowid INTEGER PRIMARY KEY, timestamp TEXT,
                timestamp_override INTEGER NOT NULL DEFAULT 0,
                guild_name_id INTEGER, channel_name_id INTEGER, author_name_id INTEGER);

CREATE TABLE message_payloads (
                message_id INTEGER PRIMARY KEY, payload TEXT NOT NULL,
                first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL,
                deleted_at TEXT, complete INTEGER NOT NULL DEFAULT 1);

CREATE TABLE message_revisions (
                revision_id INTEGER PRIMARY KEY, message_id INTEGER NOT NULL,
                observed_at TEXT NOT NULL, event TEXT NOT NULL,
                source TEXT NOT NULL, payload TEXT);

CREATE TABLE messages (
                    rowid INTEGER PRIMARY KEY, id INTEGER NOT NULL UNIQUE,
                    channel_id INTEGER, guild_id INTEGER, author_id INTEGER,
                    content TEXT, image_urls TEXT);

CREATE VIRTUAL TABLE messages_fts USING
                fts5(content, content='messages', content_rowid='rowid',
                     tokenize='trigram', detail='none', columnsize=0);

CREATE TABLE name_values (
                id INTEGER PRIMARY KEY, value TEXT UNIQUE);

CREATE TABLE profile_details (
                user_id TEXT PRIMARY KEY, payload TEXT NOT NULL, fetched_at TEXT NOT NULL);

CREATE TABLE profiles (
                user_id TEXT PRIMARY KEY, username TEXT NOT NULL, global_name TEXT,
                avatar_hash TEXT, banner_hash TEXT, accent_color INTEGER,
                bot INTEGER, public_flags INTEGER, fetched_at TEXT NOT NULL);

CREATE TABLE scrape_cursors (
                channel_id TEXT PRIMARY KEY, guild_id TEXT,
                newest_message_id TEXT NOT NULL, oldest_message_id TEXT,
                history_complete INTEGER NOT NULL DEFAULT 0,
                pending_after_message_id TEXT, pending_before_message_id TEXT);

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);

CREATE TABLE stats_counts (
                kind TEXT NOT NULL, key TEXT NOT NULL, count INTEGER NOT NULL,
                PRIMARY KEY (kind, key)) WITHOUT ROWID;

CREATE VIEW message_records AS
                SELECT m.id, CAST(m.channel_id AS TEXT) channel_id,
                       CAST(m.guild_id AS TEXT) guild_id,
                       CAST(m.author_id AS TEXT) author_id, m.content,
                       CASE WHEN h.rowid IS NOT NULL AND h.timestamp_override
                           THEN h.timestamp
                           WHEN h.rowid IS NOT NULL THEN CASE WHEN ((m.id >> 22)+1420070400000)%1000=0 THEN
        strftime('%Y-%m-%dT%H:%M:%S',((m.id >> 22)+1420070400000)/1000.0,'unixepoch')||'+00:00'
        ELSE strftime('%Y-%m-%dT%H:%M:%f',((m.id >> 22)+1420070400000)/1000.0,'unixepoch')||'000+00:00' END
                           ELSE
                           strftime('%Y-%m-%dT%H:%M:%fZ',
                           ((m.id >> 22)+1420070400000)/1000.0,'unixepoch') END timestamp,
                       m.image_urls,
                       CASE WHEN h.rowid IS NOT NULL THEN cn.value ELSE c.name END channel_name,
                       CASE WHEN h.rowid IS NOT NULL THEN gn.value ELSE g.name END guild_name,
                       CASE WHEN h.rowid IS NOT NULL THEN an.value ELSE a.name END author_name
                FROM messages m
                LEFT JOIN message_history h ON h.rowid=m.rowid
                LEFT JOIN name_values gn ON gn.id=h.guild_name_id
                LEFT JOIN name_values cn ON cn.id=h.channel_name_id
                LEFT JOIN name_values an ON an.id=h.author_name_id
                LEFT JOIN channels c ON c.id=CAST(m.channel_id AS TEXT)
                LEFT JOIN guilds g ON g.id=CAST(m.guild_id AS TEXT)
                LEFT JOIN authors a ON a.id=CAST(m.author_id AS TEXT);

CREATE INDEX idx_au_id ON messages(author_id,id);

CREATE INDEX idx_authors_name ON authors(name COLLATE NOCASE);

CREATE INDEX idx_ch_id ON messages(channel_id,id);

CREATE INDEX idx_channels_guild ON channels(guild_id);

CREATE INDEX idx_cursor_guild ON scrape_cursors(guild_id);

CREATE INDEX idx_gd_id ON messages(guild_id,id) WHERE guild_id IS NOT NULL;

CREATE INDEX idx_profiles_username ON profiles(username COLLATE NOCASE);

CREATE INDEX idx_revisions_message
                ON message_revisions(message_id,revision_id);

CREATE TRIGGER messages_fts_delete
                AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts,rowid,content)
                VALUES ('delete',old.rowid,old.content);
                END;

CREATE TRIGGER messages_fts_insert
                AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid,content) VALUES (new.rowid,new.content);
                END;

CREATE TRIGGER messages_fts_update
                AFTER UPDATE OF content ON messages BEGIN
                INSERT INTO messages_fts(messages_fts,rowid,content)
                VALUES ('delete',old.rowid,old.content);
                INSERT INTO messages_fts(rowid,content) VALUES (new.rowid,new.content);
                END;

CREATE TRIGGER messages_history_delete
                AFTER DELETE ON messages BEGIN
                DELETE FROM message_history WHERE rowid=old.rowid;
                END;

CREATE TRIGGER messages_payload_delete
                AFTER DELETE ON messages BEGIN
                DELETE FROM message_payloads WHERE message_id=old.id;
                DELETE FROM message_revisions WHERE message_id=old.id;
                DELETE FROM legacy_message_attachments WHERE message_id=old.id;
                END;

CREATE TRIGGER messages_stats_delete
                AFTER DELETE ON messages BEGIN
                INSERT INTO stats_counts(kind,key,count) VALUES
                    ('total','',-1),
                    ('guild',COALESCE(CAST(old.guild_id AS TEXT),''),-1),
                    ('channel',COALESCE(CAST(old.channel_id AS TEXT),''),-1),
                    ('author',COALESCE(CAST(old.author_id AS TEXT),''),-1),
                    ('day',date(((old.id >> 22)+1420070400000)/1000.0,'unixepoch'),-1),
                    ('hour',strftime('%H',((old.id >> 22)+1420070400000)/1000.0,'unixepoch'),-1)
                ON CONFLICT(kind,key) DO UPDATE SET count=count-1;
                END;

CREATE TRIGGER messages_stats_insert
                AFTER INSERT ON messages BEGIN
                INSERT INTO stats_counts(kind,key,count) VALUES
                    ('total','',1),
                    ('guild',COALESCE(CAST(new.guild_id AS TEXT),''),1),
                    ('channel',COALESCE(CAST(new.channel_id AS TEXT),''),1),
                    ('author',COALESCE(CAST(new.author_id AS TEXT),''),1),
                    ('day',date(((new.id >> 22)+1420070400000)/1000.0,'unixepoch'),1),
                    ('hour',strftime('%H',((new.id >> 22)+1420070400000)/1000.0,'unixepoch'),1)
                ON CONFLICT(kind,key) DO UPDATE SET count=count+1;
                END;
PRAGMA user_version=10;
