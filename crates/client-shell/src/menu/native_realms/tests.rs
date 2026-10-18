use super::*;
use launcher::menu::view::{MenuFriendCard, MenuRealmCard};
use launcher_host::account_control::{AccountControl, AccountEvent};

/// Supplies only legacy account feeds; no process, HTTP or credentials are involved.
struct Legacy;
impl AccountControl for Legacy {
    fn account_status(&mut self) -> Option<AuthState> {
        None
    }
    fn realms(&mut self) -> Option<Vec<MenuRealmCard>> {
        Some(vec![realm("legacy")])
    }
    fn friends(&mut self) -> Option<Vec<MenuFriendCard>> {
        Some(vec![MenuFriendCard {
            gamertag: "Synthetic friend".into(),
            xuid: "456".into(),
            world_name: "Synthetic world".into(),
            members: "1 / 2".into(),
            max_members: 2,
        }])
    }
    fn sign_out(&mut self) -> bool {
        false
    }
    fn poll_event(&mut self) -> Option<AccountEvent> {
        None
    }
}

/// Creates an ordinary existing menu model for ownership assertions.
fn realm(name: &str) -> MenuRealmCard {
    serde_json::from_value(serde_json::json!({"name":name,"state":"OPEN","target":format!("{}7",bridge::contract::REALM_TARGET_PREFIX)})).unwrap()
}

/// Native mode rejects stale Go Realm feeds while preserving unrelated friend data and account lifetime.
#[test]
fn native_realms_own_the_feed_and_share_the_selected_account_with_joins() {
    let mut menu = MenuRuntime::new(true, 2, "Synthetic".into()).with_native_sign_in(true);
    menu.feeds.account_active_id = Some("123".into());
    let path = menu.layout.auth_cache();
    let first = menu.native_account_for(path.clone());
    menu.realms.replace(vec![realm("native")]);
    menu.sync_account_control(&mut Legacy);
    assert_eq!(menu.realms[0].name, "native");
    assert_eq!(menu.friends[0].gamertag, "Synthetic friend");
    assert!(Arc::ptr_eq(&first, &menu.native_account_for(path.clone())));
    assert_eq!(menu.realms[0].name, "native");
    menu.native_realms.refresh();
    assert!(
        Arc::ptr_eq(&first, &menu.native_account_for(path.clone())),
        "refresh replaced the account/session owner"
    );
    menu.feeds.account_active_id = Some("456".into());
    let next = menu.native_account_for(path);
    assert!(!Arc::ptr_eq(&first, &next));
    assert!(
        menu.realms.is_empty(),
        "the previous selected account's Realms survived"
    );

    let mut go = MenuRuntime::new(true, 2, "Synthetic".into());
    go.sync_account_control(&mut Legacy);
    assert_eq!(go.realms[0].name, "legacy");
}
