//! Native launch and readiness checks never configure an account or contact Discord.
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
struct Running(Child);
fn collector_binary() -> std::path::PathBuf {
    std::env::var_os("SEARCHCORD_NATIVE_COLLECTOR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_searchcord").into())
}
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn native_collector_runs_without_python_from_spaced_application_path() {
    let dir = tempfile::Builder::new()
        .prefix("searchcord native launch ")
        .tempdir()
        .unwrap();
    let executable = dir.path().join(if cfg!(windows) {
        "searchcord.exe"
    } else {
        "searchcord"
    });
    std::fs::copy(collector_binary(), &executable).unwrap();
    let child = Command::new(&executable)
        .args([
            "--no-browser",
            "--data-dir",
            "relative archive",
            "--host",
            "127.0.0.1",
            "--port",
            "0",
        ])
        .current_dir(std::env::temp_dir())
        .env("PATH", "")
        .env("NO_COLOR", "1")
        .env(
            "SEARCHCORD_CREDENTIAL_FILE",
            dir.path().join("credentials/native.vault"),
        )
        .env(
            "SEARCHCORD_CREDENTIAL_KEY_FILE",
            dir.path().join("credentials/native.key"),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut child = Running(child);
    let stdout = child.0.stdout.take().unwrap();
    let (sent, received) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            if sent.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let url = loop {
        assert!(
            Instant::now() < deadline,
            "native collector did not become ready"
        );
        let line = received.recv_timeout(Duration::from_secs(15)).unwrap();
        assert!(!line.contains('\x1b'), "NO_COLOR must produce plain output");
        if let Some((_, url)) = line.split_once("Local database ready. Searchcord: ") {
            break url.to_owned();
        }
    };
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let health = client.get(format!("{url}/api/health")).send().unwrap();
    assert_eq!(health.status(), 200);
    assert_eq!(
        health.headers()["x-searchcord-instance"]
            .to_str()
            .unwrap()
            .len(),
        32
    );
    assert_eq!(health.text().unwrap(), "searchcord-ready");
    assert_eq!(
        client
            .get(format!("{url}/api/settings"))
            .send()
            .unwrap()
            .json::<serde_json::Value>()
            .unwrap(),
        serde_json::json!({"tokens":[],"active_token_id":null,"token_set":false})
    );
    assert!(dir.path().join("relative archive/searchcord.db").is_file());
    assert!(!dir.path().join("credentials/native.vault").exists());
    #[cfg(unix)]
    {
        // Keep a real SSE body open while the executable handles SIGTERM. Its
        // shutdown must end streams, join storage and exit successfully.
        let stream = client.get(format!("{url}/api/live/events")).send().unwrap();
        assert_eq!(stream.status(), 200);
        // SAFETY: the live child PID is valid and kill does not borrow memory.
        assert_eq!(
            unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success(), "native shutdown failed: {status}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "native shutdown did not end its active stream"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(stream);
    }
    let _ = child.0.kill();
    let _ = child.0.wait();
    drop(received);
    reader.join().unwrap();
}
#[test]
fn native_help_and_invalid_options_do_not_create_archive_or_start_browser() {
    let dir = tempfile::tempdir().unwrap();
    for (args, success) in [
        (vec!["--help"], true),
        (vec!["--unknown"], false),
        (vec!["--port", "invalid"], false),
    ] {
        let output = Command::new(collector_binary())
            .args(args)
            .env("SEARCHCORD_DATA_DIR", dir.path())
            .output()
            .unwrap();
        assert_eq!(output.status.success(), success);
        if success {
            let help = String::from_utf8(output.stdout).unwrap();
            assert!(help.contains("Usage: searchcord"));
            assert!(help.contains("--no-browser"));
        }
    }
    assert!(!dir.path().join("searchcord.db").exists());
}

#[cfg(windows)]
#[test]
fn existing_windows_launchers_find_native_binaries_with_spaces_without_python() {
    let dir = tempfile::Builder::new()
        .prefix("searchcord launcher path ")
        .tempdir()
        .unwrap();
    let collector = collector_binary();
    for (launcher, source, binary) in [
        (
            "start.bat",
            include_str!("../../../deploy/native/start.bat"),
            "searchcord.exe",
        ),
        (
            "start-search.bat",
            include_str!("../../../deploy/native/start-search.bat"),
            "searchcord-search.exe",
        ),
    ] {
        std::fs::write(dir.path().join(launcher), source).unwrap();
        let source = if binary == "searchcord.exe" {
            collector.to_owned()
        } else {
            collector.with_file_name(binary)
        };
        std::fs::copy(source, dir.path().join(binary)).unwrap();
        let output = Command::new(std::env::var_os("COMSPEC").unwrap_or("cmd.exe".into()))
            .args(["/d", "/c", launcher, "--help"])
            .current_dir(dir.path())
            .env("PATH", "")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: searchcord"));
    }
    assert!(!dir.path().join("data").exists());
}
