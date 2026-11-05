use searchcord_archive::{Archive, SearchOptions, media};
use searchcord_collector::media_collection::{MediaCollector, clear_media};
use searchcord_parity::performance;
use searchcord_snapshot::{build_snapshot, export_media};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, process::Command};

#[test]
fn compact_python_manifests_survive_native_reads_export_and_cleanup() {
    let mut h = performance::fixture(0);
    let url = "https://cdn.discordapp.com/attachments/20/40/image.png";
    h.command(json!({"op":"save","messages":[{"id":performance::BASE.to_string(),"content":"compact media","author":{"id":"100","username":"fixture"},"attachments":[{"url":url,"filename":"image.png"}]}],"channel_id":"20","channel_name":"fixture","guild_id":"1","guild_name":"Fixture"}));
    let directory = media::directory(&h.archive());
    fs::create_dir(&directory).unwrap();
    let key = media::media_key(url).unwrap();
    let bytes = b"compact representation fixture";
    let digest = format!("{:x}", Sha256::digest(bytes));
    let filename = format!("{key}.{digest}.webp");
    let manifest = directory.join(format!("{key}.json"));
    let data = json!({"source_url":url,"status":"saved","storage_file":filename,"sha256":digest,"bytes":bytes.len(),"content_type":"image/webp","original_bytes":999});
    fs::write(directory.join(&filename), bytes).unwrap();
    fs::write(&manifest, data.to_string()).unwrap();
    assert_eq!(media::status(&h.archive(), url, true)["status"], "saved");
    let verified = media::verified_file(&h.archive(), &key).unwrap();
    assert_eq!(verified.path, directory.join(&filename));
    assert_eq!(
        media::local_url(&h.archive(), url),
        format!("/api/media/{key}")
    );
    let destination = h.root().join("archive/out/snapshot.db");
    build_snapshot(&h.archive(), &destination, 128).unwrap();
    assert_eq!(export_media(&h.archive(), &destination).unwrap().copied, 1);
    assert_eq!(
        fs::read(media::directory(&destination).join(&filename)).unwrap(),
        bytes
    );
    assert_eq!(media::status(&destination, url, true)["status"], "saved");
    for invalid in [
        "../private.webp".to_owned(),
        format!("{key}.{}.webp", "0".repeat(64)),
    ] {
        let mut invalid_data = data.clone();
        invalid_data["storage_file"] = json!(invalid);
        fs::write(&manifest, invalid_data.to_string()).unwrap();
        assert!(media::verified_file(&h.archive(), &key).is_err());
        assert_ne!(media::status(&h.archive(), url, true)["status"], "saved");
    }
    fs::write(&manifest, data.to_string()).unwrap();
    fs::write(directory.join("unrelated.webp"), b"keep").unwrap();
    fs::write(directory.join(format!("{key}.not-a-digest.webp")), b"keep").unwrap();
    assert_eq!(clear_media(&h.archive()).unwrap(), 2);
    assert!(!directory.join(&filename).exists());
    assert!(directory.join("unrelated.webp").exists());
}

#[test]
fn native_cursor_filters_keep_the_python_length_bound() {
    let h = performance::fixture(0);
    let archive = Archive::new(h.archive(), false);
    for index in 0..3 {
        for raw in ["00000000000000000001".to_owned(), "9".repeat(4301)] {
            let mut options = SearchOptions::default();
            match index {
                0 => options.guild_id = Some(raw),
                1 => options.channel_id = Some(raw),
                _ => options.author_id = Some(raw),
            }
            assert_eq!(archive.search(&options).unwrap_err().status, 400);
        }
    }
    assert!(
        archive
            .search(&SearchOptions {
                author_id: Some(i64::MAX.to_string()),
                ..Default::default()
            })
            .is_ok()
    );
}

#[test]
fn policies_are_checked_in_isolated_processes() {
    for mode in ["none", "original"] {
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "media_policy_worker", "--nocapture"])
            .env("SEARCHCORD_POLICY_TEST", "1")
            .env("SEARCHCORD_MEDIA_MODE", mode)
            .env("SEARCHCORD_PROFILE_IMAGES", "shapes")
            .status()
            .unwrap();
        assert!(status.success());
    }
}

#[test]
fn media_policy_worker() {
    if std::env::var("SEARCHCORD_POLICY_TEST").is_err() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("archive.db");
    fs::write(&path, b"fixture").unwrap();
    let collector =
        MediaCollector::for_loopback(&path, "http://127.0.0.1:9/", Default::default()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        for url in [
            "https://cdn.discordapp.com/avatars/30/hash.png",
            "https://cdn.discordapp.com/banners/30/hash.png",
        ] {
            assert_eq!(
                collector.cache_media(url, None).await.unwrap()["reason"],
                "disabled_by_policy"
            );
        }
        let mut message = json!({"author":{"id":"30","avatar":"hash","banner":"hash"}});
        if std::env::var("SEARCHCORD_MEDIA_MODE").unwrap() == "none" {
            let url = "https://cdn.discordapp.com/attachments/20/40/image.png";
            assert_eq!(
                collector.cache_media(url, None).await.unwrap()["reason"],
                "disabled_by_policy"
            );
            message["attachments"] = json!([{"url":url}]);
        }
        assert!(
            collector
                .archive_message_media(&[message], None)
                .await
                .unwrap()
                .is_empty()
        );
    });
    assert!(searchcord_archive::image_url("30", Some("hash"), "avatars", 64).is_none());
    assert!(!media::directory(&path).exists());
}
