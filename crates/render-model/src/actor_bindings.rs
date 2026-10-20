//! Appearance-owned indices shared by cape and cosmetic attachment sampling.
use assets::{EntityGeometryBone, RuntimeEntityAssets};
use cosmetics::Item;
use std::{
    mem::{align_of, size_of},
    sync::Arc,
};

/// One cape bone's body driver and parent, matched with the original case-insensitive rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapeBoneBinding {
    pub(crate) bone: Option<usize>,
    pub(crate) parent: Option<usize>,
    pub(crate) needs_parent: bool,
}

#[derive(Debug)]
struct CapeNames {
    bone: Box<str>,
    parent: Option<Box<str>>,
}

/// Static attachment selection for one posed skeleton and admitted cosmetic catalog.
#[derive(Debug)]
pub struct ActorAttachmentBindings {
    cape_names: Arc<[CapeNames]>,
    cape: Box<[CapeBoneBinding]>,
    anchors: Box<[(Item, Option<usize>)]>,
    generation: u64,
}

impl ActorAttachmentBindings {
    /// Resolves cape metadata from the drawing catalog without building a second mesh.
    pub fn new(assets: &RuntimeEntityAssets, names: &[Box<str>]) -> Self {
        let bones = crate::find_geometry_index(assets, assets::CAPE_GEOMETRY_IDENTIFIER)
            .and_then(|index| crate::resolve_geometry_bones(assets, index as usize).ok())
            .unwrap_or_default();
        Self::from_cape(&bones, names)
    }

    /// Binds an authored cape and every admitted cosmetic revision to the first matching bone.
    pub fn from_cape(bones: &[EntityGeometryBone], names: &[Box<str>]) -> Self {
        let cape_names = bones
            .iter()
            .map(|bone| CapeNames {
                bone: bone.name.clone(),
                parent: bone.parent.clone(),
            })
            .collect();
        Self::bind(cape_names, names)
    }

    /// Rebinds an appearance while sharing the immutable cape metadata with its rig plan.
    pub fn rebind(&self, names: &[Box<str>]) -> Self {
        Self::bind(Arc::clone(&self.cape_names), names)
    }

    /// Prepares every selection before animation sampling, including retained old item handles.
    fn bind(cape_names: Arc<[CapeNames]>, names: &[Box<str>]) -> Self {
        let find = |name: &str| {
            names
                .iter()
                .position(|bone| bone.eq_ignore_ascii_case(name))
        };
        Self {
            cape: cape_names
                .iter()
                .map(|frame| CapeBoneBinding {
                    bone: find(&frame.bone),
                    parent: frame.parent.as_deref().and_then(find),
                    needs_parent: frame.parent.is_some(),
                })
                .collect(),
            cape_names,
            generation: Item::generation(),
            anchors: Item::registered()
                .into_iter()
                .map(|item| (item, find(&item.descriptor().attachment_bone)))
                .collect(),
        }
    }

    /// Detects admitted anchor changes so the appearance owner can rebuild before sampling.
    pub fn is_current(&self) -> bool {
        self.generation == Item::generation()
    }

    /// Returns a cape's ordered body indices without a frame-time name scan.
    pub fn cape(&self) -> &[CapeBoneBinding] {
        &self.cape
    }

    /// Returns the selected item's body anchor using its stable admitted handle.
    pub fn anchor(&self, item: Item) -> Option<usize> {
        self.anchors
            .binary_search_by_key(&item, |(item, _)| *item)
            .ok()
            .and_then(|slot| self.anchors[slot].1)
    }

    /// Binds a newly admitted item before the next actor tick has refreshed the shared plan.
    pub(crate) fn bind_anchor(&self, item: Item, names: &[Box<str>]) -> Option<usize> {
        match self.anchors.binary_search_by_key(&item, |(item, _)| *item) {
            Ok(slot) => self.anchors[slot].1,
            Err(_) => {
                #[cfg(test)]
                ANCHOR_LOOKUPS.with(|count| count.set(count.get() + 1));
                names
                    .iter()
                    .position(|name| name.eq_ignore_ascii_case(&item.descriptor().attachment_bone))
            }
        }
    }

    /// Counts boxed arrays and the complete shared metadata conservatively for plan admission.
    pub fn retained_bytes(&self) -> Option<usize> {
        let Self {
            cape_names,
            cape,
            anchors,
            generation: _,
        } = self;
        let arrays = std::mem::size_of_val(cape.as_ref())
            .checked_add(std::mem::size_of_val(anchors.as_ref()))?
            .checked_add(std::mem::size_of_val(cape_names.as_ref()))?
            .checked_add(2 * size_of::<usize>())?
            .checked_add(align_of::<CapeNames>())?;
        cape_names
            .iter()
            .try_fold(size_of::<Self>().checked_add(arrays)?, |bytes, name| {
                let CapeNames { bone, parent } = name;
                bytes
                    .checked_add(bone.len())?
                    .checked_add(parent.as_ref().map_or(0, |parent| parent.len()))
            })
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static ANCHOR_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
