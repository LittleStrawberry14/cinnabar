use super::*;

struct Fixture {
    root: PathBuf,
    store: AccountStore,
}

impl Fixture {
    /// Creates private synthetic account storage without reading the user's namespace.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "cinnabar-native-account-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        private_directory(&root).unwrap();
        let store = AccountStore::new(root.join("microsoft-token.json"));
        Self { root, store }
    }

    /// Publishes a synthetic generation using the existing private atomic writer.
    fn sign_in(&self, generation: &str) {
        let bytes = serde_json::to_vec(
            &serde_json::json!({"refresh_token": "old-refresh", "access_token": "old-access",
            "cinnabar_sign_in_generation": generation, "future_metadata": {"kept": true}}),
        )
        .unwrap();
        write_private(&self.store.active_cache, &bytes).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Native and Go refresh share one lease; publication cannot resurrect retired sign-ins.
#[test]
fn rotation_is_persisted_and_generation_replacement_or_removal_cannot_be_undone() {
    let fixture = Fixture::new();
    fixture.sign_in("first");
    fixture
        .store
        .try_native_lease()
        .unwrap()
        .publish_checkpoint(b"temporary-proof")
        .unwrap();
    write_private(
        &derived_path(&fixture.store.active_cache),
        br#"{"opaque_go_proof":"unchanged"}"#,
    )
    .unwrap();
    fixture
        .store
        .remember_current("123", "Synthetic", None)
        .unwrap();
    let mut native = fixture.store.try_native_lease().unwrap();
    assert_eq!(
        fixture.store.try_snapshot().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        fixture.store.try_discard_pending().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        fixture.store.try_native_lease().err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(try_lease(&cache_lock_path(&fixture.store.active_cache)).is_err());
    native
        .publish(
            "new-access",
            "rotated-refresh",
            "2099-01-01T00:00:00Z",
            b"synthetic-proof-owner",
        )
        .unwrap();
    assert!(!checkpoint_path(&fixture.store, None).exists());
    let oauth: serde_json::Value =
        serde_json::from_slice(&read_private(&fixture.store.active_cache).unwrap()).unwrap();
    assert_eq!(oauth["refresh_token"], "rotated-refresh");
    assert_eq!(oauth["cinnabar_sign_in_generation"], "first");
    assert_eq!(oauth["future_metadata"]["kept"], true);
    assert_eq!(
        read_private(&derived_path(&fixture.store.active_cache)).unwrap(),
        br#"{"opaque_go_proof":"unchanged"}"#
    );
    drop(native);
    let mut native = fixture.store.try_native_lease().unwrap();
    assert_eq!(native.refresh_token(), "rotated-refresh");
    assert_eq!(
        native.checkpoint().unwrap().unwrap().as_slice(),
        b"synthetic-proof-owner"
    );

    fixture.sign_in("replacement");
    assert_eq!(
        native
            .publish(
                "stale",
                "stale-refresh",
                "2099-01-01T00:00:00Z",
                b"stale-proof"
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        parse_oauth(&read_private(&fixture.store.active_cache).unwrap())
            .unwrap()
            .cinnabar_sign_in_generation,
        "replacement"
    );
    drop(native);
    let mut native = fixture.store.try_native_lease().unwrap();
    assert!(native.checkpoint().unwrap().is_none());
    remove_optional(&fixture.store.active_cache).unwrap();
    assert_eq!(
        native
            .publish(
                "stale",
                "stale-refresh",
                "2099-01-01T00:00:00Z",
                b"stale-proof"
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(!fixture.store.active_cache.exists());
    drop(native);

    fixture.sign_in("first");
    fixture.store.sign_out().unwrap();
    assert!(!checkpoint_path(&fixture.store, Some("123")).exists());
    assert!(fixture.store.try_native_lease().is_err());
}
