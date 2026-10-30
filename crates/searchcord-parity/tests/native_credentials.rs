use searchcord_collector::credentials::{Paths, Vault};
use searchcord_parity::{Backend, Harness};
use serde_json::json;
use std::fs;
fn vault(h: &Harness) -> Vault {
    Vault::portable(Paths::new(&h.archive(), &h.vault(), &h.vault().with_extension("key")).unwrap())
}
#[test]
fn portable_vault_interchanges_with_python_and_verifies_atomic_replacement() {
    let mut h = Harness::with_backend(Backend::Python).unwrap();
    let v = vault(&h);
    let tokens = json!([{"id":"one","label":"Héllo fixture","token":"synthetic-credential-not-real","unknown":42}]);
    let state = json!({"tokens":tokens,"active_token_id":"one"});
    h.command(
        json!({"op":"save_credentials","portable":true,"tokens":tokens,"active_token_id":"one"}),
    );
    assert_eq!(v.load().unwrap(), state);
    v.clear().unwrap();
    assert!(v.paths.key.exists());
    v.save(&tokens, &json!("one")).unwrap();
    assert_eq!(
        h.command(json!({"op":"load_credentials","portable":true})),
        state
    );
    let sealed = fs::read(&v.paths.vault).unwrap();
    assert!(sealed.starts_with(b"SEARCHCORD-FERNET-1\n"));
    assert!(
        !sealed
            .windows(29)
            .any(|w| w == b"synthetic-credential-not-real")
    );
    assert!(!h.archive().exists());
    assert!(
        v.save_with_hook(&tokens, &json!("one"), |p| fs::write(
            p,
            b"damaged sealed bytes"
        ))
        .is_err()
    );
    assert_eq!(fs::read(&v.paths.vault).unwrap(), sealed);
    assert!(
        fs::read_dir(v.paths.vault.parent().unwrap())
            .unwrap()
            .all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".credential-"))
    );
    for (tokens, active) in [
        (
            json!([{ "id":"x","label":"x","token":"x"},{"id":"x","label":"y","token":"y"}]),
            json!("x"),
        ),
        (json!([{"id":"x","label":"","token":"x"}]), json!("x")),
        (json!([]), json!("missing")),
        (json!("invalid"), json!(null)),
    ] {
        assert!(v.save(&tokens, &active).is_err());
        assert_eq!(fs::read(&v.paths.vault).unwrap(), sealed);
    }
    assert_eq!(
        Paths::archive_key(&h.archive()).unwrap(),
        h.command(json!({"op":"credential_key"})).as_str().unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&v.paths.key).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&v.paths.vault).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(v.paths.vault.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}
#[test]
fn portable_vault_missing_key_and_corruption_preserve_existing_ciphertext() {
    let h = Harness::with_backend(Backend::Python).unwrap();
    let v = vault(&h);
    let tokens = json!([{"id":"one","label":"Fixture","token":"synthetic"}]);
    v.save(&tokens, &json!("one")).unwrap();
    let sealed = fs::read(&v.paths.vault).unwrap();
    let key = fs::read(&v.paths.key).unwrap();
    fs::remove_file(&v.paths.key).unwrap();
    assert!(v.load().unwrap_err().to_string().contains("missing"));
    assert!(v.save(&tokens, &json!("one")).is_err());
    assert!(!v.paths.key.exists());
    assert_eq!(fs::read(&v.paths.vault).unwrap(), sealed);
    fs::write(&v.paths.key, &key).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&v.paths.key, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut corrupt = sealed.clone();
    let last = corrupt.len() - 2;
    corrupt[last] ^= 1;
    fs::write(&v.paths.vault, &corrupt).unwrap();
    assert!(v.load().is_err());
    assert!(v.save(&tokens, &json!("one")).is_err());
    assert_eq!(fs::read(&v.paths.vault).unwrap(), corrupt);
    v.clear().unwrap();
    assert_eq!(
        v.load().unwrap(),
        json!({"tokens":[],"active_token_id":null})
    );
    assert!(v.paths.key.exists());
}
#[test]
fn vault_paths_reject_archive_capture_and_insecure_permissions() {
    let h = Harness::with_backend(Backend::Python).unwrap();
    assert!(
        Paths::new(
            &h.archive(),
            &h.archive().with_file_name("vault"),
            &h.vault().with_extension("key")
        )
        .is_err()
    );
    assert!(Paths::new(&h.archive(), &h.vault(), &h.vault()).is_err());
    let v = vault(&h);
    v.save(&json!([]), &json!(null)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&v.paths.key, &v.paths.vault] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(v.load().unwrap_err().to_string().contains("chmod 600"));
            assert!(v.save(&json!([]), &json!(null)).is_err());
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}
