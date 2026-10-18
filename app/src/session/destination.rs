//! Native backend selection over the existing destination grammar and Realm authority.

use protocol::{NativeConnectionTarget, NativeTransport};

/// Keeps service-owned destinations on their provisioning path and normalizes only direct endpoints.
pub(crate) fn native_target(
    address: &str,
    transport: NativeTransport,
    local_world: bool,
) -> Option<NativeConnectionTarget> {
    if local_world {
        return None;
    }
    if launcher_host::native_account::is_realm_target(address) {
        return Some(NativeConnectionTarget {
            address: address.trim().to_owned(),
            transport: NativeTransport::RakNet,
        });
    }
    match launcher_host::launcher_core::target_for(address) {
        bridge::ConnectTarget::RakNet(address) => {
            Some(NativeConnectionTarget { address, transport })
        }
        bridge::ConnectTarget::Realm(_)
        | bridge::ConnectTarget::Friend(_)
        | bridge::ConnectTarget::Gathering(_) => None,
    }
}
