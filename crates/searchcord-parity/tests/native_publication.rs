use searchcord_archive::media;
use searchcord_parity::performance;
use searchcord_snapshot::{SnapshotError, build_snapshot, export_media, publish_with_hooks};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;

#[test]
fn native_publication_keeps_destinations_racers_and_other_builds_safe() {
    let h = performance::fixture(33);
    let source = fs::read(h.archive()).unwrap();
    let directory = h.root().join("archive/out");
    fs::create_dir(&directory).unwrap();
    let destination = directory.join("snapshot.db");
    let partial = directory.join("snapshot.db.building");
    let failed = publish_with_hooks(
        &h.archive(),
        &destination,
        16,
        |count, _| {
            if count >= 32 {
                Err(SnapshotError("cancelled".into()))
            } else {
                Ok(())
            }
        },
        |_| Ok(()),
    );
    assert_eq!(failed.unwrap_err().to_string(), "cancelled");
    assert!(!destination.exists());
    assert!(!partial.exists());
    let raced = publish_with_hooks(
        &h.archive(),
        &destination,
        16,
        |_, _| Ok(()),
        |path| {
            fs::write(path, b"concurrent exporter")?;
            Ok(())
        },
    );
    assert!(raced.is_err());
    assert_eq!(fs::read(&destination).unwrap(), b"concurrent exporter");
    assert!(!partial.exists());
    assert!(build_snapshot(&h.archive(), &destination, 16).is_err());
    fs::remove_file(&destination).unwrap();
    fs::write(&partial, b"other build").unwrap();
    assert!(build_snapshot(&h.archive(), &destination, 16).is_err());
    assert_eq!(fs::read(&partial).unwrap(), b"other build");
    fs::remove_file(&partial).unwrap();
    let report = build_snapshot(&h.archive(), &destination, 16).unwrap();
    assert_eq!(report.messages, 33);
    assert!(!partial.exists());
    assert_eq!(fs::read(h.archive()).unwrap(), source);
}

#[test]
fn native_media_export_copies_only_verified_references_and_refuses_reuse() {
    let mut h = performance::fixture(0);
    let url = "https://cdn.discordapp.com/attachments/20/40/file.pdf";
    let corrupt = "https://cdn.discordapp.com/attachments/20/41/corrupt.pdf";
    h.command(json!({"op":"save","messages":[{"id":performance::BASE.to_string(),"content":"media","author":{"id":"100","username":"fixture"},
        "attachments":[{"url":url,"filename":"file.pdf"},{"url":corrupt,"filename":"corrupt.pdf"}]}],"channel_id":"20","channel_name":"fixture","guild_id":"1","guild_name":"Fixture"}));
    let source = h.root().join("archive/media");
    fs::create_dir(&source).unwrap();
    let key = media::media_key(url).unwrap();
    let bad = media::media_key(corrupt).unwrap();
    for (key, bytes, digest) in [
        (
            &key,
            b"verified".as_slice(),
            format!("{:x}", Sha256::digest(b"verified")),
        ),
        (&bad, b"damaged".as_slice(), "0".repeat(64)),
    ] {
        fs::write(source.join(format!("{key}.bin")), bytes).unwrap();
        fs::write(source.join(format!("{key}.json")),json!({"status":"saved","bytes":bytes.len(),"sha256":digest,"content_type":"application/pdf","unknown":"preserved"}).to_string()).unwrap();
    }
    fs::write(source.join("unrelated.bin"), b"must not copy").unwrap();
    let dest = h.root().join("archive/out/snapshot.db");
    build_snapshot(&h.archive(), &dest, 128).unwrap();
    let failed = searchcord_snapshot::export_media_with_hook(&h.archive(), &dest, |path| {
        fs::write(path, b"changed")?;
        Ok(())
    });
    assert_eq!(
        failed.unwrap_err().to_string(),
        "Copied media did not match its recorded digest"
    );
    assert!(!h.root().join("archive/out/media").exists());
    assert_eq!(
        fs::read(source.join(format!("{key}.bin"))).unwrap(),
        b"verified"
    );
    let report = export_media(&h.archive(), &dest).unwrap();
    assert_eq!(report.copied, 1);
    assert_eq!(report.unavailable, 1);
    assert_eq!(report.bytes, 8);
    let copied = h.root().join("archive/out/media");
    assert_eq!(fs::read_dir(&copied).unwrap().count(), 2);
    assert_eq!(
        fs::read(copied.join(format!("{key}.bin"))).unwrap(),
        b"verified"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(copied.join(format!("{key}.json"))).unwrap()).unwrap();
    assert_eq!(manifest["unknown"], "preserved");
    assert!(export_media(&h.archive(), &dest).is_err());
    assert!(source.join("unrelated.bin").exists());
}
