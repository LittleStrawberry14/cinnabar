use super::*;

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
