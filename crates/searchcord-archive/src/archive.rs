use crate::{
    codec::{self, PackedDecoder},
    search::{self, Page, Query},
};
use chrono::{Datelike, NaiveDate};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize)]
pub struct ArchiveError {
    pub status: u16,
    pub detail: String,
}
impl ArchiveError {
    pub fn new(status: u16, message: &str) -> Self {
        Self {
            status,
            detail: message.to_owned(),
        }
    }
}
impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.detail)
    }
}
impl std::error::Error for ArchiveError {}
impl From<rusqlite::Error> for ArchiveError {
    fn from(error: rusqlite::Error) -> Self {
        if error.sqlite_error_code() == Some(ErrorCode::OperationInterrupted) {
            Self::new(
                408,
                "This search is too broad. Add a server, channel, author, or date filter and try again.",
            )
        } else {
            Self::new(
                503,
                "The archive could not be read. Check its location and access permissions.",
            )
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchOptions {
    pub q: String,
    pub guild_id: Option<String>,
    pub channel_id: Option<String>,
    pub author_id: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub before: Option<i64>,
    pub limit: usize,
    pub refresh_images: bool,
    pub scan: bool,
}
impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            q: String::new(),
            guild_id: None,
            channel_id: None,
            author_id: None,
            date_from: None,
            date_to: None,
            before: None,
            limit: 40,
            refresh_images: false,
            scan: false,
        }
    }
}

pub fn snowflake_day(value: &str, end: bool) -> Result<i64, ArchiveError> {
    let fail = || ArchiveError::new(400, "Enter a valid UTC date.");
    if !value.is_ascii()
        || ![7, 8, 10].contains(&value.len())
        || value
            .as_bytes()
            .get(..4)
            .is_none_or(|s| !s.iter().all(u8::is_ascii_digit))
    {
        return Err(fail());
    }
    let calendar = value.len() == 10
        && value.bytes().enumerate().all(|(i, c)| {
            if [4, 7].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_digit()
            }
        });
    let parsed = if calendar {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
    } else if value.len() == 8 && value.bytes().all(|c| c.is_ascii_digit()) {
        NaiveDate::parse_from_str(value, "%Y%m%d").ok()
    } else {
        let compact = value.replace('-', "");
        if [7, 8].contains(&compact.len())
            && compact.as_bytes()[4] == b'W'
            && compact[5..].bytes().all(|c| c.is_ascii_digit())
        {
            let extended = if compact.len() == 7 {
                format!("{}-W{}", &compact[..4], &compact[5..])
            } else {
                format!("{}-W{}-{}", &compact[..4], &compact[5..7], &compact[7..])
            };
            if value != compact && value != extended {
                None
            } else {
                let full = if compact.len() == 7 {
                    format!("{compact}1")
                } else {
                    compact
                };
                NaiveDate::parse_from_str(&full, "%GW%V%u").ok()
            }
        } else {
            None
        }
    }
    .ok_or_else(fail)?;
    if parsed.year() < 1 || parsed.year() > 9999 {
        return Err(fail());
    }
    let parsed = if end {
        parsed
            .succ_opt()
            .filter(|date| date.year() <= 9999)
            .ok_or_else(fail)?
    } else {
        parsed
    };
    let millis = parsed
        .and_hms_opt(0, 0, 0)
        .ok_or_else(fail)?
        .and_utc()
        .timestamp_millis();
    Ok((((millis as i128 - 1_420_070_400_000_i128) << 22).clamp(0, i64::MAX as i128)) as i64)
}
fn checked_query(query: &Query) -> Result<Query, ArchiveError> {
    let mut query = query.clone();
    query.q = search::strip(&query.q).to_owned();
    for value in [
        &mut query.guild_id,
        &mut query.channel_id,
        &mut query.author_id,
    ] {
        if let Some(raw) = value.as_ref().filter(|s| !s.is_empty()) {
            let id = if raw.bytes().all(|b| b.is_ascii_digit()) {
                raw.parse::<i64>().ok().filter(|id| *id > 0)
            } else {
                None
            };
            *value = Some(
                id.ok_or_else(|| {
                    ArchiveError::new(
                        400,
                        "Choose a filter suggestion or enter a valid Discord ID.",
                    )
                })?
                .to_string(),
            );
        }
    }
    if let (Some(lower), Some(upper)) = (query.lower, query.upper)
        && lower >= upper
    {
        return Err(ArchiveError::new(
            400,
            "From date must be on or before the to date.",
        ));
    }
    if query.window == 0 || query.window > 1_000_000 {
        return Err(ArchiveError::new(400, "Invalid scan window"));
    }
    if !(1..=100).contains(&query.limit) {
        return Err(ArchiveError::new(
            422,
            "Search limit must be between 1 and 100",
        ));
    }
    Ok(query)
}

#[derive(Clone)]
pub struct Archive {
    pub path: PathBuf,
    pub immutable: bool,
    timeout: Duration,
    pub scan_window: usize,
}
pub struct ReadSession {
    db: Connection,
    pub(crate) decoder: Option<PackedDecoder>,
}
impl ReadSession {
    pub fn database(&self) -> &Connection {
        &self.db
    }
    fn search_query(&self, query: &Query) -> Result<Page, ArchiveError> {
        Ok(if let Some(decoder) = &self.decoder {
            search::packed_search(&self.db, decoder, query)?
        } else {
            search::standard_search(&self.db, query)?
        })
    }
}
impl Archive {
    pub fn new(path: impl AsRef<Path>, immutable: bool) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            immutable,
            timeout: Duration::from_secs(4),
            scan_window: 16384,
        }
    }
    pub fn with_query_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    pub fn connect(&self) -> Result<ReadSession, ArchiveError> {
        if !self.path.is_file() {
            return Err(ArchiveError::new(
                503,
                "Archive not found. Set SEARCHCORD_DATA_DIR to an existing archive.",
            ));
        }
        let path = self.path.canonicalize().map_err(|_| {
            ArchiveError::new(
                503,
                "The archive could not be read. Check its location and access permissions.",
            )
        })?;
        let mut wal = path.as_os_str().to_os_string();
        wal.push("-wal");
        if self.immutable && std::fs::metadata(&wal).is_ok_and(|s| s.len() > 0) {
            return Err(ArchiveError::new(
                503,
                "Snapshot mode needs a closed archive with no pending WAL. Use normal read-only mode for a live archive.",
            ));
        }
        let mut uri = url::Url::from_file_path(&path).map_err(|_| {
            ArchiveError::new(
                503,
                "The archive could not be read. Check its location and access permissions.",
            )
        })?;
        uri.query_pairs_mut().append_pair("mode", "ro");
        if self.immutable {
            uri.query_pairs_mut().append_pair("immutable", "1");
        }
        let db = Connection::open_with_flags(
            uri.as_str(),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(Duration::from_secs(1))?;
        db.execute_batch("PRAGMA query_only=ON; BEGIN;")?;
        let deadline = Instant::now() + self.timeout;
        db.progress_handler(1000, Some(move || Instant::now() > deadline));
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if ![9, 10, codec::VERSION].contains(&version) {
            return Err(ArchiveError::new(
                503,
                "This display app needs a schema v9/v10 archive or a verified search snapshot.",
            ));
        }
        let decoder = if version == codec::VERSION {
            Some(codec::install_reader(&db)?)
        } else {
            None
        };
        Ok(ReadSession { db, decoder })
    }
    fn execute_search(&self, query: &Query) -> Result<Page, ArchiveError> {
        self.connect()?.search_query(query)
    }
    pub fn raw_search(&self, query: &Query) -> Result<Page, ArchiveError> {
        let query = checked_query(query)?;
        match self.execute_search(&query) {
            Err(error) if error.status == 408 && !query.scan => {
                let mut scan = query;
                scan.scan = true;
                self.execute_search(&scan)
            }
            result => result,
        }
    }
    pub fn search_records(&self, options: &SearchOptions) -> Result<Page, ArchiveError> {
        self.raw_search(&self.query_options(options)?)
    }
    pub(crate) fn search_session(
        &self,
        options: &SearchOptions,
    ) -> Result<(ReadSession, Page), ArchiveError> {
        let query = self.query_options(options)?;
        let session = self.connect()?;
        let page = session.search_query(&query)?;
        Ok((session, page))
    }
    fn query_options(&self, options: &SearchOptions) -> Result<Query, ArchiveError> {
        let lower = options
            .date_from
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| snowflake_day(s, false))
            .transpose()?;
        let upper = options
            .date_to
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| snowflake_day(s, true))
            .transpose()?;
        checked_query(&Query {
            q: options.q.clone(),
            guild_id: options.guild_id.clone(),
            channel_id: options.channel_id.clone(),
            author_id: options.author_id.clone(),
            lower,
            upper,
            before: options.before,
            limit: options.limit,
            scan: options.scan,
            window: self.scan_window,
        })
    }
}
