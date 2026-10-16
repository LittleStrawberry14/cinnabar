//! Observations of the actor resources selected by the production preparation system.

use super::*;

/// Prepared draw spans and the immutable artwork they sample.
#[derive(Debug, PartialEq, Eq)]
pub struct PublicationActorPreparation {
    pub artwork_identity: [u8; 32],
    pub instances: u32,
    pub geometry_ready: bool,
    pub artwork_current: bool,
    pub spans: Vec<(render_model::ActorArtworkPageId, std::ops::Range<u32>)>,
}

/// Reads the render world's actual prepared resources without creating or repairing them.
pub fn publication_actor_preparation(app: &App) -> PublicationActorPreparation {
    let gpu = app.sub_app(RenderApp).world().resource::<ActorGpu>();
    PublicationActorPreparation {
        artwork_identity: gpu.artwork_identity,
        instances: gpu.instance_count,
        geometry_ready: gpu.geometry_ready,
        artwork_current: gpu.artwork_current,
        spans: gpu
            .spans
            .iter()
            .map(|span| (span.page, span.first..span.first + span.count))
            .collect(),
    }
}
