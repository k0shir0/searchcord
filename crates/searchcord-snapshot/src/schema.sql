
CREATE TABLE snapshot_info(key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE message_attachments(message_id INTEGER PRIMARY KEY,payload TEXT NOT NULL);
CREATE TABLE ref_values(ref INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE);
CREATE TABLE record_names(id INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE);
CREATE TABLE message_sources(ref INTEGER PRIMARY KEY, channel_id TEXT, guild_id TEXT,
    UNIQUE(channel_id,guild_id));
CREATE INDEX idx_sources_guild ON message_sources(guild_id);
CREATE TABLE message_index(rowid INTEGER PRIMARY KEY, source_id INTEGER, author_id INTEGER);
CREATE TABLE message_blocks(block INTEGER PRIMARY KEY, first_id INTEGER NOT NULL UNIQUE,
    payload BLOB NOT NULL);
CREATE VIRTUAL TABLE messages_fts USING fts5(content, content='',
    tokenize='trigram', detail='none', columnsize=0);
CREATE VIRTUAL TABLE message_short_fts USING fts5(tokens, content='',
    tokenize='ascii', detail='none', columnsize=0);
CREATE VIEW message_records AS
    SELECT record_id(m.rowid) id, s.channel_id, s.guild_id, a.value author_id,
        record_content(m.rowid) content, record_timestamp(m.rowid) timestamp,
        record_image_urls(m.rowid) image_urls, record_channel_name(m.rowid) channel_name,
        record_guild_name(m.rowid) guild_name, record_author_name(m.rowid) author_name,
        m.rowid storage_rowid
    FROM message_index m
    LEFT JOIN message_sources s ON s.ref=m.source_id
    LEFT JOIN ref_values a ON a.ref=m.author_id;
