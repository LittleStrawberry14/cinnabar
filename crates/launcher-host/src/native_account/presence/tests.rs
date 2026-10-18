//! A retired original presence generation must fail before restoring or using another account.
use super::*;
use std::{fs, io::Write};

#[tokio::test]
async fn replacement_generation_cannot_receive_original_presence_update_or_removal() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synthetic-presence-oauth.json");
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).unwrap();
    file.write_all(
        serde_json::to_vec(&serde_json::json!({
            "refresh_token": "synthetic-refresh", "cinnabar_sign_in_generation": "replacement"
        }))
        .unwrap()
        .as_slice(),
    )
    .unwrap();
    drop(file);
    let account = Arc::new(NativeAccount::new(
        AccountStore::new(path),
        directory.path().join("device.json"),
        None,
    ));
    // Reaching restoration would fail with a different error, without contacting a service.
    account
        .store
        .try_native_lease()
        .unwrap()
        .publish_active_checkpoint(b"synthetic-invalid-sdk-checkpoint")
        .unwrap();
    let scope = PresenceScope {
        generation: "original".into(),
        xuid: "123".into(),
    };
    for removal in [false, true] {
        let result = if removal {
            Arc::clone(&account).remove_title_presence(&scope).await
        } else {
            Arc::clone(&account)
                .update_title_presence(&scope, &bridge::XboxPresenceState::default())
                .await
                .map(|_| ())
        };
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(account.state.lock().unwrap().cached.is_none());
        assert!(account.store.try_native_lease().is_ok());
    }
}
