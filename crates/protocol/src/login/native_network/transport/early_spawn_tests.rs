use super::super::{LoginSequence, SessionLogin};
use super::*;
use bedrock_net::{
    BatchCodec, BatchLimits, CompressionAlgorithm, CompressionSettings, DecodeOutcome,
};
use bedrock_protocol_1_26_51::{enums::PlayStatusType, packets, types::ActorRuntimeID, wire};
use nethernet_tokio::{AcceptedSession, NetherServer, ServerData};
use std::time::Duration;

/// Sends generated packets through the same bounded batch codec used by native connections.
pub(in crate::login::native_network) async fn send<S: bedrock_net::BatchPhase>(
    peer: &AcceptedSession,
    codec: &BatchCodec<S>,
    packets: &[crate::Packet],
) {
    let body = crate::session_transport::batch_body(packets).unwrap();
    let entries = wire::Frames::new(body, BatchLimits::default().max_packets)
        .map(|entry| {
            let entry = entry.unwrap();
            let mut reader = wire::Reader::from_shared(entry.frame());
            reader.read_var_u32().unwrap();
            entry.frame().slice(reader.position()..)
        })
        .collect::<Vec<_>>();
    for message in codec.encode(&entries).unwrap() {
        peer.session.send(message).await.unwrap();
    }
}

/// Reads one real reliable message and decodes its original generated packet entries.
pub(in crate::login::native_network) async fn receive<S: bedrock_net::BatchPhase>(
    peer: &mut AcceptedSession,
    codec: &BatchCodec<S>,
) -> Vec<crate::Packet> {
    let message = peer.reliable.recv().await.unwrap().unwrap();
    let DecodeOutcome::Packets { packets, .. } = codec.decode(message).unwrap() else {
        panic!("the native client must send a valid batch");
    };
    let mut batch = vec![wire::BATCH_HEADER];
    for packet in packets {
        wire::Writer::with_limit(&mut batch, BatchLimits::default().max_decoded_bytes)
            .write_len_var_u32(packet.len())
            .unwrap();
        batch.extend_from_slice(&packet);
    }
    crate::codec::decode_batch(Bytes::from(batch)).unwrap()
}

#[tokio::test]
async fn native_pack_deferred_spawn_reaches_startup_once_with_siblings() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let reservation = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let signaling = LanSignaling::with_config(
            31,
            address,
            LanConfig {
                discovery_port: address.port(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut data = ServerData::new("Fixture".into(), "World".into());
        data.flag_b = true;
        data.session_id = "synthetic-nonce".into();
        data.connection_type = LAN_CONNECTION_TYPE;
        data.protocol_version = crate::PROTOCOL_VERSION as u32;
        signaling.set_server_data(data);
        let mut listener = NetherServer::bind_with(
            signaling,
            ConnectionConfig {
                allow_unsigned_answers: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let first = crate::Packet::new(packets::SetTime {
            time: wire::ZigZag32(1),
        });
        let second = crate::Packet::new(packets::SetTime {
            time: wire::ZigZag32(2),
        });
        let third = crate::Packet::new(packets::SetTime {
            time: wire::ZigZag32(3),
        });
        let radius = crate::Packet::new(packets::ChunkRadiusUpdated {
            chunk_radius: wire::ZigZag32(8),
        });
        let server = async {
            let mut peer = listener.accept().await.unwrap();
            let codec = BatchCodec::new(BatchHeader::NetherNet, BatchLimits::default());
            assert!(matches!(
                &*receive(&mut peer, &codec).await[0].data,
                packets::Packet::RequestNetworkSettings(_)
            ));
            send(
                &peer,
                &codec,
                &[crate::Packet::new(packets::NetworkSettings {
                    compression_threshold: wire::U16LE(u16::MAX),
                    ..Default::default()
                })],
            )
            .await;
            let codec = codec.negotiate(CompressionSettings {
                algorithm: CompressionAlgorithm::Deflate,
                threshold: u16::MAX,
            });
            assert!(matches!(
                &*receive(&mut peer, &codec).await[0].data,
                packets::Packet::Login(_)
            ));
            send(
                &peer,
                &codec,
                &[
                    crate::Packet::new(packets::PlayStatus {
                        status: PlayStatusType::Loginsuccess,
                    }),
                    crate::Packet::new(packets::ResourcePacksInfo::default()),
                ],
            )
            .await;
            assert!(matches!(
                &*receive(&mut peer, &codec).await[0].data,
                packets::Packet::ClientCacheStatus(_)
            ));
            assert!(matches!(
                &*receive(&mut peer, &codec).await[0].data,
                packets::Packet::ResourcePackClientResponse(_)
            ));
            send(
                &peer,
                &codec,
                &[
                    crate::Packet::new(packets::PlayStatus {
                        status: PlayStatusType::Playerspawn,
                    }),
                    first.clone(),
                    second.clone(),
                    crate::Packet::new(packets::ResourcePackStack::default()),
                    crate::Packet::new(packets::StartGame {
                        runtime_id: ActorRuntimeID(42),
                        ..Default::default()
                    }),
                    crate::Packet::new(packets::ItemRegistry::default()),
                    radius.clone(),
                    third.clone(),
                ],
            )
            .await;
            assert!(matches!(
                &*receive(&mut peer, &codec).await[0].data,
                packets::Packet::ResourcePackClientResponse(_)
            ));
            let requested = receive(&mut peer, &codec).await;
            assert!(matches!(
                &*requested[0].data,
                packets::Packet::RequestChunkRadius(_)
            ));
            let initialized = receive(&mut peer, &codec).await;
            assert_eq!(
                initialized
                    .iter()
                    .filter(|packet| matches!(&*packet.data, packets::Packet::SetLocalPlayerAsInitialized(value) if value.player_id.0 == 42))
                    .count(),
                1
            );
            peer.session.closed().await;
        };
        let client = async {
            let settings = super::super::super::LoginSettings::default();
            let (mut play, runtime_id) = LoginSequence::connect_native(
                &NativeConnectionTarget {
                    address: address.to_string(),
                    transport: NativeTransport::NetherNetLan,
                },
                SessionLogin {
                    display_name: "Fixture",
                    client_random_id: crate::ClientRandomId::new(1).unwrap(),
                    cache: None,
                    skin: None,
                    settings: &settings,
                    tap: None,
                    target: None,
                },
                |_, game| game.world.local_player_runtime_id,
            )
            .await
            .expect("early startup input must survive the actual native pack consumer");
            assert_eq!(runtime_id, 42);
            for expected in [first.clone(), second.clone(), radius.clone(), third.clone()] {
                assert_eq!(play.recv().await.unwrap(), expected);
            }
            play.finish_loading().await.unwrap();
            play.finish_loading().await.unwrap();
            play.close().await.unwrap();
        };
        tokio::join!(server, client);
        listener.close().await.unwrap();
    })
    .await
    .expect("finite actual native pack/startup fixture stalled");
}
