use super::*;
use bevy::prelude::{App, Update};
use client_shell::menu::test_support as menu_fixture;
use client_shell::ui_resources::UiRuntimeResource;
use client_ui::ui_runtime::UiRuntime;

/// The actual Transfer follow keeps native I/O while replacing discovery with addressed RakNet.
#[test]
fn native_lan_transfer_selects_raknet_after_tearing_down_the_original_session() {
    let layout = launcher::test_support::scratch("native-lan-transfer");
    let mut menu = MenuRuntime::new_with_layout(
        true,
        Some(2),
        "Fixture".into(),
        layout,
        client_shell::player_skin::LocalPlayerSkin::generated_default("Fixture"),
    );
    menu_fixture::suppress_catalog(&mut menu);
    menu.show_world();
    let cache = BlobCache::default();
    let mut controller = SessionController::default()
        .with_native_transport(Some(protocol::NativeTransport::NetherNetLan))
        .with_client_random_id(Some(protocol::ClientRandomId::new(1).unwrap()));
    let old_generation = controller.generation;
    let JoinStage::Native(original) = controller
        .start_core(&menu, &cache, "127.0.0.1:19132", None, old_generation)
        .unwrap()
    else {
        panic!("original directed LAN join retains its native owner");
    };
    assert_eq!(original.transport, protocol::NativeTransport::NetherNetLan);
    let mut app = App::new();
    app.insert_resource(menu)
        .insert_resource(controller)
        .insert_resource(NetworkHandle::<PackApplication>::disconnected())
        .insert_resource(cache)
        .insert_resource(client_runtime::pack_admission::ResourcePackAdmissionState::default())
        .insert_resource(crate::runtime::network::session_registries::tests::collision_resource())
        .insert_resource(UiRuntimeResource(UiRuntime::new(old_generation)))
        .insert_resource(client_state::PlayerRuntime(player_state::PlayerState::new(
            1,
        )))
        .insert_resource(crate::runtime::world::ClientWorld {
            transfer_notice: Some(TransferNotice {
                host: "127.0.0.1".into(),
                port: 19133,
            }),
            ..Default::default()
        })
        .init_resource::<crate::runtime::shutdown::FatalError>()
        .insert_resource(client_interaction::movement::MovementTicker::default())
        .insert_resource(client_interaction::movement::LocalPhysicsController::default())
        .insert_resource(client_presentation::local_player::LocalPlayerFrameCarrier::default())
        .insert_resource(client_presentation::local_player::InteractionOriginSnapshot::default())
        .add_systems(Update, follow_server_transfer);
    app.update();
    let controller = app.world().resource::<SessionController>();
    assert_ne!(controller.generation, old_generation);
    let join = controller
        .join
        .as_ref()
        .expect("transfer retained the native join");
    let JoinStage::Native(target) = &join.stage else {
        panic!("native transfer started a Core owner");
    };
    assert_eq!(target.address, "127.0.0.1:19133");
    assert_eq!(target.transport, protocol::NativeTransport::RakNet);
    assert!(controller.directory.is_none());
    assert!(controller.core.id().is_none());
    assert!(
        app.world()
            .resource::<crate::runtime::world::ClientWorld>()
            .transfer_notice
            .is_none()
    );
}
