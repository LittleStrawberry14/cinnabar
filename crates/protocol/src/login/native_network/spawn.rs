//! Shared spawn and finite preparation after the native connection negotiates its packs.

use super::super::{JoinPreparation, NativeJoinPreparation, PlaySession, SessionLogin};
use super::negotiated::NegotiatedConnection;
use crate::session_transport::{MAX_READ_AHEAD_BYTES, SessionTransport};
use crate::{GameData, ProtocolError};
use bedrock_packs::ResourcePackHandoff;

/// Consumes the negotiated connection once without replacing its raw FIFO or pack archives.
pub(super) async fn continue_join<P: Send + 'static>(
    negotiated: NegotiatedConnection,
    login: SessionLogin<'_>,
    prepare: impl FnOnce(ResourcePackHandoff, GameData) -> P + Send + 'static,
) -> Result<(PlaySession, P), ProtocolError> {
    let NegotiatedConnection {
        mut connection,
        packs,
    } = negotiated;
    let deferred = connection.take_deferred();
    let mut transport = SessionTransport::from_native(connection).with_tap(login.tap);
    transport.adopt_startup(deferred);
    let preparation = NativeJoinPreparation;
    let mut prepared_job = None;
    let (initialization, spawned) = super::super::startup::await_spawn(
        &mut transport,
        login.settings.view_distance_chunks,
        |start, items| {
            prepared_job = Some(
                preparation.submit(move || prepare(packs, GameData::from_wire(&start, &items))),
            );
        },
    )
    .await?;
    let job = prepared_job.expect("spawn requires StartGame and item registry");
    tokio::pin!(job);
    let prepared = tokio::select! {
        value = &mut job => value,
        () = transport.read_ahead(MAX_READ_AHEAD_BYTES) => job.await,
    };
    Ok((
        PlaySession::new(
            transport,
            login.cache,
            initialization,
            spawned.terrain_received,
        ),
        prepared,
    ))
}
