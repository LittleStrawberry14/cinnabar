use super::*;
use crate::args::ParseOutcome;
use std::fs;

#[cfg(feature = "native-networking")]
#[test]
fn native_direct_connections_do_not_provision_a_core_directory() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("native-session");
    let parsed = run_args(&[
        "client",
        "--address",
        "127.0.0.1:19132",
        "--native-networking",
        "raknet",
    ]);
    let _holder = bind_direct_session_directory(&parsed, directory.clone()).unwrap();
    assert!(!directory.exists());
    assert!(matches!(
        parsed.session_endpoint(directory).unwrap(),
        protocol::SessionEndpoint::Native(_)
    ));
}

/// Native command-line connections use the same endpoint grammar as saved menu destinations.
#[cfg(feature = "native-networking")]
#[test]
fn native_cli_endpoint_normalizes_bare_host_and_ipv6_addresses() {
    let root = tempfile::tempdir().unwrap();
    for address in [
        "play.example.net:19133",
        "[2001:db8::7]:19134",
        "play.example.net",
        "2001:db8::7",
        " play.example.net ",
    ] {
        let parsed = run_args(&[
            "client",
            "--address",
            address,
            "--native-networking",
            "raknet",
        ]);
        let bridge::ConnectTarget::RakNet(expected) =
            launcher_host::launcher_core::target_for(address)
        else {
            panic!("direct fixture must use the ordinary endpoint owner");
        };
        let protocol::SessionEndpoint::Native(target) = parsed
            .session_endpoint(root.path().join("native-session"))
            .unwrap()
        else {
            panic!("native direct command-line target changed backend");
        };
        assert_eq!(target.transport, protocol::NativeTransport::RakNet);
        assert_eq!(
            target.address, expected,
            "native command-line endpoint lost existing normalization for {address}"
        );
    }
}

/// Go-owned service locators retain an actual scoped Core endpoint despite native direct opt-in.
#[cfg(feature = "native-networking")]
#[test]
fn native_cli_service_destinations_keep_core_directory_ownership() {
    let root = tempfile::tempdir().unwrap();
    for address in [
        format!(
            "{}{}",
            launcher::menu::EXPERIENCE_ADDRESS_PREFIX,
            "00000000-0000-0000-0000-000000000007"
        ),
        format!("{}123", bridge::contract::FRIEND_TARGET_PREFIX),
    ] {
        let parsed = run_args(&[
            "client",
            "--address",
            &address,
            "--native-networking",
            "raknet",
        ]);
        let directory = root.path().join(format!("direct-{}", std::process::id()));
        let endpoint = parsed.session_endpoint(directory.clone()).unwrap();
        assert_eq!(endpoint.core_directory(), Some(directory.as_path()));
        let holder = bind_direct_session_directory(&parsed, directory.clone()).unwrap();
        assert!(
            directory.is_dir(),
            "Go provisioning requires its actual owned directory"
        );
        drop(holder);
        assert!(
            !directory.exists(),
            "the fallback directory lost scoped cleanup"
        );
    }
}

/// Native preference alone starts the launcher without creating a fictitious connection directory.
#[cfg(feature = "native-networking")]
#[test]
fn native_cli_menu_without_address_does_not_bind_a_session_directory() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("menu-runtime");
    let parsed = run_args(&["client", "--native-networking", "raknet"]);
    assert!(!parsed.connection_requested());
    let _holder = bind_direct_session_directory(&parsed, directory.clone()).unwrap();
    assert!(!directory.exists());
}

/// Realm namespaces stay opaque and account-resolved, including an existing invite or case alias.
#[cfg(feature = "native-networking")]
#[test]
fn native_cli_realms_keep_opaque_locators_for_account_resolution() {
    let root = tempfile::tempdir().unwrap();
    for address in [
        format!("{}7", bridge::contract::REALM_TARGET_PREFIX),
        format!(
            " {}7 ",
            bridge::contract::REALM_TARGET_PREFIX.to_ascii_uppercase()
        ),
        format!("{}synthetic-code", bridge::contract::REALM_CODE_PREFIX),
    ] {
        assert!(launcher_host::native_account::is_realm_target(&address));
        let parsed = run_args(&[
            "client",
            "--address",
            &address,
            "--native-networking",
            "nethernet-lan",
        ]);
        let protocol::SessionEndpoint::Native(target) = parsed
            .session_endpoint(root.path().join("native-session"))
            .unwrap()
        else {
            panic!("Realm locator lost its native account resolution owner");
        };
        assert_eq!(target.address, address.trim());
        assert_eq!(target.transport, protocol::NativeTransport::RakNet);
    }
}

fn run_args(arguments: &[&str]) -> args::ClientArgs {
    match args::ClientArgs::parse_from(arguments.to_vec()) {
        Ok(ParseOutcome::Run(parsed)) => *parsed,
        outcome => panic!("expected run arguments, got {outcome:?}"),
    }
}

fn temporary_root(label: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock must be after the Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("rust-mcbe-{label}-{}-{nonce}", std::process::id()))
}

#[test]
fn explicit_non_grammar_socket_dir_starts_without_a_guard() {
    // Base behavior: documented custom socket directories are accepted
    // even though their leaf violates the session-directory grammar.
    let root = temporary_root("explicit-sock-dir");
    let custom = root.join("custom.sock");
    let parsed = run_args(&[
        "client",
        "--address",
        "127.0.0.1:19132",
        "--socket-dir",
        custom.to_str().expect("temp path is UTF-8"),
    ]);
    assert!(parsed.socket_dir_explicit);

    let holder = bind_direct_session_directory(&parsed, resolve_socket_dir(&parsed.socket_dir))
        .expect("an explicit non-grammar socket directory must start cleanly");
    assert!(
        custom.is_dir(),
        "the historical side effect of ensuring the directory exists stays"
    );
    let owned_entries = fs::read_dir(&custom)
        .expect("read prepared socket directory")
        .count();
    assert_eq!(
        owned_entries, 0,
        "unguarded operator directories never receive an ownership marker"
    );
    drop(holder);
    assert!(
        custom.is_dir(),
        "teardown leaves an unowned operator directory untouched"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn derived_default_directory_still_binds_the_guard() {
    let root = temporary_root("derived-sock-dir");
    fs::create_dir_all(&root).expect("create temp root");
    let parsed = run_args(&["client", "--address", "127.0.0.1:19132"]);
    assert!(!parsed.socket_dir_explicit);
    let socket_dir = root.join("direct-123");

    let holder = bind_direct_session_directory(&parsed, socket_dir.clone())
        .expect("app-derived directories keep exclusive ownership");
    assert!(socket_dir.is_dir(), "binding prepares the owned directory");
    drop(holder);
    assert!(
        !socket_dir.exists(),
        "default-path ownership and teardown are unchanged"
    );
    let _ = fs::remove_dir_all(&root);
}
