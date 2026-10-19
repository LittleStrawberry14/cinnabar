use super::*;
use bedrock_protocol_1_26_51::{enums::PlayStatusType, wire};
mod provider;

#[tokio::test]
async fn authenticated_join_refuses_a_transport_without_signed_identity_support() {
    let settings = super::super::LoginSettings::default();
    let authentication = provider::authentication();
    let result = LoginSequence::connect_native_authenticated(
        &NativeConnectionTarget::new("unresolved.invalid:0".into(), NativeTransport::NetherNetLan),
        SessionLogin {
            display_name: "Fixture",
            client_random_id: crate::ClientRandomId::new(1).unwrap(),
            cache: None,
            skin: None,
            settings: &settings,
            tap: None,
            target: None,
        },
        authentication,
        |_, _| (),
    )
    .await;
    assert!(matches!(
        result,
        Err(ProtocolError::Session(crate::SessionError(
            SessionFailure::NativeTransport(error)
        ))) if error.kind() == io::ErrorKind::Unsupported
    ));
}

struct PendingPackets(std::collections::VecDeque<bytes::Bytes>);

impl PacketConnection for PendingPackets {
    async fn send_body(&mut self, _: bytes::Bytes) -> io::Result<()> {
        unreachable!("receive-only replay")
    }
    async fn recv_body(&mut self) -> io::Result<bytes::Bytes> {
        self.0
            .pop_front()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "peer closed"))
    }
    async fn close(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn negotiation_write_failure_preserves_a_received_redirect() {
    let body = crate::session_transport::batch_body(&[
        crate::Packet::new(packets::PlayStatus {
            status: PlayStatusType::Loginsuccess,
        }),
        crate::Packet::new(packets::Transfer {
            server_address: "target.example".into(),
            server_port: wire::U16LE(19132),
            ..Default::default()
        }),
    ])
    .unwrap();
    let mut connection = PendingPackets([body].into());
    let error = preserve_negotiation_boundary(
        &mut connection,
        io::Error::new(io::ErrorKind::BrokenPipe, "response failed"),
    )
    .await;
    assert_eq!(error.server_transfer().unwrap().host, "target.example");
}

#[tokio::test]
async fn negotiation_write_failure_preserves_received_disconnect_text() {
    let body = crate::session_transport::batch_body(&[crate::Packet::new(packets::Disconnect {
        messages: bedrock_protocol_1_26_51::types::DisconnectMessages::DisconnectPacketMessages {
            message: "peer reason".into(),
            filtered_message: "".into(),
        },
        ..Default::default()
    })])
    .unwrap();
    let mut connection = PendingPackets([body].into());
    let error = preserve_negotiation_boundary(
        &mut connection,
        io::Error::new(io::ErrorKind::BrokenPipe, "response failed"),
    )
    .await;
    assert!(
        matches!(error, ProtocolError::Session(crate::SessionError(SessionFailure::ServerDisconnect { message, .. })) if message == "peer reason")
    );
}

#[test]
fn negotiation_redirect_keeps_its_destination() {
    let packet = packets::Packet::Transfer(packets::Transfer {
        server_address: "target.example".into(),
        server_port: wire::U16LE(19132),
        reload_world: true,
        ..Default::default()
    });
    let error = negotiation_failure(packet).unwrap();
    assert_eq!(
        error.server_transfer(),
        Some(crate::ServerTransferEvent {
            host: "target.example".into(),
            port: 19132,
            reload_world: true
        })
    );
}

#[test]
fn unusable_negotiation_redirect_is_a_skippable_semantic_rejection() {
    let packet = packets::Packet::Transfer(packets::Transfer {
        server_address: "".into(),
        server_port: wire::U16LE(19132),
        ..Default::default()
    });
    assert!(matches!(
        negotiation_failure(packet),
        Err(crate::ServerTransferRejection::EmptyHost)
    ));
}
