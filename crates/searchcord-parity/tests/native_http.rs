use searchcord_parity::{Backend, Harness};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
fn viewer(backend: Backend) -> Harness {
    let mut h = Harness::viewer_with_backend(backend).unwrap();
    h.init();
    h.start_viewer("synthetic.db", true, None);
    h
}
#[test]
fn native_http_validation_assets_methods_and_media_match_reference() {
    let mut python = viewer(Backend::Python);
    let mut native = viewer(Backend::Rust);
    for path in [
        "/api/search",
        "/api/summary",
        "/api/search/",
        "/api/suggestions/x",
        "/api/profiles/nope",
        "/api/profiles/1/servers",
        "/api/settings",
        "/api/media/invalid",
        "/docs",
        "/openapi.json",
    ] {
        for method in ["GET", "HEAD", "POST", "OPTIONS"] {
            let p = python.request(method, path, &[]);
            let r = native.request(method, path, &[]);
            assert_eq!(r.status, p.status, "{method} {path}");
            if method != "HEAD" {
                let mut a = r.json();
                let mut b = p.json();
                if let Some(o) = a.as_object_mut() {
                    o.remove("elapsed_ms");
                }
                if let Some(o) = b.as_object_mut() {
                    o.remove("elapsed_ms");
                }
                assert_eq!(a, b, "{method} {path}");
            }
        }
    }
    for (path, params) in [
        (
            "/api/search",
            vec![("limit", "101"), ("before", "0"), ("scan", "bad")],
        ),
        (
            "/api/search",
            vec![("before", "no"), ("q", &"a".repeat(201))],
        ),
        ("/api/search", vec![("limit", "2.000"), ("scan", "YES")]),
        (
            "/api/profiles/1/servers",
            vec![("q", &"a".repeat(101)), ("offset", "-1"), ("limit", "0")],
        ),
        ("/api/suggestions/invalid", vec![("q", &"a".repeat(101))]),
    ] {
        let params: Vec<_> = params.into_iter().map(|(k, v)| (k, v.to_owned())).collect();
        let p = python.get(path, &params);
        let r = native.get(path, &params);
        let mut a = r.json();
        let mut b = p.json();
        if let Some(o) = a.as_object_mut() {
            o.remove("elapsed_ms");
        }
        if let Some(o) = b.as_object_mut() {
            o.remove("elapsed_ms");
        }
        assert_eq!((r.status, a), (p.status, b), "{path} {params:?}");
    }
    for path in [
        "/",
        "/index.html",
        "/search.css",
        "/search.js",
        "/privacy.html",
        "/privacy.css",
        "/bauhaus.css",
        "/profile-view.js",
        "/profile-view.css",
    ] {
        let p = python.get(path, &[]);
        let r = native.get(path, &[]);
        assert_eq!(r.status, 200);
        assert_eq!(r.body, p.body);
        for header in [
            "content-type",
            "cache-control",
            "x-content-type-options",
            "referrer-policy",
            "content-security-policy",
        ] {
            assert_eq!(r.headers[header], p.headers[header], "{path} {header}");
        }
    }
    let key = "a".repeat(64);
    let bytes = b"archived file";
    for h in [&python, &native] {
        let dir = h.root().join("archive/media");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join(format!("{key}.bin")), bytes).unwrap();
        fs::write(dir.join(format!("{key}.json")),json!({"status":"saved","bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes)),"content_type":"application/pdf"}).to_string()).unwrap();
    }
    let path = format!("/api/media/{key}");
    let p = python.get(&path, &[]);
    let r = native.get(&path, &[]);
    assert_eq!(r.body, "archived file");
    assert_eq!(r.status, 200);
    for key in [
        "content-type",
        "content-disposition",
        "cache-control",
        "content-security-policy",
        "accept-ranges",
    ] {
        assert_eq!(r.headers.get(key), p.headers.get(key), "{key}");
    }
    fs::write(
        native.root().join(format!("archive/media/{key}.bin")),
        b"corrupted!!!!",
    )
    .unwrap();
    assert_eq!(native.get(&path, &[]).status, 404);
}

#[test]
fn native_http_gzip_ranges_conditional_assets_and_media_preserve_bytes() {
    use std::io::Read;
    let mut python = viewer(Backend::Python);
    let mut native = viewer(Backend::Rust);
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    for h in [&mut python, &mut native] {
        let root = format!("{}/", h.origin());
        let response = client.get(&root).send().unwrap();
        let etag = response.headers()["etag"].to_str().unwrap().to_owned();
        let original = response.bytes().unwrap();
        let compressed = client
            .get(&root)
            .header("accept-encoding", "gzip")
            .send()
            .unwrap();
        assert_eq!(compressed.headers()["content-encoding"], "gzip");
        assert_eq!(compressed.headers()["vary"], "Accept-Encoding");
        let bytes = compressed.bytes().unwrap();
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_ref())
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, original);
        assert_eq!(
            client
                .get(&root)
                .header("if-none-match", &etag)
                .send()
                .unwrap()
                .status()
                .as_u16(),
            304
        );
        assert_eq!(
            client
                .get(format!("{}/index.html", h.origin()))
                .header("if-none-match", &etag)
                .send()
                .unwrap()
                .status()
                .as_u16(),
            200
        );
        let ranged = client
            .get(&root)
            .header("range", "bytes=0-9")
            .send()
            .unwrap();
        assert_eq!(ranged.status().as_u16(), 206);
        assert_eq!(
            ranged.headers()["content-range"],
            format!("bytes 0-9/{}", original.len())
        );
        assert_eq!(ranged.bytes().unwrap(), &original[..10]);
        let full = client
            .get(&root)
            .header("range", "bytes=0-9")
            .header("if-range", "different-version")
            .send()
            .unwrap();
        assert_eq!(full.status().as_u16(), 200);
        assert_eq!(full.bytes().unwrap(), original);
        let key = "b".repeat(64);
        let bytes = vec![42u8; 4096];
        let dir = h.root().join("archive/media");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join(format!("{key}.bin")), &bytes).unwrap();
        fs::write(dir.join(format!("{key}.json")),json!({"status":"saved","bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes)),"content_type":"image/png"}).to_string()).unwrap();
        let url = format!("{}/api/media/{key}", h.origin());
        let r = client
            .get(&url)
            .header("range", "bytes=2-8")
            .send()
            .unwrap();
        assert_eq!(r.status().as_u16(), 206);
        assert_eq!(r.bytes().unwrap(), &bytes[2..9]);
        let r = client
            .get(&url)
            .header("accept-encoding", "gzip")
            .send()
            .unwrap();
        assert!(r.headers().get("content-encoding").is_none());
        assert_eq!(r.bytes().unwrap(), bytes);
        fs::write(dir.join(format!("{key}.json")),json!({"status":"saved","bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes)),"content_type":"application/pdf"}).to_string()).unwrap();
        let r = client
            .get(&url)
            .header("accept-encoding", "gzip")
            .send()
            .unwrap();
        assert_eq!(r.headers()["content-encoding"], "gzip");
        let compressed = r.bytes().unwrap();
        let mut plain = Vec::new();
        flate2::read::GzDecoder::new(compressed.as_ref())
            .read_to_end(&mut plain)
            .unwrap();
        assert_eq!(plain, bytes);
    }
}
