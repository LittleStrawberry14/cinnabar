use super::transfer::Download;
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

/// Borrows completed retained bytes; an excluded or pending fixture is a test failure.
fn completed_bytes(download: &Download) -> &[u8] {
    match &download.complete {
        Some(transfer::Completion::Archive(bytes)) => bytes,
        _ => panic!("expected a completed retained archive"),
    }
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
    let (mut downloads, mut budget) =
        transfer::Budget::offers(vec![offer(uuid::Uuid::from_u128(1), bytes.len())]);
    let mut download = downloads.remove(0);
    let info = packets::ResourcePackDataInfo {
        resource_name: download.name.clone(),
        chunk_size: wire::U32LE(1),
        number_of_chunks: wire::U32LE(0),
        file_size: wire::U64LE(bytes.len() as u64),
        file_hash: Bytes::copy_from_slice(&Sha256::digest(&bytes)),
        ..Default::default()
    };
    let requests = budget.begin(&mut download, info.clone()).unwrap();
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
    assert_eq!(completed_bytes(&download), &bytes);
    let (mut downloads, mut budget) =
        transfer::Budget::offers(vec![offer(uuid::Uuid::from_u128(1), bytes.len())]);
    let mut bad = downloads.remove(0);
    let mut info = info;
    info.chunk_size = wire::U32LE(bytes.len() as u32);
    info.file_hash = Bytes::from_static(&[0; 32]);
    budget.begin(&mut bad, info).unwrap();
    let packet = chunk(&bad, 0, &bytes);
    assert!(bad.chunk(packet).is_err());
}

#[test]
fn final_stack_selects_order_subpack_and_offer_key_without_applying_unselected_archives() {
    let mut downloads = BTreeMap::new();
    for id in 1..=3 {
        let id = uuid::Uuid::from_u128(id);
        let bytes = if id.as_u128() == 3 {
            vec![1, 2, 3]
        } else {
            archive_bytes(id, &[])
        };
        let mut offered = offer(id, bytes.len());
        offered.content_key = "synthetic-content-key".into();
        offered.subpack_name = "offer-subpack".into();
        let mut download = Download::new(offered).unwrap();
        download.complete = Some(transfer::Completion::Archive(Arc::new(bytes)));
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
    let handoff = transfer::finish(downloads, stack, false).unwrap();
    assert!(!handoff.required());
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
        texture_pack_list: vec![missing.clone()],
        ..Default::default()
    };
    assert!(
        transfer::finish(BTreeMap::new(), stack.clone(), false)
            .unwrap()
            .is_empty()
    );
    for selected in [
        vec![PackInstanceId {
            pack_id: "invalid-uuid".into(),
            ..missing.clone()
        }],
        vec![missing],
        vec![PackInstanceId {
            pack_id: uuid::Uuid::from_u128(1).to_string().into(),
            version: "9.9.9".into(),
            ..Default::default()
        }],
        vec![
            PackInstanceId {
                pack_id: uuid::Uuid::from_u128(1).to_string().into(),
                version: "1.0.0".into(),
                ..Default::default()
            };
            2
        ],
    ] {
        for (offer_required, stack_required) in [(true, false), (false, true), (true, true)] {
            let id = uuid::Uuid::from_u128(1);
            let bytes = archive_bytes(id, &[]);
            let mut download = Download::new(offer(id, bytes.len())).unwrap();
            download.complete = Some(transfer::Completion::Archive(Arc::new(bytes)));
            let stack = packets::ResourcePackStack {
                texture_pack_required: stack_required,
                texture_pack_list: selected.clone(),
                ..Default::default()
            };
            assert!(transfer::finish([(id, download)].into(), stack, offer_required).is_err());
        }
    }
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
    assert_eq!(completed_bytes(&download), &bytes);
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
    assert_eq!(completed_bytes(&hit), &bytes);
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
        match socket.read(&mut [0_u8; 1]).await {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                ) => {}
            result => panic!("canceled download must close its pending body: {result:?}"),
        }
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
    tokio::time::timeout(std::time::Duration::from_secs(5), peer)
        .await
        .expect("canceled download retained its peer socket")
        .unwrap();
    assert!(
        std::fs::read_dir(root.path().join("native-v1"))
            .unwrap()
            .all(|entry| entry.unwrap().file_name() == ".packcache.lock")
    );
}

#[test]
fn optional_content_admission_and_transfer_budget_are_separate() {
    let id = uuid::Uuid::from_u128(1);
    let bytes = b"unsupported archive bytes";
    let mut oversized = offer(uuid::Uuid::from_u128(99), 0);
    oversized.pack_size = wire::U64LE(u64::MAX);
    let mut unusable = offer(uuid::Uuid::from_u128(98), 1);
    unusable.pack_id_version.pack_version.version = "".into();
    let (mut downloads, mut budget) =
        transfer::Budget::offers(vec![oversized, unusable, offer(id, bytes.len())]);
    assert_eq!(downloads.len(), 1);
    let mut download = downloads.remove(0);
    let info = packets::ResourcePackDataInfo {
        resource_name: download.name.clone(),
        chunk_size: wire::U32LE(bytes.len() as u32),
        file_size: wire::U64LE(bytes.len() as u64),
        file_hash: Bytes::copy_from_slice(&Sha256::digest(bytes)),
        ..Default::default()
    };
    budget.begin(&mut download, info).unwrap();
    let packet = chunk(&download, 0, bytes);
    download.chunk(packet).unwrap();
    let stack = packets::ResourcePackStack {
        texture_pack_list: vec![PackInstanceId {
            pack_id: id.to_string().into(),
            version: "1.0.0".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let handoff = transfer::finish([(id, download)].into(), stack.clone(), false).unwrap();
    assert!(!handoff.required());
    let admitted = resource_pack::validate_handoff(handoff);
    assert!(admitted.packs().is_empty());
    assert_eq!(admitted.rejections().len(), 1);

    let (bounded, _) = transfer::Budget::offers(
        (1..=resource_pack::MAX_PACKS + 1)
            .map(|index| offer(uuid::Uuid::from_u128(index as u128), 1))
            .collect(),
    );
    assert_eq!(bounded.len(), resource_pack::MAX_PACKS);

    for (offer_required, stack_required) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        let (mut downloads, mut budget) = transfer::Budget::offers(vec![
            offer(id, 1),
            offer(uuid::Uuid::from_u128(2), resource_pack::MAX_ARCHIVE_BYTES),
            offer(
                uuid::Uuid::from_u128(3),
                resource_pack::MAX_STACK_ARCHIVE_BYTES - resource_pack::MAX_ARCHIVE_BYTES - 1,
            ),
            offer(uuid::Uuid::from_u128(4), 1),
        ]);
        assert_eq!(downloads.len(), 3);
        let mut download = downloads.remove(0);
        let bytes = b"12";
        let info = packets::ResourcePackDataInfo {
            resource_name: download.name.clone(),
            chunk_size: wire::U32LE(2),
            file_size: wire::U64LE(2),
            file_hash: Bytes::copy_from_slice(&Sha256::digest(bytes)),
            ..Default::default()
        };
        budget.begin(&mut download, info).unwrap();
        let packet = chunk(&download, 0, bytes);
        download.chunk(packet).unwrap();
        assert!(matches!(
            &download.complete,
            Some(transfer::Completion::Excluded)
        ));
        let stack = packets::ResourcePackStack {
            texture_pack_required: stack_required,
            ..stack.clone()
        };
        let result = transfer::finish([(id, download)].into(), stack, offer_required);
        if offer_required || stack_required {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_empty());
        }
    }

    let (mut downloads, mut budget) = transfer::Budget::offers(vec![offer(id, 1)]);
    let mut download = downloads.remove(0);
    let info = packets::ResourcePackDataInfo {
        resource_name: download.name.clone(),
        chunk_size: wire::U32LE(1),
        file_size: wire::U64LE(transfer::MAX_TRANSFER_BYTES + 1),
        file_hash: Bytes::from_static(&[0; 32]),
        ..Default::default()
    };
    assert!(budget.begin(&mut download, info).is_err());
}

#[test]
fn retained_metadata_and_out_of_order_fragment_release_the_shared_decoded_batches() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct BatchOwner {
        bytes: Vec<u8>,
        dropped: Arc<AtomicBool>,
    }
    impl AsRef<[u8]> for BatchOwner {
        fn as_ref(&self) -> &[u8] {
            &self.bytes
        }
    }
    impl Drop for BatchOwner {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Relaxed);
        }
    }
    let bytes = [7, 7];
    let (mut downloads, mut budget) =
        transfer::Budget::offers(vec![offer(uuid::Uuid::from_u128(1), bytes.len())]);
    let mut download = downloads.remove(0);
    let dropped_name = Arc::new(AtomicBool::new(false));
    let mut metadata = vec![0; 64 * 1024];
    let name_length = download.name.len();
    metadata[..name_length].copy_from_slice(download.name.as_bytes());
    let batch = Bytes::from_owner(BatchOwner {
        bytes: metadata,
        dropped: Arc::clone(&dropped_name),
    });
    let name = batch.slice(..name_length).into();
    drop(batch);
    budget
        .begin(
            &mut download,
            packets::ResourcePackDataInfo {
                resource_name: name,
                chunk_size: wire::U32LE(1),
                file_size: wire::U64LE(bytes.len() as u64),
                file_hash: Bytes::copy_from_slice(&Sha256::digest(bytes)),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(dropped_name.load(Ordering::Relaxed));
    let dropped = Arc::new(AtomicBool::new(false));
    let batch = Bytes::from_owner(BatchOwner {
        bytes: vec![7; 64 * 1024],
        dropped: Arc::clone(&dropped),
    });
    let packet = packets::ResourcePackChunkData {
        resource_name: download.name.clone(),
        chunk_id: wire::U32LE(1),
        chunk_data: batch.slice(123..124),
        ..Default::default()
    };
    drop(batch);
    download.chunk(packet).unwrap();
    assert!(dropped.load(Ordering::Relaxed));
    let packet = chunk(&download, 0, &bytes[..1]);
    download.chunk(packet).unwrap();
    assert_eq!(completed_bytes(&download), bytes);
}
