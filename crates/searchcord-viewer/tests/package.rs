struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::Duration,
};
#[test]
fn standalone_folder_is_fresh_has_only_approved_files_and_runs_without_python() {
    let root = tempfile::tempdir().unwrap();
    let help = Command::new(env!("CARGO_BIN_EXE_searchcord-package-search"))
        .arg("--help")
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: searchcord-package-search"));
    assert!(!root.path().join("--help").exists());
    let folder = root.path().join("native viewer with spaces");
    let output = Command::new(env!("CARGO_BIN_EXE_searchcord-package-search"))
        .arg(&folder)
        .arg("--viewer")
        .arg(env!("CARGO_BIN_EXE_searchcord-search"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut files = Vec::new();
    fn walk(path: &std::path::Path, files: &mut Vec<String>, root: &std::path::Path) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, files, root);
            } else {
                files.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    walk(&folder, &mut files, &folder);
    files.sort();
    let mut expected = vec![
        format!("searchcord-search{}", std::env::consts::EXE_SUFFIX),
        "LICENSE".into(),
        "README.txt".into(),
        "THIRD_PARTY_NOTICES.md".into(),
        "Dockerfile".into(),
        ".dockerignore".into(),
        "start-search.bat".into(),
    ];
    for name in searchcord_viewer::package::ASSETS {
        expected.push(
            if ["bauhaus.css", "profile-view.css", "profile-view.js"].contains(name) {
                format!("static/{name}")
            } else {
                format!("static/search/{name}")
            },
        );
    }
    expected.sort();
    assert_eq!(files, expected);
    let binary = folder.join(format!("searchcord-search{}", std::env::consts::EXE_SUFFIX));
    let mut child = ChildGuard(
        Command::new(binary)
            .current_dir(root.path())
            .args(["--port=0", "--db=missing archive.db", "--snapshot"])
            .env("PATH", "")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stdout = BufReader::new(child.0.stdout.take().unwrap());
    let origin = loop {
        let mut line = String::new();
        assert!(
            stdout.read_line(&mut line).unwrap() > 0,
            "native package exited before readiness"
        );
        if let Some(origin) = line.trim().strip_prefix("Searchcord: ") {
            break origin.to_owned();
        }
    };
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let index = client.get(&origin).send().unwrap();
    assert_eq!(index.status().as_u16(), 200);
    assert!(index.text().unwrap().contains("<title>searchcord</title>"));
    assert_eq!(
        client
            .get(format!("{origin}/api/summary"))
            .send()
            .unwrap()
            .status()
            .as_u16(),
        503
    );
    assert_eq!(
        client
            .get(format!("{origin}/api/settings"))
            .send()
            .unwrap()
            .status()
            .as_u16(),
        404
    );
    let _ = child.0.kill();
    child.0.wait().unwrap();
    assert!(!folder.join("missing archive.db").exists());
    assert!(
        !Command::new(env!("CARGO_BIN_EXE_searchcord-package-search"))
            .arg(&folder)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        fs::read_to_string(folder.join("start-search.bat")).unwrap(),
        "@echo off\ncd /d \"%~dp0\"\n\"%~dp0searchcord-search.exe\" %*\n".replace('\n', "\r\n")
    );
    let other = root.path().join("failed");
    assert!(
        searchcord_viewer::package::build(&other, &root.path().join("missing binary")).is_err()
    );
    assert!(!other.exists());
}
