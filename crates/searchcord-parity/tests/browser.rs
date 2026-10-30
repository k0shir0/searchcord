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
        let executable=std::env::var_os("SEARCHCORD_WEBDRIVER").expect("Set SEARCHCORD_WEBDRIVER to pinned ChromeDriver; see docs/rust-benchmarks.md. Browser gates never silently skip.");
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

fn fixture() -> Harness {
    let mut h = Harness::new().unwrap();
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
    let mut h = fixture();
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
    let mut h = fixture();
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
