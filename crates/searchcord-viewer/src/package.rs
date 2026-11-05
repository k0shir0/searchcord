//! Fresh native deployment folders. No working-tree traversal or data copying.
use std::{
    fs, io,
    path::{Path, PathBuf},
};
pub const ASSETS: &[&str] = &[
    "index.html",
    "search.css",
    "search.js",
    "privacy.html",
    "privacy.css",
    "bauhaus.css",
    "profile-view.css",
    "profile-view.js",
];
struct Cleanup(PathBuf, bool);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.1 {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
pub fn build(destination: &Path, viewer: &Path) -> io::Result<PathBuf> {
    if !viewer.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Native viewer binary not found",
        ));
    }
    fs::create_dir(destination)?;
    let mut cleanup = Cleanup(destination.to_path_buf(), true);
    let binary = destination.join(format!("searchcord-search{}", std::env::consts::EXE_SUFFIX));
    fs::copy(viewer, &binary)?;
    for name in ASSETS {
        let (bytes, _) = crate::asset(&format!("/{name}")).expect("approved asset");
        let relative = if ["bauhaus.css", "profile-view.css", "profile-view.js"].contains(name) {
            format!("static/{name}")
        } else {
            format!("static/search/{name}")
        };
        let path = destination.join(relative);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, bytes)?;
    }
    for (name, bytes) in [
        ("LICENSE", include_bytes!("../../../LICENSE").as_slice()),
        (
            "README.txt",
            include_bytes!("../../../deploy/search/RUNNING.txt").as_slice(),
        ),
        (
            "THIRD_PARTY_NOTICES.md",
            include_bytes!("../../../deploy/search/THIRD_PARTY_NOTICES.md").as_slice(),
        ),
        (
            "Dockerfile",
            include_bytes!("../../../deploy/search/Dockerfile.native").as_slice(),
        ),
        (
            ".dockerignore",
            include_bytes!("../../../deploy/search/Dockerfile.native.dockerignore").as_slice(),
        ),
        (
            "start-search.bat",
            b"@echo off\r\ncd /d \"%~dp0\"\r\n\"%~dp0searchcord-search.exe\" %*\r\n".as_slice(),
        ),
    ] {
        fs::write(destination.join(name), bytes)?;
    }
    fs::create_dir(destination.join("data"))?;
    cleanup.1 = false;
    destination.canonicalize()
}
