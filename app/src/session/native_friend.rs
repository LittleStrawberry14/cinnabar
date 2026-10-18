//! Opaque native friend dispatch through the existing generation-fenced session controller.

use super::*;

/// Starts worker provisioning for the selected owner without converting display fields into authority.
pub(super) fn begin(
    menu: &mut MenuRuntime,
    session: &mut SessionResources<'_>,
    selection: launcher_host::native_account::NativeFriendSelection,
) {
    let generation = session.retire();
    let controller = &mut *session.controller;
    controller.begin_fresh_transfer_chain();
    if let Err(error) = controller.prepare_client_random_id(menu.layout()) {
        controller.fail_join(menu, error);
        return;
    }
    controller.native_friend = Some(selection);
    controller.native_friend_session = true;
    controller.connecting = true;
    menu.begin_join_progress("Friend world", false);
    controller.join = Some(JoinAttempt {
        generation,
        address: String::new(),
        auth_cache: None,
        local_world: false,
        stage: JoinStage::Native(protocol::NativeConnectionTarget {
            address: String::new(),
            transport: protocol::NativeTransport::NetherNetMessaging,
        }),
    });
}
