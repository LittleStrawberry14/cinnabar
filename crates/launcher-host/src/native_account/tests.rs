use super::*;
use std::{
    fs,
    future::Future,
    io::Write,
    task::{Context, Poll, Waker},
};

/// Creates only synthetic private OAuth bytes in a fresh test namespace.
fn seed(path: &std::path::Path, generation: &str) {
    let mut options = fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).unwrap();
    let bytes = serde_json::to_vec(&serde_json::json!({"access_token":"before-access", "refresh_token":"before-refresh", "cinnabar_sign_in_generation":generation})).unwrap();
    file.write_all(&bytes).unwrap();
}

/// Dropping a later-stage acquisition preserves the SDK-visible refresh and retires replaced generations.
#[test]
fn cancelling_after_oauth_rotation_persists_before_releasing_the_lease() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synthetic-oauth.json");
    seed(&path, "first");
    let account = Arc::new(NativeAccount::new(
        AccountStore::new(path.clone()),
        directory.path().join("device.json"),
        None,
    ));
    let mut operation = account.begin().unwrap();
    let lease = account.store.try_native_lease().unwrap();
    let policy = xbox::Config::default();
    let credentials = Credentials::from_tokens(
        policy.client_id.clone(),
        "acquired-access".into(),
        "rotated-refresh".into(),
        UNIX_EPOCH + Duration::from_secs(4_000_000_000),
    )
    .unwrap();
    let sdk = MicrosoftAuth::new(policy.clone())
        .unwrap()
        .into_account(credentials)
        .unwrap();
    let proof = sdk.checkpoint().unwrap();
    operation.cached = Some(Cached {
        generation: lease.generation().into(),
        binding: lease.binding(),
        owner: Owner::Xbox(Box::new(sdk)),
        gamertag: String::new(),
    });
    operation.lease = Some(lease);
    let mut pending = Box::pin(async move {
        let _operation = operation;
        std::future::pending::<()>().await;
    });
    assert!(matches!(
        pending
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    assert_eq!(
        account.store.try_native_lease().err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(pending);
    let lease = account.store.try_native_lease().unwrap();
    assert_eq!(lease.refresh_token(), "rotated-refresh");
    let bytes = lease.checkpoint().unwrap().unwrap();
    let restored = MicrosoftAuth::new(policy)
        .unwrap()
        .restore_account(xbox::Checkpoint::from_bytes(bytes).unwrap())
        .unwrap();
    assert_eq!(restored.checkpoint().unwrap().as_bytes(), proof.as_bytes());
    drop(lease);
    let oauth: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(oauth["access_token"], "acquired-access");
    assert_eq!(oauth["cinnabar_sign_in_generation"], "first");

    let mut operation = account.begin().unwrap();
    seed(&path, "replacement");
    operation.lease = Some(account.store.try_native_lease().unwrap());
    drop(operation);
    assert!(account.state.lock().unwrap().cached.is_none());
    let lease = account.store.try_native_lease().unwrap();
    assert_eq!(lease.generation(), "replacement");
    assert_eq!(lease.refresh_token(), "before-refresh");
    assert!(lease.checkpoint().unwrap().is_none());
}
