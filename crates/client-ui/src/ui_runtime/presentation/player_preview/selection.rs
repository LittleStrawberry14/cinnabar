//! Worn preview selection uses the local actor's attachable query context.
use super::*;
use actor_animation::AttachablesRuntime;

struct SelectionAssets {
    source: Arc<assets::RuntimeEntityAssets>,
    runtime: AttachablesRuntime,
}

/// Retains the same bounded predicate caches as worn world rendering.
#[derive(Default)]
pub(in crate::ui_runtime::presentation) struct EquipmentSelection {
    base: Option<SelectionAssets>,
    pack: Option<SelectionAssets>,
}

impl EquipmentSelection {
    /// Selects session artwork before startup, sharing the caller's budget across both layers.
    pub(super) fn binding<'a>(
        &mut self,
        base: Option<&'a RuntimeEquipmentCatalog>,
        pack: Option<&'a RuntimeEquipmentCatalog>,
        identifier: &str,
        owner: Option<PreviewEquipmentOwner<'_>>,
        slot: usize,
        budget: &mut usize,
    ) -> Option<&'a assets::EquipmentBinding> {
        let drawable = |binding: &assets::EquipmentBinding| {
            pack.and_then(|catalog| catalog.texture(&binding.texture.identifier))
                .or_else(|| base?.texture(&binding.texture.identifier))
                .is_some()
        };
        let mut select = |catalog: Option<&'a RuntimeEquipmentCatalog>,
                          layer: &mut Option<SelectionAssets>| {
            let catalog = catalog?;
            let index = if let (Some(owner), Some(layer)) = (owner, layer.as_mut()) {
                layer.runtime.select_equipment_binding(
                    catalog,
                    identifier,
                    owner.actor,
                    &owner.rig,
                    actor_animation::AttachableAnimationInput {
                        worn: true,
                        worn_slot: slot as u8,
                        ..owner.input
                    },
                    budget,
                    drawable,
                )?
            } else {
                // Without an actor, only unrestricted ordinary definitions are safe to admit.
                return catalog
                    .bindings_for(identifier)
                    .iter()
                    .rev()
                    .find(|binding| {
                        binding.attachable.is_none()
                            && drawable(binding)
                            && layer.as_ref().is_none_or(|layer| {
                                layer
                                    .source
                                    .attachable_rig_binding(binding.attachable_identifier())
                                    .is_none_or(|rig| {
                                        layer.source.rig_bindings()[rig].item_condition.is_none()
                                    })
                            })
                    });
            };
            catalog.bindings().get(index)
        };
        select(pack, &mut self.pack).or_else(|| select(base, &mut self.base))
    }
}

/// Keeps predicate state until the immutable catalog changes, then drops its cached results.
fn sync(layer: &mut Option<SelectionAssets>, source: Option<Arc<assets::RuntimeEntityAssets>>) {
    if match (layer.as_ref(), source.as_ref()) {
        (Some(old), Some(source)) => Arc::ptr_eq(&old.source, source),
        (None, None) => true,
        _ => false,
    } {
        return;
    }
    *layer = source.map(|source| SelectionAssets {
        runtime: AttachablesRuntime::new(Arc::clone(&source)),
        source,
    });
}

/// The player and completed animation inputs that a worn preview observes.
#[derive(Clone, Copy, Debug)]
pub struct PreviewEquipmentOwner<'a> {
    pub actor: &'a dyn actor_animation::AnimatedActor,
    pub rig: actor_animation::ActorRigSnapshot<'a>,
    pub input: actor_animation::AttachableAnimationInput<'a>,
}

impl UiPresentationRuntime {
    /// Supplies the immutable startup and session catalogs that own item conditions.
    pub fn set_preview_equipment_assets(
        &mut self,
        base: Option<Arc<assets::RuntimeEntityAssets>>,
        pack: Option<Arc<assets::RuntimeEntityAssets>>,
    ) {
        sync(&mut self.preview_equipment_selection.base, base);
        sync(&mut self.preview_equipment_selection.pack, pack);
    }

    /// Dresses a preview using its player's authoritative attachable inputs.
    pub fn set_player_preview_gear_with_owner(
        &mut self,
        armor: [Option<(&str, Option<u32>)>; 4],
        held: Option<(&str, u32)>,
        owner: PreviewEquipmentOwner<'_>,
    ) {
        self.set_preview_gear(armor, held, Some(owner));
    }
}
