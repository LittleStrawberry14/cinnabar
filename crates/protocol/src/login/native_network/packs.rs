//! Bounded in-band pack acquisition and server-selected stack ordering.

mod builtins;

use super::{Connection, disconnected, failure};
use crate::{ProtocolError, ResourcePackArchive, ResourcePackHandoff};
use bedrock_protocol_1_26_51::{
    packets,
    types::{PackInfoData, ResourcePackClientResponseData},
    wire,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io};

const MAX_PACK_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PACK_TOTAL_BYTES: u64 = MAX_PACK_BYTES * 4;

struct Download {
    offer: PackInfoData,
    name: wire::Str,
    size: u64,
    chunk_size: u32,
    hash: Vec<u8>,
    next_chunk: u32,
    bytes: Vec<u8>,
    complete: bool,
}

/// Downloads advertised packs over the game connection, verifies each SHA-256,
/// and returns only the archives selected by the server's final stack.
pub(super) async fn acquire(
    connection: &mut Connection,
    offer: packets::ResourcePacksInfo,
) -> Result<ResourcePackHandoff, ProtocolError> {
    let mut downloads = BTreeMap::new();
    let mut total = 0_u64;
    let mut requested = Vec::new();
    for pack in offer.resource_packs {
        let id = pack.pack_id_version.pack_uuid;
        if downloads.contains_key(&id) {
            tracing::warn!(%id, "skipped duplicate native pack offer");
            continue;
        }
        total = total
            .checked_add(pack.pack_size.0)
            .ok_or_else(|| invalid("pack sizes overflow"))?;
        if pack.pack_size.0 > MAX_PACK_BYTES || total > MAX_PACK_TOTAL_BYTES {
            return Err(invalid("pack offer exceeds download budget"));
        }
        let name: wire::Str = format!(
            "{id}_{}",
            pack.pack_id_version.pack_version.version.to_string_lossy()
        )
        .into();
        requested.push(name.clone());
        downloads.insert(
            id,
            Download {
                size: pack.pack_size.0,
                offer: pack,
                name,
                chunk_size: 0,
                hash: Vec::new(),
                next_chunk: 0,
                bytes: Vec::new(),
                complete: false,
            },
        );
    }
    let response = if requested.is_empty() {
        ResourcePackClientResponseData::DownloadingFinished {
            response_type: "downloadingfinished".into(),
        }
    } else {
        ResourcePackClientResponseData::Downloading {
            response_type: "downloading".into(),
            downloading_packs: requested,
        }
    };
    connection
        .send(packets::ResourcePackClientResponse { response })
        .await
        .map_err(failure)?;
    loop {
        let packet = connection.next_packet().await.map_err(failure)?;
        match *packet.data {
            packets::Packet::ResourcePackDataInfo(info) => {
                let id = pack_id(
                    info.resource_name
                        .to_str()
                        .map_err(io::Error::other)
                        .map_err(failure)?,
                )?;
                let download = downloads
                    .get_mut(&id)
                    .ok_or_else(|| invalid("pack data info was not advertised"))?;
                if download.chunk_size != 0 || download.complete {
                    return Err(invalid("repeated pack data info"));
                }
                total = total
                    .checked_sub(download.size)
                    .and_then(|sum| sum.checked_add(info.file_size.0))
                    .ok_or_else(|| invalid("pack sizes overflow"))?;
                if info.file_size.0 == 0
                    || info.file_size.0 > MAX_PACK_BYTES
                    || total > MAX_PACK_TOTAL_BYTES
                    || info.chunk_size.0 == 0
                    || info.file_hash.len() != 32
                {
                    return Err(invalid("invalid pack size, chunk size or SHA-256"));
                }
                download.size = info.file_size.0;
                download.chunk_size = info.chunk_size.0;
                download.hash = info.file_hash.to_vec();
                download.name = info.resource_name;
                request(connection, download).await?;
            }
            packets::Packet::ResourcePackChunkData(chunk) => {
                let id = pack_id(
                    chunk
                        .resource_name
                        .to_str()
                        .map_err(io::Error::other)
                        .map_err(failure)?,
                )?;
                let download = downloads
                    .get_mut(&id)
                    .ok_or_else(|| invalid("chunk pack was not advertised"))?;
                if chunk.chunk_id.0 < download.next_chunk {
                    tracing::debug!(%id, "skipped duplicate pack chunk");
                    continue;
                }
                if download.chunk_size == 0
                    || download.complete
                    || chunk.chunk_id.0 != download.next_chunk
                {
                    return Err(invalid("unexpected pack chunk index"));
                }
                let remaining = download
                    .size
                    .checked_sub(download.bytes.len() as u64)
                    .ok_or_else(|| invalid("pack exceeded declared size"))?;
                let expected = remaining.min(download.chunk_size.into());
                if chunk.chunk_data.len() as u64 != expected {
                    return Err(invalid("pack chunk length does not match remaining data"));
                }
                download
                    .bytes
                    .try_reserve(chunk.chunk_data.len())
                    .map_err(io::Error::other)
                    .map_err(failure)?;
                download.bytes.extend_from_slice(&chunk.chunk_data);
                download.next_chunk = download
                    .next_chunk
                    .checked_add(1)
                    .ok_or_else(|| invalid("pack chunk index overflow"))?;
                if download.bytes.len() as u64 == download.size {
                    if Sha256::digest(&download.bytes).as_slice() != download.hash {
                        return Err(invalid("downloaded pack SHA-256 mismatch"));
                    }
                    download.complete = true;
                    if downloads.values().all(|download| download.complete) {
                        connection
                            .send(packets::ResourcePackClientResponse {
                                response: ResourcePackClientResponseData::DownloadingFinished {
                                    response_type: "downloadingfinished".into(),
                                },
                            })
                            .await
                            .map_err(failure)?;
                    }
                } else {
                    request(connection, download).await?;
                }
            }
            packets::Packet::ResourcePackStack(stack) => {
                if downloads.values().any(|download| !download.complete) {
                    return Err(invalid("pack stack preceded completed downloads"));
                }
                let mut archives = Vec::with_capacity(stack.texture_pack_list.len());
                for pack in stack.texture_pack_list {
                    let id = pack_id(
                        pack.pack_id
                            .to_str()
                            .map_err(io::Error::other)
                            .map_err(failure)?,
                    )?;
                    if !downloads.contains_key(&id)
                        && builtins::contains(id, &pack.version.to_string_lossy())
                    {
                        continue;
                    }
                    let download = downloads
                        .remove(&id)
                        .ok_or_else(|| invalid("stack contains an unavailable pack"))?;
                    if download.offer.pack_id_version.pack_version.version != pack.version {
                        return Err(invalid("stack pack version differs from offer"));
                    }
                    archives.push(ResourcePackArchive::with_content_key(
                        id,
                        pack.version.to_string_lossy().into_owned(),
                        pack.sub_pack_name.to_string_lossy().into_owned(),
                        download.bytes,
                        download.offer.content_key.as_bytes().to_vec(),
                    ));
                }
                connection
                    .send(packets::ResourcePackClientResponse {
                        response: ResourcePackClientResponseData::ResourcePackStackFinished {
                            response_type: "resourcepackstackfinished".into(),
                        },
                    })
                    .await
                    .map_err(failure)?;
                return Ok(ResourcePackHandoff::from_archives(archives)
                    .with_required(offer.resource_pack_required || stack.texture_pack_required));
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

/// Requests only the next chunk for this pack; independent packs may progress concurrently.
async fn request(connection: &mut Connection, download: &Download) -> Result<(), ProtocolError> {
    let chunk = i32::try_from(download.next_chunk)
        .map_err(|_| invalid("pack chunk count exceeds wire range"))?;
    connection
        .send(packets::ResourcePackChunkRequest {
            resource_name: download.name.clone(),
            chunk: wire::I32LE(chunk),
        })
        .await
        .map_err(failure)
}

/// Accepts the protocol's UUID or UUID_version resource name without treating it as a path.
fn pack_id(name: &str) -> Result<uuid::Uuid, ProtocolError> {
    uuid::Uuid::parse_str(name.split('_').next().unwrap_or(name))
        .map_err(|_| invalid("pack identity is not a UUID"))
}

/// Reports a malformed pack transfer while retaining its original error context.
fn invalid(message: &str) -> ProtocolError {
    failure(io::Error::new(
        io::ErrorKind::InvalidData,
        message.to_owned(),
    ))
}
