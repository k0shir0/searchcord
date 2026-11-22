use std::{fs, process::Command};
#[cfg(unix)]
#[test]
fn interrupt_cleans_owned_partial_build() {
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("source.db");
    source(&input);
    rusqlite::Connection::open(&input).unwrap().execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000) INSERT INTO messages SELECT x,1,1,1 FROM n;").unwrap();
    let dest = root.path().join("output.db");
    let partial = root.path().join("output.db.building");
    let mut child = Command::new(env!("CARGO_BIN_EXE_searchcord-snapshot"))
        .arg(&input)
        .arg(&dest)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !partial.exists() {
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(child.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(child.wait().unwrap().code(), Some(130));
    assert!(!dest.exists());
    assert!(!partial.exists());
}
fn source(path: &std::path::Path) {
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch("PRAGMA user_version=10;
    CREATE TABLE messages(id INTEGER PRIMARY KEY,channel_id INTEGER,guild_id INTEGER,author_id INTEGER); CREATE VIEW message_records AS SELECT id, NULL channel_id,NULL guild_id,NULL author_id,NULL content,NULL timestamp,NULL image_urls,NULL channel_name,NULL guild_name,NULL author_name FROM messages;
    CREATE TABLE guilds(id TEXT PRIMARY KEY,name TEXT); CREATE TABLE channels(id TEXT PRIMARY KEY,guild_id TEXT,name TEXT);
    CREATE TABLE authors(id TEXT PRIMARY KEY,name TEXT); CREATE TABLE profiles(user_id TEXT PRIMARY KEY,username TEXT,avatar_hash TEXT,banner_hash TEXT);
    CREATE TABLE profile_details(user_id TEXT PRIMARY KEY,payload TEXT,fetched_at TEXT); CREATE TABLE stats_counts(kind TEXT,key TEXT,count INTEGER); CREATE TABLE name_values(value TEXT);").unwrap();
}
#[test]
fn native_cli_preserves_flags_paths_reports_and_refusals() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("source archive.db");
    source(&input);
    let dest = root.path().join("new output/snapshot.db");
    let report = root.path().join("report.json");
    let output = Command::new(env!("CARGO_BIN_EXE_searchcord-snapshot"))
        .arg(&input)
        .arg(&dest)
        .args(["--block-rows", "16", "--with-media", "--report"])
        .arg(&report)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["messages"], 0);
    assert_eq!(
        value["media"],
        serde_json::json!({"copied":0,"unavailable":0,"bytes":0})
    );
    assert_eq!(value["integrity"], "ok");
    let bytes = fs::read(&report).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        value
    );
    for report_path in [&report, &input, &dest] {
        let fresh = root.path().join("fresh.db");
        let output = Command::new(env!("CARGO_BIN_EXE_searchcord-snapshot"))
            .arg(&input)
            .arg(&fresh)
            .arg("--report")
            .arg(report_path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!fresh.exists());
    }
    let fresh = root.path().join("new output/other.db");
    let output = Command::new(env!("CARGO_BIN_EXE_searchcord-snapshot"))
        .arg(&input)
        .arg(&fresh)
        .arg("--with-media")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!fresh.exists());
    assert_eq!(fs::read(report).unwrap(), bytes);
    let fresh = root.path().join("fresh.db");
    let alias = root.path().join("missing/../fresh.db");
    let output = Command::new(env!("CARGO_BIN_EXE_searchcord-snapshot"))
        .arg(&input)
        .arg(&fresh)
        .arg("--report")
        .arg(alias)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!fresh.exists());
}
