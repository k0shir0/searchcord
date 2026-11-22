use searchcord_archive::{Archive, SearchOptions, media, search_label};
use searchcord_parity::{Harness, performance};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;

fn normalized(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.remove("elapsed_ms");
        object.remove("profile_age_seconds");
    }
    value
}
fn fixture() -> Harness {
    let mut h = performance::fixture(600);
    h.database().execute_batch("UPDATE guilds SET name='🌿Ｆｉｘｔｕｒｅ — StraßE' WHERE id='1'; UPDATE channels SET name='📢┃ＧＥＮＥＲＡＬ' WHERE id='20';
        UPDATE authors SET name='²Alice' WHERE id='101'; UPDATE authors SET name='ⅧOld' WHERE id='102';
        INSERT INTO profiles(user_id,username,global_name,avatar_hash,accent_color,fetched_at) VALUES ('100','saved.name','Saved Name','a_abc123',0,'2026-01-01T00:00:00+00:00');
        INSERT INTO authors(id,name) VALUES ('1450000000000000001','Large ID'),('9223372036854775807','Maximum ID');").unwrap();
    let payload = json!({"user":{"id":"100","username":"saved.name","global_name":"Saved Name","avatar":"a_abc123"},
        "user_profile":{"bio":"Complete bio","pronouns":"they/them","banner":"banner123","accent_color":0},
        "presence":{"status":"idle","activities":[{"type":4,"state":"saved status"}]},"unknown":{"nested":[1,2,3]},
        "badges":[],"connected_accounts":[{"type":"github","name":"saved"}]});
    h.database()
        .execute(
            "INSERT INTO profile_details VALUES ('100',?,'2026-01-01T00:00:00+00:00')",
            [payload.to_string()],
        )
        .unwrap();
    let messages = json!([{"id":performance::BASE.to_string(),"content":"media fixture","author":{"id":"100","username":"saved.name"},
        "attachments":[{"filename":"image.png","content_type":"image/png","url":"https://cdn.discordapp.com/attachments/20/40/image.png?ex=abc"},
            {"filename":"unsafe.png","content_type":"image/png","url":"javascript:alert(1)"},
            {"filename":"file.pdf","content_type":"application/pdf","url":"https://cdn.discordapp.com/attachments/20/41/file.pdf"}]}]);
    h.command(json!({"op":"save","messages":messages,"channel_id":"20","channel_name":"general","guild_id":"1","guild_name":"Fixture"}));
    h.command(json!({"op":"snapshot","destination":"packed.db","block_rows":16}));
    h
}

#[test]
fn native_profile_presentation_and_observed_servers_match_standard_and_packed() {
    let mut h = fixture();
    for archive in ["synthetic.db", "packed.db"] {
        let native = Archive::new(h.root().join("archive").join(archive), true);
        for uid in [
            "100",
            "101",
            "102",
            "1450000000000000001",
            "9223372036854775807",
        ] {
            let actual = native.profile(uid).unwrap();
            let expected=h.command(json!({"op":"archive_read","archive":archive,"method":"profile","args":{"uid":uid}}));
            assert_eq!(normalized(actual), normalized(expected), "{archive} {uid}");
            for q in ["", "fixture", "STRASSE", "1", "absent"] {
                let actual = native.profile_servers(uid, q, 0, 2).unwrap();
                let expected=h.command(json!({"op":"archive_read","archive":archive,"method":"profile_servers","args":{"uid":uid,"q":q,"limit":2}}));
                assert_eq!(actual, expected);
            }
        }
        assert_eq!(native.profile("unknown").unwrap_err().status, 400);
        assert_eq!(native.profile("999").unwrap_err().status, 404);
        let actual = native
            .search(&SearchOptions {
                q: "media fixture".into(),
                refresh_images: false,
                ..Default::default()
            })
            .unwrap();
        let expected=h.command(json!({"op":"archive_read","archive":archive,"method":"search","args":{"q":"media fixture"}}));
        assert_eq!(normalized(actual), normalized(expected));
    }
}

#[test]
fn native_unicode_labels_and_suggestions_preserve_reference_normalization() {
    let mut h = fixture();
    for label in [
        "📢┃ＧＥＮＥＲＡＬ — Café",
        "Straße Σς İ",
        "e\u{301} test",
        " 𝒜ＢＣ １２ ² ",
        "Ⓐ—漢字🙂",
        "\u{1c}Hi\u{1f}",
        "Ა Georgian Ꭰᏸ",
    ] {
        let expected =
            h.command(json!({"op":"presentation","method":"search_label","value":label}));
        assert_eq!(json!(search_label(label)), expected);
    }
    let archive = Archive::new(h.archive(), true);
    h.start_viewer("synthetic.db", true, None);
    for (kind, q) in [
        ("server", "fixture"),
        ("server", "strasse"),
        ("channel", "general"),
        ("author", "saved"),
        ("author", "100"),
        ("author", "²"),
        ("author", "Ⅷ"),
    ] {
        let expected = h.get(&format!("/api/suggestions/{kind}"), &[("q", q.to_owned())]);
        assert_eq!(expected.status, 200);
        assert_eq!(
            archive.suggestions(kind, q, None).unwrap(),
            expected.json(),
            "{kind} {q}"
        );
    }
}

#[test]
fn native_media_identity_status_and_serving_reject_corruption() {
    let mut h = fixture();
    for url in [
        "https://cdn.discordapp.com/avatars/100/a_abc123.gif?size=64&ex=1",
        "HTTPS://CDN.DISCORDAPP.COM:443/a b?z=*&q=a+b&EX=2#fragment",
        "https://media.discordapp.net/a\\b?empty=&x=%00",
        "https://evil.example/file.png",
        "http://cdn.discordapp.com/x",
        "https://user@cdn.discordapp.com/x",
        "https://cdn.discordapp.com:80/x",
        "https://cdn.discordapp.com/x?x=%E9",
        "https://cdn.discordapp.com/x?x=~&x=*&a=%2f",
        "https://cdn.discordapp.com/x\n",
    ] {
        let expected = h.command(json!({"op":"presentation","method":"canonical_url","value":url}));
        assert_eq!(json!(media::canonical_url(url)), expected, "{url}");
    }
    let url = "https://cdn.discordapp.com/attachments/20/40/image.png?ex=abc";
    let key = media::media_key(url).unwrap();
    let dir = h.root().join("archive/media");
    fs::create_dir_all(&dir).unwrap();
    let binary = b"saved png";
    let manifest = json!({"status":"saved","bytes":binary.len(),"sha256":format!("{:x}",Sha256::digest(binary)),"content_type":"image/png","source_url":media::canonical_url(url)});
    fs::write(dir.join(format!("{key}.bin")), binary).unwrap();
    fs::write(dir.join(format!("{key}.json")), manifest.to_string()).unwrap();
    assert_eq!(media::status(&h.archive(), url, true), manifest);
    assert_eq!(
        media::local_url(&h.archive(), url),
        format!("/api/media/{key}")
    );
    let file = media::verified_file(&h.archive(), &key).unwrap();
    assert!(file.inline);
    assert_eq!(file.content_type, "image/png");
    assert_eq!(file.bytes, 9);
    let archive = Archive::new(h.archive(), true);
    let actual = archive
        .search(&SearchOptions {
            q: "media fixture".into(),
            refresh_images: true,
            ..Default::default()
        })
        .unwrap();
    let expected=h.command(json!({"op":"archive_read","method":"search","args":{"q":"media fixture","refresh_images":true}}));
    assert_eq!(normalized(actual), normalized(expected));
    fs::write(dir.join(format!("{key}.bin")), b"bad! png!").unwrap();
    assert_eq!(
        media::verified_file(&h.archive(), &key)
            .err()
            .unwrap()
            .status,
        404
    );
    assert_eq!(
        media::status(&h.archive(), url, true),
        h.command(json!({"op":"presentation","method":"media_status","value":url,"verify":true}))
    );
    assert!(media::verified_file(&h.archive(), "invalid").is_err());
}
