use super::*;
use bedrock_client::packs::{PackEvent, PackOffer, PackTransferLimits};
use bedrock_protocol_1_26_51::types::{PackInfoData, PackInstanceId};

/// Uses the real preload/final-stack owner, retaining the absence of upstream chunk metadata.
fn preloaded(archive: Vec<u8>, id: uuid::Uuid) -> packs::Acquired {
    let mut offered = PackInfoData::default();
    offered.pack_id_version.pack_uuid = id;
    offered.pack_id_version.pack_version.version = "1.0.0".into();
    offered.pack_size = wire::U64LE(archive.len() as u64);
    offered.cdn_url = "https://example.invalid/original.zip".into();
    let mut offer = PackOffer::new_with_metadata(
        packets::ResourcePacksInfo {
            resource_packs: vec![offered],
            texture_pack_required: true,
            ..Default::default()
        },
        PackTransferLimits::default(),
    )
    .unwrap();
    offer.supply_preloaded(id, archive).unwrap();
    let (mut transfer, _) = offer.start().unwrap();
    let PackEvent::Finished(complete) = transfer
        .receive(
            packets::ResourcePackStack {
                texture_pack_list: vec![PackInstanceId {
                    pack_id: id.to_string().into(),
                    version: "1.0.0".into(),
                    ..Default::default()
                }],
                texture_pack_required: true,
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
    else {
        panic!("real preloaded stack did not complete");
    };
    let (_, handoff, metadata) = complete.into_parts_with_metadata();
    packs::Acquired {
        handoff,
        metadata: metadata.map(Box::new),
    }
}

#[test]
fn hosted_snapshot_accepts_actual_preloaded_archive_without_invented_data_info() {
    let id = uuid::Uuid::from_u128(1);
    let original = resource_pack::test_support::archive_bytes(id, &[]);
    resource_pack::verify_archive_identity(&original, id, "1.0.0").unwrap();
    let acquired = preloaded(original.clone(), id);
    assert!(matches!(
        acquired.metadata.as_ref().unwrap().data.as_slice(),
        [None]
    ));
    let result = snapshot(acquired);
    assert!(
        result.is_ok(),
        "a verified original preloaded archive required nonexistent upstream DataInfo"
    );
    let (snapshot, archives) = result.unwrap();
    assert_eq!(archives.as_slice(), &[Bytes::from(original)]);
    assert_eq!(
        snapshot.offer.resource_packs[0].pack_id_version.pack_uuid,
        id
    );
    assert_eq!(snapshot.contents.len(), 1);
}
