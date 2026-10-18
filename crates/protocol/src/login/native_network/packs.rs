//! Runtime-only acquisition around the reusable caller-driven pack negotiation.

mod acquisition;

use super::{Connection, PacketConnection, apply_key_exchange, failure, handshake_failure};
use crate::ProtocolError;
use bedrock_client::{
    handshake::{ClientPackOffer, PacksEvent},
    packs::PackError,
};
use bedrock_packs::ResourcePackHandoff;
use bedrock_protocol_1_26_51::packets;
use futures::{StreamExt, stream};
use std::{io, path::Path, sync::Arc};

const MAX_CDN_DOWNLOADS: usize = 4;

#[cfg(test)]
tokio::task_local! {
    /// Reports real HTTP attempts only within one fixture-owned native join future.
    pub(super) static HTTP_ATTEMPTS: tokio::sync::mpsc::Sender<uuid::Uuid>;
}

/// Tries cache and CDN before requesting missing archives on the retained game connection.
pub(super) async fn acquire(
    connection: &mut Connection,
    mut preloading: ClientPackOffer,
    cache_root: Option<&Path>,
) -> Result<ResourcePackHandoff, ProtocolError> {
    let cancelled = acquisition::Cancellation::new();
    let offers = preloading.offers().cloned().collect::<Vec<_>>();
    let cache = if offers.is_empty() {
        None
    } else {
        wait_io(connection, acquisition::open_cache(cache_root)).await
    };
    let client = if offers.iter().any(|offer| !offer.cdn_url.is_empty()) {
        acquisition::http_client().ok()
    } else {
        None
    };
    let preloads = stream::iter(offers.into_iter().enumerate().map(|(index, offer)| {
        let (cache, client, cancelled) = (cache.clone(), client.clone(), Arc::clone(&cancelled.0));
        async move {
            let result = acquisition::preload(&offer, cache, client, cancelled).await;
            (index, offer, result)
        }
    }))
    .buffer_unordered(MAX_CDN_DOWNLOADS)
    .collect::<Vec<_>>();
    let mut preloads = wait_io(connection, preloads).await;
    preloads.sort_unstable_by_key(|(index, _, _)| *index);
    let mut stores = acquisition::Stores::new();
    for (_, offer, preload) in preloads {
        if cache.is_some() && !preload.cache_hit {
            stores.register(&offer);
        }
        if let Some(bytes) = preload.archive {
            preloading
                .supply_preloaded(offer.pack_id_version.pack_uuid, bytes)
                .map_err(pack_failure)?;
        }
    }
    let (mut negotiation, response) = preloading.start().map_err(pack_failure)?;
    stores.queue_completed(&cache, |id| negotiation.completed_archive(id), &cancelled.0);
    connection.send(response).await?;
    loop {
        let packet = connection.next_packet().await.map_err(failure)?;
        if matches!(&*packet.data, packets::Packet::ResourcePackStack(_)) {
            // The library transfers exclusive archive ownership at the stack boundary.
            wait_io(connection, stores.finish()).await;
        }
        match negotiation
            .receive(*packet.data)
            .map_err(handshake_failure)?
        {
            PacksEvent::Send { packs, packets } => {
                negotiation = packs;
                stores.queue_completed(
                    &cache,
                    |id| negotiation.completed_archive(id),
                    &cancelled.0,
                );
                send_packets(connection, packets).await?;
            }
            PacksEvent::Finished(complete) => {
                let (response, handoff) = complete.into_parts();
                connection.send(response).await?;
                return Ok(handoff);
            }
            PacksEvent::KeyExchange(exchange) => {
                negotiation = apply_key_exchange(connection, exchange).await?;
            }
            PacksEvent::Unhandled { packs, packet } => {
                connection.defer_unhandled(*packet)?;
                negotiation = packs;
            }
        }
    }
}

/// Drives service work beside one retained I/O wait; ordinary reads still own staged terminals.
async fn wait_io<T>(
    connection: &mut impl PacketConnection,
    operation: impl std::future::Future<Output = T>,
) -> T {
    tokio::pin!(operation);
    loop {
        tokio::select! {
            biased;
            () = connection.maintain() => {},
            output = &mut operation => return output,
        }
    }
}

/// Sends each generated response immediately under the existing batch packet limit.
pub(super) async fn send_packets(
    connection: &mut Connection,
    packets: Vec<packets::Packet>,
) -> Result<(), ProtocolError> {
    let packets = packets
        .into_iter()
        .map(crate::Packet::new)
        .collect::<Vec<_>>();
    for batch in packets.chunks(bedrock_net::BatchLimits::default().max_packets) {
        connection.send_packets(batch).await?;
    }
    Ok(())
}

/// Retains the existing native transport failure classification without exposing server data.
fn pack_failure(error: PackError) -> ProtocolError {
    failure(io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests;
