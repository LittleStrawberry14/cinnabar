//! Finishes the existing endpoint provisioning and starts the shared session worker.

use super::*;

/// Advances the pending join without blocking: starts its network session
/// once the endpoint is ready, or fails it back to the menu.
pub(super) fn poll_join(
    commands: &mut Commands,
    menu: &mut MenuRuntime,
    session: &mut SessionResources<'_>,
    cache: &BlobCache,
) {
    let controller = &mut *session.controller;
    if controller.join.is_none() {
        return;
    }
    let client_random_id = match controller.poll_client_random_id() {
        Ok(Some(id)) => id,
        Ok(None) => return,
        Err(message) => {
            let join = controller.join.take();
            controller.core.stop_detached(move || drop(join));
            controller.fail_join(menu, message);
            return;
        }
    };
    let attempt = controller.join.take().expect("pending join");
    #[cfg(feature = "native-networking")]
    let native_auth_cache = attempt.auth_cache.clone();
    use client_session::connection::JoinPoll;
    match attempt.poll(controller.generation, controller.connecting, || {
        controller.core.exit_status()
    }) {
        JoinPoll::Retired => {}
        JoinPoll::Pending(attempt) => controller.join = Some(attempt),
        JoinPoll::StartCore(mut attempt) => {
            match controller.start_core(
                menu,
                cache,
                &attempt.address,
                attempt.auth_cache.as_deref(),
                attempt.generation,
            ) {
                Ok(stage) => {
                    attempt.stage = stage;
                    controller.join = Some(attempt);
                }
                Err(message) => controller.fail_join(menu, message),
            }
        }
        JoinPoll::Ready {
            endpoint,
            directory,
        } => {
            let owned_core = directory.is_some();
            if let Some(directory) = directory {
                controller.bind_directory(directory);
            }
            let login_settings = controller.login_settings(menu, session.input.as_deref());
            #[cfg(feature = "native-networking")]
            let native_account = if controller.native_friend.is_none()
                && controller.native_gathering.is_none()
                && controller.native_local_world.is_none()
                && matches!(endpoint, protocol::SessionEndpoint::Native(_))
            {
                native_auth_cache.map(|path| controller.native_account_for(menu, path))
            } else {
                None
            };
            #[cfg(feature = "native-networking")]
            let native_friend = match controller
                .native_friend
                .take()
                .map(|selection| controller.friend_cleanup.request(selection))
                .transpose()
            {
                Ok(request) => request,
                Err(error) => {
                    controller.fail_join(menu, error.to_string());
                    return;
                }
            };
            if let Err(error) = start_network(
                commands,
                menu,
                controller.generation,
                cache,
                endpoint,
                #[cfg(feature = "native-networking")]
                native_account,
                #[cfg(feature = "native-networking")]
                native_friend,
                #[cfg(feature = "native-networking")]
                controller.native_gathering.take(),
                #[cfg(feature = "native-networking")]
                controller.native_local_world.take(),
                login_settings,
                client_random_id,
                session.actor_artwork.as_deref().map(|artwork| &artwork.0),
                session.ui_catalog.as_deref(),
                crate::runtime::network::session_registries::RegistryBase::new(
                    &session.state.client_world.runtime_assets,
                    &session.collisions,
                ),
            ) {
                if owned_core {
                    let directory = controller.directory.take();
                    controller.core.stop_detached(move || drop(directory));
                }
                controller.fail_join(menu, format!("Could not connect: {error}"));
            }
        }
        JoinPoll::Failed { message, directory } => {
            if let Some(directory) = directory {
                controller.core.stop_detached(move || drop(directory));
            }
            controller.fail_join(menu, message);
        }
    }
}

/// Starts the network session against the core serving `socket_dir`.
#[allow(
    clippy::too_many_arguments,
    reason = "joining forwards captured session inputs and optional presentation assets"
)]
fn start_network(
    commands: &mut Commands,
    menu: &MenuRuntime,
    session_generation: u64,
    cache: &BlobCache,
    endpoint: protocol::SessionEndpoint,
    #[cfg(feature = "native-networking")] native_account: Option<
        std::sync::Arc<launcher_host::native_account::NativeAccount>,
    >,
    #[cfg(feature = "native-networking")] native_friend: Option<
        crate::runtime::network::session::friend::FriendConnectionRequest,
    >,
    #[cfg(feature = "native-networking")] native_gathering: Option<
        launcher_host::native_account::NativeGatheringSelection,
    >,
    #[cfg(feature = "native-networking")] native_local_world: Option<
        crate::runtime::network::session::local_world::WorldConnectionRequest,
    >,
    login_settings: protocol::LoginSettings,
    client_random_id: protocol::ClientRandomId,
    actor_artwork: Option<&render_model::ActorArtworkPages>,
    ui_catalog: Option<&crate::runtime::network::PackUiCatalog>,
    registry_base: crate::runtime::network::session_registries::RegistryBase,
) -> Result<(), String> {
    bridge::join_profile::JoinProfile::current().mark("core_ready");
    let (runtime, tasks) = menu.io_execution().map_err(|error| error.to_string())?;
    let replacement = crate::runtime::network::session::spawn_network(
        NetworkConfig {
            registry_base,
            session_generation: simulation_clock::identity::SessionGeneration::new(
                session_generation,
            ),
            endpoint,
            #[cfg(feature = "native-networking")]
            native_account,
            #[cfg(feature = "native-networking")]
            native_friend,
            #[cfg(feature = "native-networking")]
            native_gathering,
            #[cfg(feature = "native-networking")]
            native_local_world,
            display_name: menu.display_name().to_owned(),
            client_random_id,
            client_blob_cache: cache.cache(),
            player_skin: menu.player_skin().clone(),
            login_settings,
            actor_artwork: actor_artwork.cloned(),
            ui_catalog: ui_catalog.map(|base| base.0.clone()),
            log_dir: menu.layout().log_dir(),
            record_session: menu.settings_snapshot().0.record_network_session(),
        },
        runtime,
        tasks,
    )
    .map_err(|error| error.to_string())?;
    commands.insert_resource(
        client_interaction::movement::MovementTicker::with_epoch_publisher(
            replacement.physics_epoch_publisher(),
        ),
    );
    commands.insert_resource(replacement);
    Ok(())
}
