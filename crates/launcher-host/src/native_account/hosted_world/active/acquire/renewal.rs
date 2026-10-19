//! Fresh admission proof and ICE for the same protected host, without replacing established peers.

use super::super::{MessagingServer, NativeHostedWorld, driving, expired, unavailable};
use super::{bind_address, service_ice_servers};
use nethernet_tokio::ServerIceConfig;
use std::{io, time::Instant};

/// A complete renewal candidate; the coordinator installs it synchronously after fresh authority.
pub(in crate::native_account::hosted_world::active) struct Renewed {
    pub(in crate::native_account::hosted_world::active) identity: protocol::NativeHostingIdentity,
    pub(in crate::native_account::hosted_world::active) ice: ServerIceConfig,
    pub(in crate::native_account::hosted_world::active) expires: Instant,
}

/// Retains one original finite mint, TURN request and DNS operation while the same owners progress.
/// Cancellation requires closing the host rather than replaying an abandoned refresh receipt.
pub(in crate::native_account::hosted_world::active) async fn renew(
    world: &mut NativeHostedWorld,
    server: &mut MessagingServer,
    original: &protocol::NativeHostingIdentity,
    deadline: Instant,
) -> io::Result<Renewed> {
    let identity = {
        let NativeHostedWorld {
            directory, peers, ..
        } = world;
        let mint = directory
            .as_mut()
            .ok_or_else(unavailable)?
            .renew_hosting_identity(original, deadline);
        tokio::pin!(mint);
        driving(server, async {
            loop {
                tokio::select! { biased;
                    result = &mut mint => return result,
                    _ = peers.next() => {},
                }
            }
        })
        .await?
    };
    let deadline = deadline.min(identity.expires_at());
    let receipt = server
        .begin_turn_refresh(deadline)
        .map_err(|_| unavailable())?;
    let credentials = world
        .peer_io(Some(deadline), async {
            server
                .finish_turn_refresh(&receipt)
                .await
                .map_err(|_| unavailable())
        })
        .await?;
    let credential_expiry = credentials.expires_at();
    let expires = identity.expires_at().min(credential_expiry);
    let servers = service_ice_servers(&credentials);
    drop(credentials);
    let bind_addr = bind_address();
    let resolved = driving(
        server,
        world.peer_io(Some(deadline), async {
            nethernet_tokio::resolve_ice_servers(&servers, bind_addr, credential_expiry, deadline)
                .await
                .map_err(|_| unavailable())
        }),
    )
    .await?;
    if Instant::now() >= expires {
        return Err(expired());
    }
    let mut ice = ServerIceConfig {
        ice: Default::default(),
        bind_addr,
    };
    ice.ice.servers = resolved;
    Ok(Renewed {
        identity,
        ice,
        expires,
    })
}
