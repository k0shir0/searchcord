//! Rust drives the W3C WebDriver protocol against both unchanged frontends.
use reqwest::{Method, blocking::Client};
use searchcord_parity::Harness;
use serde_json::{Value, json};
use std::{
    net::TcpListener,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct Browser {
    driver: Child,
    client: Client,
    origin: String,
    session: String,
    _directory: TempDir,
}
impl Browser {
    fn new() -> Self {
        let executable=std::env::var_os("SEARCHCORD_WEBDRIVER").expect("Set SEARCHCORD_WEBDRIVER to ChromeDriver installed by driver/install_webdriver.py. Browser gates never silently skip.");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let directory = tempfile::tempdir().unwrap();
        let driver = Command::new(executable)
            .arg(format!("--port={port}"))
            .arg("--allowed-ips=127.0.0.1")
            .arg(format!(
                "--log-path={}",
                directory.path().join("webdriver.log").display()
            ))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start ChromeDriver");
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        let origin = format!("http://127.0.0.1:{port}");
        let mut browser = Self {
            driver,
            client,
            origin,
            session: String::new(),
            _directory: directory,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if browser
                .client
                .get(format!("{}/status", browser.origin))
                .send()
                .is_ok()
            {
                break;
            }
            assert!(Instant::now() < deadline, "ChromeDriver failed to start");
            thread::sleep(Duration::from_millis(20));
        }
        let binary = std::env::var("SEARCHCORD_CHROME_BINARY")
            .expect("Set SEARCHCORD_CHROME_BINARY to pinned Chrome");
        let args = vec![
            "--headless=new".to_owned(),
            "--no-sandbox".into(),
            "--disable-dev-shm-usage".into(),
            "--disable-background-networking".into(),
            "--disable-component-update".into(),
            "--disable-sync".into(),
            "--no-first-run".into(),
            "--disable-default-apps".into(),
            "--window-size=1400,1100".into(),
            "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1, EXCLUDE localhost".into(),
            "--proxy-server=direct://".into(),
            "--force-prefers-reduced-motion".into(),
            format!(
                "--user-data-dir={}",
                browser._directory.path().join("profile").display()
            ),
        ];
        let result=browser.raw(Method::POST,"/session",json!({"capabilities":{"alwaysMatch":{"browserName":"chrome",
            "goog:chromeOptions":{"binary":binary,"args":args},"goog:loggingPrefs":{"browser":"ALL"}}}}));
        browser.session = result["sessionId"].as_str().unwrap().to_owned();
        browser
    }
    fn raw(&self, method: Method, path: &str, body: Value) -> Value {
        let response = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .json(&body)
            .send()
            .unwrap();
        let status = response.status();
        let value: Value = response.json().unwrap();
        assert!(status.is_success(), "WebDriver {path}: {value}");
        value["value"].clone()
    }
    fn command(&self, path: &str, body: Value) -> Value {
        self.raw(
            Method::POST,
            &format!("/session/{}{path}", self.session),
            body,
        )
    }
    fn navigate(&self, url: &str) {
        assert!(url.starts_with("http://127.0.0.1:"));
        self.command("/url", json!({"url":url}));
    }
    fn js(&self, script: &str) -> Value {
        self.command("/execute/sync", json!({"script":script,"args":[]}))
    }
    fn wait(&self, script: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.js(script) == true {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "browser condition failed: {script}; DOM: {}",
                self.js("return document.body.innerText")
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn click(&self, selector: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let element =
                self.command("/element", json!({"using":"css selector","value":selector}));
            let id = element["element-6066-11e4-a52e-4f735466cecf"]
                .as_str()
                .unwrap();
            let response = self
                .client
                .post(format!(
                    "{}/session/{}/element/{id}/click",
                    self.origin, self.session
                ))
                .json(&json!({}))
                .send()
                .unwrap();
            let status = response.status();
            let value: Value = response.json().unwrap();
            if status.is_success() {
                return;
            }
            let error = value["value"]["error"].as_str().unwrap_or("");
            assert!(
                Instant::now() < deadline
                    && [
                        "element not interactable",
                        "element click intercepted",
                        "stale element reference"
                    ]
                    .contains(&error),
                "click {selector}: {value}"
            );
            thread::sleep(Duration::from_millis(30));
        }
    }
    fn assert_no_script_errors(&self) {
        let logs = self.command("/se/log", json!({"type":"browser"}));
        let errors: Vec<_> = logs
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| l["source"] == "javascript" && l["level"] == "SEVERE")
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
    }
}
impl Drop for Browser {
    fn drop(&mut self) {
        if !self.session.is_empty() {
            let _ = self
                .client
                .delete(format!("{}/session/{}", self.origin, self.session))
                .send();
        }
        let _ = self.driver.kill();
        let _ = self.driver.wait();
    }
}

fn fixture(viewer: bool) -> Harness {
    let mut h = if viewer {
        Harness::new_viewer_fixture()
    } else {
        Harness::new_collector_fixture()
    }
    .unwrap();
    h.init();
    let mut db = h.database();
    let tx = db.transaction().unwrap();
    tx.execute_batch("INSERT INTO guilds VALUES ('10','Fixture Server'); INSERT INTO channels VALUES ('20','10','general');
        INSERT INTO authors(id,name) VALUES ('30','Fixture Author');").unwrap();
    for n in 0..81 {
        tx.execute(
            "INSERT INTO messages(id,channel_id,guild_id,author_id,content) VALUES (?,20,10,30,?)",
            rusqlite::params![
                1_450_000_000_000_000_000_i64 + n,
                if n == 80 {
                    "rare <script>window.parityXss=true</script>"
                } else {
                    "common fixture text"
                }
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    drop(db);
    h
}

#[test]
fn unchanged_viewer_search_pagination_profiles_filters_and_privacy_work_in_browser() {
    let mut h = fixture(true);
    h.start_viewer("synthetic.db", true, None);
    let browser = Browser::new();
    browser.navigate(&format!("{}/", h.origin()));
    browser.wait("return document.querySelector('#messageTotal').textContent==='81'");
    browser.js("document.querySelector('#searchInput').value='common'; document.querySelector('#searchForm').requestSubmit(); return true;");
    browser.wait("return document.querySelectorAll('#messages .result-row').length===50 && document.querySelector('#resultBody').getAttribute('aria-busy')==='false'");
    assert_eq!(
        browser.js("return document.querySelector('#pageNumber').textContent"),
        "Page 1"
    );
    browser.click("#nextPage");
    browser.wait("return document.querySelector('#pageNumber').textContent==='Page 2' && document.querySelector('#resultBody').getAttribute('aria-busy')==='false'");
    assert_eq!(
        browser.js("return document.querySelectorAll('#messages .result-row').length"),
        30
    );
    browser.click("#previousPage");
    browser.wait("return document.querySelector('#pageNumber').textContent==='Page 1'");
    browser.click("#searchInput");
    browser.js("const input=document.querySelector('#author'); input.value='30'; input.dispatchEvent(new Event('change',{bubbles:true})); return true;");
    browser.wait("return new URL(location.href).searchParams.get('author_id')==='30' && document.querySelector('#resultBody').getAttribute('aria-busy')==='false'");
    browser.click("#resultsTitle");
    browser.wait("return document.querySelector('#filterTray').inert");
    browser.click("#messages .message-author");
    browser.wait("return !document.querySelector('#profileSection').hidden && document.querySelector('#profileHost').textContent.includes('Fixture Author')");
    assert_eq!(
        browser.js("return new URL(location.href).searchParams.get('author_id')"),
        "30"
    );
    browser.click("#closeProfile");
    browser.js("document.querySelector('#searchInput').value='rare'; document.querySelector('#searchForm').requestSubmit(); return true;");
    browser.wait("return document.querySelectorAll('#messages .result-row').length===1 && document.querySelector('#messages').textContent.includes('<script>')");
    assert_eq!(browser.js("return window.parityXss === undefined"), true);
    browser.js("document.querySelector('#searchInput').value='no matching text'; document.querySelector('#searchForm').requestSubmit(); return true;");
    browser.wait("return !document.querySelector('#emptyState').hidden");
    browser.navigate(&format!("{}/privacy.html", h.origin()));
    browser.wait("return document.body.textContent.toLowerCase().includes('privacy')");
    browser.assert_no_script_errors();
}

#[test]
fn unchanged_collector_search_statistics_settings_and_queue_render_in_browser() {
    let mut h = fixture(false);
    h.command(json!({"op":"job","action":"configure"}));
    h.start_collector();
    let browser = Browser::new();
    browser.navigate(&format!("{}/", h.origin()));
    browser.wait("return document.querySelector('#homeMessages').textContent==='81'");
    browser.js("document.querySelector('#searchInput').value='common'; document.querySelector('#queryForm').requestSubmit(); return true;");
    browser.wait("return document.querySelector('#searchResults').textContent.includes('common fixture text')");
    assert_eq!(
        browser.js("return document.querySelectorAll('#searchResults .result-row').length"),
        50
    );
    browser.click(".tab[data-view='stats']");
    browser.wait("return document.querySelector('#sTotal').textContent==='81' && document.querySelector('#leaderboard').textContent.includes('Fixture Author')");
    browser.click("#menuBtn");
    browser.wait("return document.querySelector('#settingsPanel').getAttribute('aria-modal')==='true' && !document.querySelector('#settingsPanel').inert");
    assert_eq!(
        browser.js("return document.querySelector('#savedTokenSelect').disabled"),
        true
    );
    browser.click("#closeSettings");
    browser.click(".tab[data-view='scrape']");
    browser.wait("return document.querySelector('#view-scrape').classList.contains('active')");
    assert_eq!(
        browser.js("return document.querySelector('#startScrape').disabled"),
        true
    );
    assert_eq!(
        browser.js("return document.querySelector('#queueCount').textContent"),
        "0"
    );
    browser.assert_no_script_errors();
}

#[derive(Default)]
struct UpstreamTrace(std::sync::Mutex<Vec<Value>>);
async fn native_browser_upstream(
    axum::extract::State(trace): axum::extract::State<std::sync::Arc<UpstreamTrace>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let path = request.uri().path().strip_prefix("/api/v10").unwrap_or("");
    let query = request.uri().query().unwrap_or("");
    let token = request
        .headers()
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap();
    let beta = token == "synthetic-beta";
    trace
        .0
        .lock()
        .unwrap()
        .push(json!({"method":request.method().as_str(),"path":path,"query":query,"token":token}));
    let message = |n: i64, author: &str| json!({"id":(searchcord_parity::performance::BASE+n).to_string(),"channel_id":"20","author":{"id":author,"username":"Fixture Author"},"content":format!("Browser message {n}"),"attachments":[]});
    let payload = match (request.method().as_str(), path) {
        ("GET", "/users/@me") => {
            json!({"id":if beta {"32"}else{"30"},"username":if beta {"Beta"}else{"Alpha"}})
        }
        ("GET", "/users/@me/guilds") => json!([{"id":"10","name":"Fixture Server","owner":true}]),
        ("GET", "/users/@me/channels") => {
            json!([{"id":"21","type":1,"recipients":[{"id":"31","username":"Fixture Other"}]}])
        }
        ("GET", "/guilds/10") => {
            json!({"id":"10","name":"Fixture Server","owner_id":"30","roles":[{"id":"10","permissions":"3072"}]})
        }
        ("GET", "/guilds/10/members/30") => json!({"user":{"id":"30"},"roles":[]}),
        ("GET", "/guilds/10/channels") => {
            json!([{"id":"20","guild_id":"10","name":"general","type":0,"permission_overwrites":[]}])
        }
        ("GET", "/guilds/10/threads/active") => json!({"threads":[],"members":[],"has_more":false}),
        ("GET", "/channels/20/messages")
            if query.contains("before=") || query.contains("after=") =>
        {
            json!([])
        }
        ("GET", "/channels/20/messages") if query.contains("limit=100") => {
            // Let the real UI pause during the fetch, then require its accepted
            // page save to finish before reporting a paused checkpoint.
            tokio::time::sleep(Duration::from_millis(1200)).await;
            json!(
                (5000..5100)
                    .rev()
                    .map(|n| message(n, "30"))
                    .collect::<Vec<_>>()
            )
        }
        ("GET", "/channels/20/messages") => json!([message(5099, "30")]),
        ("GET", "/channels/21/messages") => json!([
            {"id":(searchcord_parity::performance::BASE+1001).to_string(),"channel_id":"21","author":{"id":"30"},"content":"answer","attachments":[]},
            {"id":(searchcord_parity::performance::BASE+1000).to_string(),"channel_id":"21","author":{"id":"31"},"content":"question","attachments":[]}
        ]),
        ("GET", "/invites/browser-fixture") => json!({"guild":{"id":"10","name":"Fixture Server"}}),
        ("POST", "/invites/browser-fixture") => {
            json!({"guild":{"id":"10","name":"Fixture Server"}})
        }
        ("DELETE", p)
            if p == format!(
                "/channels/21/messages/{}",
                searchcord_parity::performance::BASE + 1001
            ) =>
        {
            return axum::http::StatusCode::NO_CONTENT.into_response();
        }
        ("GET", p) if p.ends_with("/profile") => {
            json!({"user":{"id":"30","username":"Fixture Author"},"user_profile":{"bio":"Saved fixture"}})
        }
        _ => {
            return (
                axum::http::StatusCode::NOT_IMPLEMENTED,
                axum::Json(json!({"unexpected":path})),
            )
                .into_response();
        }
    };
    axum::Json(payload).into_response()
}

#[test]
fn native_collector_account_queue_recovery_invites_monitor_cleanup_and_export_work_in_browser() {
    use searchcord_collector::{
        accounts::Accounts,
        credentials::Paths,
        discord::Discord,
        http::{App, router},
        media_collection::MediaCollector,
        pacing::{Gate, SystemClock},
        writer::Writer,
    };
    use std::sync::Arc;
    let h = fixture(false);
    {
        let db = h.database();
        db.execute_batch("INSERT INTO channels VALUES ('21',NULL,'Fixture DM'); INSERT INTO authors(id,name) VALUES ('31','Fixture Other');").unwrap();
        for (offset, author, content) in [(1000, "31", "question"), (1001, "30", "answer")] {
            db.execute(
                "INSERT INTO messages(id,channel_id,author_id,content) VALUES (?,21,?,?)",
                rusqlite::params![
                    searchcord_parity::performance::BASE + offset,
                    author,
                    content
                ],
            )
            .unwrap();
        }
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (app, trace, upstream, server, origin) = runtime.block_on(async {
        let trace = Arc::new(UpstreamTrace::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let fixture = axum::Router::new()
            .fallback(native_browser_upstream)
            .with_state(trace.clone());
        let upstream = tokio::spawn(async move { axum::serve(listener, fixture).await.unwrap() });
        let clock = SystemClock::default();
        let writer = Writer::open(h.archive()).await.unwrap();
        let gate = Gate::open(h.archive(), clock.clone()).await.unwrap();
        let discord = Discord::for_loopback(gate, &origin).unwrap();
        let media = MediaCollector::for_loopback(h.archive(), &origin, Default::default()).unwrap();
        let mut app =
            App::open_with_media(h.archive(), discord, Arc::new(clock), writer.clone(), media)
                .await
                .unwrap();
        let paths = Paths::new(
            &h.archive(),
            &h.root().join("credentials/browser.vault"),
            &h.root().join("credentials/browser.key"),
        )
        .unwrap();
        let accounts = Accounts::with_paths(paths, writer);
        app.scrapes = app.scrapes.clone().with_accounts(accounts.clone());
        app.monitor = app.monitor.clone().with_accounts(accounts.clone());
        app.accounts = accounts;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let http = router(app.clone());
        let server = tokio::spawn(async move { axum::serve(listener, http).await.unwrap() });
        (app, trace, upstream, server, origin)
    });
    let browser = Browser::new();
    browser.navigate(&origin);
    browser.wait("return document.querySelector('#homeMessages').textContent==='83'");
    browser.click("#menuBtn");
    for (label, token) in [("Alpha", "synthetic-alpha"), ("Beta", "synthetic-beta")] {
        browser.js(&format!("document.querySelector('#tokenName').value={};document.querySelector('#tokenInput').value={};return true",json!(label),json!(token)));
        browser.click("#saveToken");
        browser.wait(&format!("return document.querySelector('#tokenStatus').textContent.includes({}) && !document.querySelector('#saveToken').disabled",json!(format!("connected as {label}"))));
    }
    assert_eq!(
        browser.js("return document.querySelector('#savedTokenSelect').options.length"),
        2
    );
    browser.js("const s=document.querySelector('#savedTokenSelect');s.selectedIndex=0;s.dispatchEvent(new Event('change',{bubbles:true}));return true");
    browser.wait("return document.querySelector('#tokenStatus').textContent.includes('connected as Alpha') && !document.querySelector('#savedTokenSelect').disabled");
    browser.click("#closeSettings");
    browser.click(".tab[data-view='scrape']");
    browser.wait("return document.querySelector('#serverList .server-item')!==null");
    browser.click("#serverList .server-item");
    browser.wait("return document.querySelector('#cpBody [data-id=\"20\"]')!==null");
    browser.click("#cpBody .ch-live-btn");
    browser
        .wait("return document.querySelector('#cpBody .ch-live-btn').textContent==='stop monitor'");
    browser.click("#cpBody .ch-live-btn");
    browser.wait("return document.querySelector('#cpBody .ch-live-btn').textContent==='monitor'");

    browser.click("#cpBody [aria-label='Toggle scrape queue']");
    browser.click("#startScrape");
    browser.wait("return !document.querySelector('#pauseQueueScrape').disabled");
    browser.click("#pauseQueueScrape");
    browser
        .wait("return document.querySelector('#queueProgressTitle').textContent==='Scrape paused'");
    browser.navigate(&origin);
    browser.click(".tab[data-view='scrape']");
    browser.wait("return document.querySelector('#queueProgressTitle').textContent==='Scrape paused' && document.querySelector('#pauseQueueScrape').textContent==='resume scraping'");
    browser.click("#pauseQueueScrape");
    browser.wait("return !document.querySelector('#dismissQueueProgress').hidden");
    assert_eq!(
        h.database()
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE id>=?",
                [searchcord_parity::performance::BASE + 5000],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        100
    );

    browser.js("document.querySelector('#inviteInput').value='https://discord.gg/browser-fixture browser-fixture';document.querySelector('#inviteForm').requestSubmit();return true");
    browser.wait("return document.querySelector('#inviteResults strong')?.textContent==='joined' && !document.querySelector('#startInvites').disabled");
    assert_eq!(
        browser.js("return document.querySelectorAll('#inviteResults li').length"),
        1
    );
    browser.click("#sourceDms");
    browser.wait("return document.querySelector('#dmBody [data-id=\"21\"]')!==null");
    browser.click("#dmBody [aria-label='Export to ChatML JSONL']");
    browser.wait("return document.querySelector('#exportParticipants .persona-item')!==null");
    browser.click("#exportParticipants .persona-item[data-id='30']");
    browser.js("const original=window.fetch;window.fetch=async(...args)=>{const response=await original(...args);if(String(args[0]).includes('/api/export/chatml'))window.parityExport=await response.clone().text();return response;};return true");
    browser.click("#runExportBtn");
    browser.wait("return typeof window.parityExport==='string' && !document.querySelector('#exportModalBg').classList.contains('visible')");
    let export = browser.js("return window.parityExport");
    let line: Value =
        serde_json::from_str(export.as_str().unwrap().lines().next().unwrap()).unwrap();
    assert_eq!(
        line["messages"][1],
        json!({"role":"user","content":"Fixture Other: question"})
    );
    assert_eq!(
        line["messages"][2],
        json!({"role":"assistant","content":"answer"})
    );
    browser.click("#dmBody .ch-clear-btn");
    browser.click("#dmBody .ch-clear-btn");
    browser.wait("return document.querySelector('#moStats').textContent==='1 deleted · 0 failed · 0 already missing' && document.querySelector('#modalFoot').classList.contains('visible')");
    assert_eq!(
        h.database()
            .query_row(
                "SELECT COUNT(*) FROM message_payloads WHERE message_id=? AND deleted_at IS NOT NULL",
                [searchcord_parity::performance::BASE + 1000],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        h.database()
            .query_row(
                "SELECT COUNT(*) FROM message_payloads WHERE message_id=? AND deleted_at IS NOT NULL",
                [searchcord_parity::performance::BASE + 1001],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
    browser.assert_no_script_errors();
    let requests = trace.0.lock().unwrap().clone();
    assert!(requests.iter().any(|r| r["token"] == "synthetic-beta"));
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "POST" && r["path"] == "/invites/browser-fixture")
            .count(),
        1
    );
    assert_eq!(
        requests.iter().filter(|r| r["method"] == "DELETE").count(),
        1
    );
    drop(browser);
    runtime.block_on(async {
        app.shutdown().await.unwrap();
        app.writer.shutdown().await.unwrap();
        upstream.abort();
        server.abort();
    });
}
