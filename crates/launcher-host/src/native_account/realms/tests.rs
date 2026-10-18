use super::*;
use launcher::accounts::AccountProfile;

/// Completes a fixture's nonblocking transaction, retrying only temporary lease contention.
fn transaction<T>(what: &str, mut perform: impl FnMut() -> io::Result<T>) -> T {
    let mut completed = None;
    test_time::eventually(what, || match perform() {
        Ok(value) => {
            completed = Some(value);
            true
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
        Err(error) => panic!("{what}: {error}"),
    });
    completed.expect("transaction completed")
}

/// Service transport admission preserves IPv6 text and never guesses unknown/signed transports as RakNet.
#[test]
fn realm_transport_and_catalog_metadata_follow_the_service() {
    let address: RealmAddress = serde_json::from_str(
        r#"{"address":"[2001:db8::8]:19132","networkProtocol":"default","pendingUpdate":true}"#,
    )
    .unwrap();
    let target = raknet_target(address).unwrap();
    assert_eq!(target.address, "[2001:db8::8]:19132");
    assert_eq!(target.transport, protocol::NativeTransport::RakNet);
    for protocol in ["NETHERNET", "NETHERNET_JSONRPC", "FUTURE_PROTOCOL"] {
        let address: RealmAddress = serde_json::from_value(
            serde_json::json!({"address":"123", "networkProtocol": protocol}),
        )
        .unwrap();
        assert_eq!(
            raknet_target(address).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
    let realm: Realm = serde_json::from_str(r#"{"id":7,"name":"  Synthetic Realm  ","owner":"  Synthetic Owner ","state":"CLOSED","maxPlayers":10,"daysLeft":-1,"expired":true,"member":true,"players":[{"online":true},{"online":false},{"online":true}]}"#).unwrap();
    let card = card(realm);
    assert_eq!(card.name, "Synthetic Realm");
    assert_eq!(card.owner, "Synthetic Owner");
    assert_eq!(
        card.target,
        format!("{}7", bridge::contract::REALM_TARGET_PREFIX)
    );
    assert_eq!(
        (card.online_players, card.max_players, card.days_left),
        (2, 10, -1)
    );
    assert!(card.member && card.expired && !card.can_play());
    assert!(card.address.is_empty(), "listing resolved an address");
    assert!(
        matches!(realm_target(" REALM_ID/7 ").unwrap(), Some(RealmTarget::Id(id)) if id.get() == 7)
    );
    assert!(matches!(
        realm_target("realm/synthetic-code").unwrap(),
        Some(RealmTarget::Invite("synthetic-code"))
    ));
    assert!(realm_target("realm_id/0").is_err());
    assert!(realm_target("realm_id/not-an-id").is_err());
    assert!(realm_target("[2001:db8::8]:19132").unwrap().is_none());
}

/// Realms promotion and subsequent operations retain the same Minecraft Session-Id and proof owner.
#[test]
fn listing_revision_fence_and_service_promotion_retain_one_account_owner() {
    let directory = tempfile::tempdir().unwrap();
    let store = AccountStore::new(directory.path().join("synthetic-oauth.json"));
    let credentials = Credentials::from_tokens(
        authentication_policy().client_id,
        "synthetic-access".into(),
        "synthetic-refresh".into(),
        UNIX_EPOCH + Duration::from_secs(4_000_000_000),
    )
    .unwrap();
    let sdk = MicrosoftAuth::new(authentication_policy())
        .unwrap()
        .into_account(credentials)
        .unwrap();
    let checkpoint = sdk.checkpoint().unwrap();
    let pending = transaction("reserve Realm fixture sign-in", || {
        store.try_begin_native_sign_in("synthetic-generation", None)
    });
    transaction("publish Realm fixture OAuth", || {
        pending.try_publish(
            "synthetic-access",
            "synthetic-refresh",
            "2099-01-01T00:00:00Z",
        )
    });
    transaction("publish Realm fixture checkpoint", || {
        pending.try_publish_checkpoint(checkpoint.as_bytes())
    });
    transaction("commit Realm fixture sign-in", || {
        pending.try_commit(&AccountProfile::from_identity("123", "Synthetic").unwrap())
    });
    let account = Arc::new(NativeAccount::new(
        store.clone(),
        directory.path().join("device.json"),
        Some("123".into()),
    ));
    let mut operation = account.begin().unwrap();
    let lease = transaction("acquire Realm fixture account", || store.try_native_lease());
    operation.cached = Some(Cached {
        provider: None,
        generation: lease.generation().into(),
        binding: lease.binding(),
        checkpoint_revision: lease.checkpoint_revision(),
        owner: Owner::Xbox(Box::new(sdk)),
        identity: Some(AccountIdentity {
            xuid: "123".into(),
            gamertag: "Synthetic".into(),
            current_credential: true,
        }),
    });
    operation.lease = Some(lease);
    operation.promote().unwrap();
    let session = operation.minecraft().unwrap().session_id();
    let proof = operation.minecraft().unwrap().checkpoint().unwrap();
    operation.promote().unwrap();
    assert_eq!(operation.minecraft().unwrap().session_id(), session);
    assert!(
        operation
            .minecraft()
            .unwrap()
            .checkpoint()
            .unwrap()
            .as_bytes()
            == proof.as_bytes(),
        "proof owner changed"
    );
    operation.persist().unwrap();
    let cached = operation.cached.as_ref().unwrap();
    let listing = CatalogListing {
        cards: CatalogCards {
            realms: Ok(Vec::new()),
            friends: Ok(Vec::new()),
        },
        generation: cached.generation.clone(),
        binding: cached.binding,
    };
    let stale = CatalogListing {
        cards: CatalogCards {
            realms: Ok(Vec::new()),
            friends: Ok(Vec::new()),
        },
        generation: cached.generation.clone(),
        binding: cached.binding,
    };
    drop(operation);
    assert!(listing.into_current_cards(&account).is_some());

    let mut operation = account.begin().unwrap();
    let lease = transaction("reacquire Realm fixture account", || {
        store.try_native_lease()
    });
    operation
        .cached
        .as_mut()
        .unwrap()
        .adopt_credentials(
            Credentials::from_tokens(
                authentication_policy().client_id,
                "new-access".into(),
                "rotated-refresh".into(),
                UNIX_EPOCH + Duration::from_secs(4_000_000_000),
            )
            .unwrap(),
            lease.binding(),
        )
        .unwrap();
    operation.lease = Some(lease);
    let Owner::Minecraft(services) = &operation.cached.as_ref().unwrap().owner else {
        panic!("service owner was replaced")
    };
    assert_eq!(services.session_id(), session);
    drop(operation);
    assert!(
        stale.into_current_cards(&account).is_none(),
        "an earlier credential revision published its list"
    );
    assert_eq!(
        transaction("read Realm fixture rotation", || store.try_native_lease()).refresh_token(),
        "rotated-refresh"
    );
}
