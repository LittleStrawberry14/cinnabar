//! Runtime-only acquisition around the reusable caller-driven pack negotiation.

mod acquisition;

use super::{Connection, disconnected, failure};
use crate::ProtocolError;
use bedrock_client::packs::{PackError, PackEvent, PackNegotiation, PackTransferLimits};
use bedrock_packs::ResourcePackHandoff;
use bedrock_protocol_1_26_51::packets;
use futures::{StreamExt, stream};
use std::{io, path::Path, sync::Arc};

const MAX_CDN_DOWNLOADS: usize = 4;

/// Tries cache and CDN before requesting missing archives on the retained game connection.
pub(super) async fn acquire(
    connection: &mut Connection,
    offer: packets::ResourcePacksInfo,
    cache_root: Option<&Path>,
) -> Result<ResourcePackHandoff, ProtocolError> {
    let cancelled = acquisition::Cancellation::new();
    let mut negotiation =
        PackNegotiation::new(offer, PackTransferLimits::default()).map_err(pack_failure)?;
    let offers = negotiation.offers().cloned().collect::<Vec<_>>();
    let cache = if offers.is_empty() {
        None
    } else {
        acquisition::open_cache(cache_root).await
    };
    let client = if offers.iter().any(|offer| !offer.cdn_url.is_empty()) {
        acquisition::http_client().ok()
    } else {
        None
    };
    let mut preloads = stream::iter(offers.into_iter().enumerate().map(|(index, offer)| {
        let (cache, client, cancelled) = (cache.clone(), client.clone(), Arc::clone(&cancelled.0));
        async move {
            let result = acquisition::preload(&offer, cache, client, cancelled).await;
            (index, offer, result)
        }
    }))
    .buffer_unordered(MAX_CDN_DOWNLOADS)
    .collect::<Vec<_>>()
    .await;
    preloads.sort_unstable_by_key(|(index, _, _)| *index);
    let mut stores = acquisition::Stores::new();
    for (_, offer, preload) in preloads {
        if cache.is_some() && !preload.cache_hit {
            stores.register(&offer);
        }
        if let Some(bytes) = preload.archive {
            negotiation
                .supply_preloaded(offer.pack_id_version.pack_uuid, bytes)
                .map_err(pack_failure)?;
        }
    }
    stores.queue_completed(&cache, &negotiation, &cancelled.0);
    connection
        .send(negotiation.start().map_err(pack_failure)?)
        .await?;
    loop {
        let packet = connection.next_packet().await.map_err(failure)?;
        if matches!(&*packet.data, packets::Packet::ResourcePackStack(_)) {
            // The library transfers exclusive archive ownership at the stack boundary.
            stores.finish().await;
        }
        match negotiation.receive(*packet.data).map_err(pack_failure)? {
            PackEvent::Send(packets) => {
                stores.queue_completed(&cache, &negotiation, &cancelled.0);
                send_packets(connection, packets).await?;
            }
            PackEvent::Finished { response, handoff } => {
                connection.send(response).await?;
                return Ok(handoff);
            }
            PackEvent::Unhandled(packet) => match *packet {
                packets::Packet::PlayStatus(status)
                    if status.status
                        == bedrock_protocol_1_26_51::enums::PlayStatusType::Loginsuccess => {}
                packets::Packet::Disconnect(packet) => return Err(disconnected(packet)),
                other => {
                    if let Some(error) = connection.negotiation_failure(other) {
                        return Err(error);
                    }
                }
            },
        }
    }
}

/// Sends each generated response immediately under the existing batch packet limit.
async fn send_packets(
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
