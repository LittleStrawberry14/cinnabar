use super::*;
use launcher_host::auth::test_support::native_state;

/// Failed and expired checks of a selected account never emit a signed-out native join intent.
#[test]
fn native_saved_account_failure_and_timeout_return_to_the_menu() {
    for state in [
        AuthState::Failed("Synthetic validation failed.".into()),
        AuthState::Checking,
    ] {
        let mut menu = MenuRuntime::new(true, 2, "Synthetic".into()).with_native_sign_in(true);
        menu.auth_process = Some(native_state(state));
        menu.auth_attempted = true;
        menu.feeds.account_active_id = Some("123".into());
        menu.request_connect("synthetic.example:19132".into());
        let now = std::time::Instant::now();
        menu.intents.join_sign_in_deadline = Some(now);
        assert!(menu.take_join_intent().is_none());
        assert!(menu.intents.join.is_none());
        assert!(
            menu.view()
                .disconnect_message
                .as_deref()
                .is_some_and(|message| message.contains("selected account"))
        );
    }

    let mut menu = MenuRuntime::new(true, 2, "Signed out".into()).with_native_sign_in(true);
    menu.auth_process = Some(native_state(AuthState::SignedOut));
    menu.auth_attempted = true;
    menu.request_connect("synthetic.example:19132".into());
    assert!(menu.take_join_intent().unwrap().auth_cache.is_none());
}

/// A verified native direct join does not depend on an unrelated Go account core becoming ready.
#[test]
fn verified_native_direct_join_does_not_wait_for_the_account_core() {
    let mut menu = MenuRuntime::new(true, 2, "Synthetic".into()).with_native_sign_in(true);
    menu.auth_process = Some(native_state(AuthState::Authenticated));
    menu.auth_attempted = true;
    menu.feeds.account_active_id = Some("123".into());
    menu.account_core = Some(super::super::launcher_core::AccountCore::default());
    menu.request_connect("synthetic.example:19132".into());
    menu.intents.join_sign_in_deadline =
        Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
    let intent = menu
        .take_join_intent()
        .expect("native readiness does not use the Go core");
    assert_eq!(intent.auth_cache, Some(menu.layout.auth_cache()));
}
