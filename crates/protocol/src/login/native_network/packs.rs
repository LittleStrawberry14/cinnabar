//! Bounded native CDN/chunk acquisition and persistent original archive caching.

mod acquisition;
mod builtins;
mod transfer;

use super::{Connection, disconnected, failure};
use crate::ProtocolError;
use bedrock_protocol_1_26_51::{packets, types::ResourcePackClientResponseData};
use futures::{StreamExt, stream};
use resource_pack::ResourcePackHandoff;
use std::{collections::BTreeMap, io, path::Path, sync::Arc};
use transfer::Download;

const MAX_CDN_DOWNLOADS: usize = 4;

/// Tries cache and CDN before requesting missing archives over the game connection.
/// The same connection and FIFO survive every response and final stack transition.
pub(super) async fn acquire(
    connection: &mut Connection,
    offer: packets::ResourcePacksInfo,
    cache_root: Option<&Path>,
) -> Result<ResourcePackHandoff, ProtocolError> {
    let cancelled = acquisition::Cancellation::new();
    let mut downloads = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut total = 0_u64;
    let mut skipped_offers = 0_usize;
    for pack in offer.resource_packs {
        let id = pack.pack_id_version.pack_uuid;
        if !ids.insert(id) {
            skipped_offers += 1;
            continue;
        }
        total = total
            .checked_add(pack.pack_size.0)
            .ok_or_else(|| invalid("pack sizes overflow"))?;
        if downloads.len() >= resource_pack::MAX_PACKS
            || pack.pack_size.0 > resource_pack::MAX_ARCHIVE_BYTES as u64
            || total > resource_pack::MAX_STACK_ARCHIVE_BYTES as u64
        {
            return Err(invalid("pack offer exceeds download budget"));
        }
        downloads.push(Download::new(pack)?);
    }
    if skipped_offers != 0 {
        tracing::warn!(skipped_offers, "skipped duplicate native pack offers");
    }
    let cache = if downloads.is_empty() {
        None
    } else {
        acquisition::open_cache(cache_root).await
    };
    let client = if downloads
        .iter()
        .any(|download| !download.offer.cdn_url.is_empty())
    {
        acquisition::http_client().ok()
    } else {
        None
    };
    let mut preloads = stream::iter(downloads.into_iter().enumerate().map(|(index, download)| {
        let preload = acquisition::preload(
            download,
            cache.clone(),
            client.clone(),
            Arc::clone(&cancelled.0),
        );
        async move { (index, preload.await) }
    }))
    .buffer_unordered(MAX_CDN_DOWNLOADS)
    .collect::<Vec<_>>()
    .await;
    preloads.sort_unstable_by_key(|(index, _)| *index);
    let mut downloads = BTreeMap::new();
    let mut requested = Vec::new();
    let mut stores = acquisition::Stores::new();
    for (_, download) in preloads {
        if download.complete.is_some() {
            stores.queue(&cache, &download, &cancelled.0);
        } else {
            requested.push(download.name.clone());
        }
        downloads.insert(download.offer.pack_id_version.pack_uuid, download);
    }
    let response = if requested.is_empty() {
        finished()
    } else {
        ResourcePackClientResponseData::Downloading {
            response_type: "downloading".into(),
            downloading_packs: requested,
        }
    };
    connection
        .send(packets::ResourcePackClientResponse { response })
        .await?;
    loop {
        let packet = connection.next_packet().await.map_err(failure)?;
        match *packet.data {
            packets::Packet::ResourcePackDataInfo(info) => {
                let id = pack_id(
                    info.resource_name
                        .to_str()
                        .map_err(|_| invalid("pack identity is not UTF-8"))?,
                )?;
                let download = downloads
                    .get_mut(&id)
                    .ok_or_else(|| invalid("pack data info was not advertised"))?;
                total = total
                    .checked_sub(download.size)
                    .and_then(|sum| sum.checked_add(info.file_size.0))
                    .ok_or_else(|| invalid("pack sizes overflow"))?;
                if total > resource_pack::MAX_STACK_ARCHIVE_BYTES as u64 {
                    return Err(invalid("pack transfer exceeds total budget"));
                }
                send_requests(connection, download.begin(info)?).await?;
            }
            packets::Packet::ResourcePackChunkData(chunk) => {
                let id = pack_id(
                    chunk
                        .resource_name
                        .to_str()
                        .map_err(|_| invalid("pack identity is not UTF-8"))?,
                )?;
                let download = downloads
                    .get_mut(&id)
                    .ok_or_else(|| invalid("chunk pack was not advertised"))?;
                let complete_before = download.complete.is_some();
                send_requests(connection, download.chunk(chunk)?).await?;
                if !complete_before && download.complete.is_some() {
                    let bytes = Arc::clone(
                        download
                            .complete
                            .as_ref()
                            .expect("completed chunk owns bytes"),
                    );
                    acquisition::verify(
                        bytes,
                        id,
                        download
                            .offer
                            .pack_id_version
                            .pack_version
                            .version
                            .to_string_lossy()
                            .into_owned(),
                    )
                    .await
                    .map_err(failure)?;
                    stores.queue(&cache, download, &cancelled.0);
                    if downloads
                        .values()
                        .all(|download| download.complete.is_some())
                    {
                        connection
                            .send(packets::ResourcePackClientResponse {
                                response: finished(),
                            })
                            .await?;
                    }
                }
            }
            packets::Packet::ResourcePackStack(stack) => {
                // Cache workers release their shared bytes before the application takes ownership.
                stores.finish().await;
                let handoff = transfer::finish(downloads, stack, offer.resource_pack_required)?;
                connection
                    .send(packets::ResourcePackClientResponse {
                        response: ResourcePackClientResponseData::ResourcePackStackFinished {
                            response_type: "resourcepackstackfinished".into(),
                        },
                    })
                    .await?;
                return Ok(handoff);
            }
            packets::Packet::PlayStatus(status)
                if status.status
                    == bedrock_protocol_1_26_51::enums::PlayStatusType::Loginsuccess => {}
            packets::Packet::Disconnect(packet) => return Err(disconnected(packet)),
            other => {
                if let Some(error) = connection.negotiation_failure(other) {
                    return Err(error);
                }
            }
        }
    }
}

/// Coalesces each request window into immediate batches under the shared packet-count limit.
async fn send_requests(
    connection: &mut Connection,
    requests: Vec<packets::ResourcePackChunkRequest>,
) -> Result<(), ProtocolError> {
    let packets = requests
        .into_iter()
        .map(crate::Packet::new)
        .collect::<Vec<_>>();
    for batch in packets.chunks(bedrock_net::BatchLimits::default().max_packets) {
        connection.send_packets(batch).await?;
    }
    Ok(())
}

/// Produces the completion response for both an all-cached offer and the final chunk.
fn finished() -> ResourcePackClientResponseData {
    ResourcePackClientResponseData::DownloadingFinished {
        response_type: "downloadingfinished".into(),
    }
}

/// Accepts UUID or UUID_version resource names without treating server text as a path.
fn pack_id(name: &str) -> Result<uuid::Uuid, ProtocolError> {
    uuid::Uuid::parse_str(name.split('_').next().unwrap_or(name))
        .map_err(|_| invalid("pack identity is not a UUID"))
}

/// Reports malformed pack framing without including keys, URLs or archive contents.
fn invalid(message: &'static str) -> ProtocolError {
    failure(io::Error::new(io::ErrorKind::InvalidData, message))
}

#[cfg(test)]
mod tests;
