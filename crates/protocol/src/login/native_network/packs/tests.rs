use super::*;
use bedrock_protocol_1_26_51::{
    types::{PackInfoData, PackInstanceId},
    wire,
};
use bytes::Bytes;
use resource_pack::test_support::archive_bytes;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Advertises a synthetic archive without sharing production server identities.
fn offer(id: uuid::Uuid, size: usize) -> PackInfoData {
    let mut offer = PackInfoData::default();
    offer.pack_id_version.pack_uuid = id;
    offer.pack_id_version.pack_version.version = "1.0.0".into();
    offer.pack_size = wire::U64LE(size as u64);
    offer
}

/// Wraps one requested fragment with its original index and resource name.
fn chunk(download: &Download, index: u32, bytes: &[u8]) -> packets::ResourcePackChunkData {
    packets::ResourcePackChunkData {
        resource_name: download.name.clone(),
        chunk_id: wire::U32LE(index),
        chunk_data: Bytes::copy_from_slice(bytes),
        ..Default::default()
    }
}

#[test]
fn chunk_window_accepts_reordering_without_serializing_the_tail_and_checks_hashes() {
    let bytes = vec![7; transfer::REQUEST_WINDOW as usize + 5];
    let mut download = Download::new(offer(uuid::Uuid::from_u128(1), bytes.len())).unwrap();
    let info = packets::ResourcePackDataInfo {
        resource_name: download.name.clone(),
        chunk_size: wire::U32LE(1),
        number_of_chunks: wire::U32LE(0),
        file_size: wire::U64LE(bytes.len() as u64),
        file_hash: Bytes::copy_from_slice(&Sha256::digest(&bytes)),
        ..Default::default()
    };
    let requests = download.begin(info.clone()).unwrap();
    assert_eq!(requests.len(), transfer::REQUEST_WINDOW as usize);
    let late = chunk(&download, transfer::REQUEST_WINDOW, &[7]);
    assert!(download.chunk(late).is_err());
    let mut replenished = Vec::new();
    for index in (0..transfer::REQUEST_WINDOW).rev() {
        let packet = chunk(&download, index, &[7]);
        replenished.extend(download.chunk(packet).unwrap());
    }
    assert_eq!(replenished.len(), 5);
    for index in transfer::REQUEST_WINDOW..bytes.len() as u32 {
        let packet = chunk(&download, index, &[7]);
        download.chunk(packet).unwrap();
    }
    assert_eq!(download.complete.as_deref().unwrap(), &bytes);
    let mut bad = Download::new(offer(uuid::Uuid::from_u128(1), bytes.len())).unwrap();
    let mut info = info;
    info.chunk_size = wire::U32LE(bytes.len() as u32);
    info.file_hash = Bytes::from_static(&[0; 32]);
    bad.begin(info).unwrap();
    let packet = chunk(&bad, 0, &bytes);
    assert!(bad.chunk(packet).is_err());
}

#[test]
fn final_stack_selects_order_subpack_and_offer_key_without_applying_unselected_archives() {
    let mut downloads = BTreeMap::new();
    for id in 1..=3 {
        let id = uuid::Uuid::from_u128(id);
        let bytes = archive_bytes(id, &[]);
        let mut offered = offer(id, bytes.len());
        offered.content_key = "synthetic-content-key".into();
        offered.subpack_name = "offer-subpack".into();
        let mut download = Download::new(offered).unwrap();
        download.complete = Some(Arc::new(bytes));
        downloads.insert(id, download);
    }
    let mut stack = packets::ResourcePackStack {
        texture_pack_list: [2, 1]
            .into_iter()
            .map(|id| PackInstanceId {
                pack_id: uuid::Uuid::from_u128(id).to_string().into(),
                version: "1.0.0".into(),
                sub_pack_name: "stack-subpack".into(),
            })
            .collect(),
        ..Default::default()
    };
    stack.texture_pack_list.insert(
        0,
        PackInstanceId {
            pack_id: uuid::Uuid::from_u128(2).to_string().into(),
            version: "9.9.9".into(),
            ..Default::default()
        },
    );
    let handoff = transfer::finish(downloads, stack, true).unwrap();
    assert!(handoff.required());
    let archives = handoff.into_archives();
    assert_eq!(
        archives
            .iter()
            .map(|archive| archive.pack_id.as_u128())
            .collect::<Vec<_>>(),
        [2, 1]
    );
    assert_eq!(archives[0].sub_pack_name, "stack-subpack");
    assert_eq!(archives[0].content_key.expose(), b"synthetic-content-key");
    let missing = PackInstanceId {
        pack_id: uuid::Uuid::from_u128(99).to_string().into(),
        version: "1.0.0".into(),
        ..Default::default()
    };
    let stack = packets::ResourcePackStack {
        texture_pack_list: vec![missing],
        ..Default::default()
    };
    assert!(
        transfer::finish(BTreeMap::new(), stack.clone(), false)
            .unwrap()
            .is_empty()
    );
    assert!(
        transfer::finish(
            BTreeMap::new(),
            packets::ResourcePackStack {
                texture_pack_required: true,
                ..stack
            },
            false
        )
        .is_err()
    );
}

/// Serves one HTTP response after a complete bounded request header.
async fn serve(listener: tokio::net::TcpListener, response: Vec<u8>) {
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        request.push(socket.read_u8().await.unwrap());
        assert!(request.len() < 8192);
    }
    socket.write_all(&response).await.unwrap();
}

#[tokio::test]
async fn cdn_accepts_original_archive_and_cache_hit_avoids_http_while_bad_body_falls_back() {
    let root = tempfile::tempdir().unwrap();
    let cache = acquisition::open_cache(Some(root.path())).await.unwrap();
    let cancelled = acquisition::Cancellation::new();
    let id = uuid::Uuid::from_u128(1);
    let bytes = archive_bytes(id, &[]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut offered = offer(id, bytes.len());
    offered.cdn_url = format!("http://{}/pack", listener.local_addr().unwrap()).into();
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    )
    .into_bytes();
    response.extend_from_slice(&bytes);
    let client = acquisition::http_client().unwrap();
    let (download, ()) = tokio::join!(
        acquisition::preload(
            Download::new(offered.clone()).unwrap(),
            Some(Arc::clone(&cache)),
            Some(client.clone()),
            Arc::clone(&cancelled.0)
        ),
        serve(listener, response)
    );
    assert_eq!(download.complete.as_deref().unwrap(), &bytes);
    let mut stores = acquisition::Stores::new();
    stores.queue(&Some(Arc::clone(&cache)), &download, &cancelled.0);
    stores.finish().await;
    // The fixture listener is closed; only the persistent cache can satisfy this retry.
    let hit = acquisition::preload(
        Download::new(offered).unwrap(),
        Some(cache),
        Some(client.clone()),
        Arc::clone(&cancelled.0),
    )
    .await;
    assert_eq!(hit.complete.as_deref().unwrap(), &bytes);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut offered = offer(uuid::Uuid::from_u128(2), 4);
    offered.cdn_url = format!("http://{}/oversized", listener.local_addr().unwrap()).into();
    let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\n12345\r\n0\r\n\r\n".to_vec();
    let (fallback, ()) = tokio::join!(
        acquisition::preload(
            Download::new(offered).unwrap(),
            None,
            Some(client),
            Arc::clone(&cancelled.0)
        ),
        serve(listener, response)
    );
    assert!(fallback.complete.is_none());
}

#[tokio::test]
async fn dropping_cdn_acquisition_closes_the_pending_body_without_cache_objects() {
    let root = tempfile::tempdir().unwrap();
    let cache = acquisition::open_cache(Some(root.path())).await.unwrap();
    let cancelled = acquisition::Cancellation::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut offered = offer(uuid::Uuid::from_u128(3), 1024);
    offered.cdn_url = format!("http://{}/pending", listener.local_addr().unwrap()).into();
    let (started, ready) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            header.push(socket.read_u8().await.unwrap());
            assert!(header.len() < 8192);
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\n")
            .await
            .unwrap();
        started.send(()).unwrap();
        assert_eq!(socket.read(&mut [0_u8; 1]).await.unwrap(), 0);
    });
    let mut preload = Box::pin(acquisition::preload(
        Download::new(offered).unwrap(),
        Some(cache),
        Some(acquisition::http_client().unwrap()),
        Arc::clone(&cancelled.0),
    ));
    tokio::select! {
        result = &mut preload => { drop(result); panic!("the fixture has not supplied its body"); }
        result = ready => result.unwrap(),
    }
    drop(preload);
    peer.await.unwrap();
    assert!(
        std::fs::read_dir(root.path().join("native-v1"))
            .unwrap()
            .all(|entry| entry.unwrap().file_name() == ".packcache.lock")
    );
}
