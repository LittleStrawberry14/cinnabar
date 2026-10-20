//! Content keys retain immutable device textures across artwork publications.
use super::GpuArtworkPage;
use std::hash::BuildHasher;
use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};

/// Fingerprints select candidates; exact source equality keeps hash collisions harmless.
#[derive(Clone)]
pub(super) struct TextureKey {
    page: ActorTexturePage,
    fingerprint: u64,
}

impl TextureKey {
    /// Hashes content only when a publication introduces a different source allocation.
    pub(super) fn new(page: &ActorTexturePage, hasher: &RandomState) -> Self {
        #[cfg(test)]
        super::work_tests::record_hash(page.rgba8.len());
        Self {
            page: page.clone(),
            fingerprint: hasher.hash_one((page.width, page.height, page.layers, &page.rgba8)),
        }
    }

    /// Pointer identity is safe because every key owns its immutable source allocation.
    pub(super) fn shares_pixels_with(&self, page: &ActorTexturePage) -> bool {
        self.page.width == page.width
            && self.page.height == page.height
            && self.page.layers == page.layers
            && Arc::ptr_eq(&self.page.rgba8, &page.rgba8)
    }

    /// Returns the preserved layer prefix when a larger page appends identical-sized rasters.
    pub(super) fn appended_layers(&self, page: &ActorTexturePage) -> Option<u32> {
        (self.page.width == page.width
            && self.page.height == page.height
            && self.page.layers < page.layers
            && page.rgba8.starts_with(&self.page.rgba8))
        .then_some(self.page.layers)
    }

    /// Reports whether uploaded source layers cover the request, excluding spare capacity.
    pub(super) fn contains_layers(&self, layers: u32) -> bool {
        self.page.layers >= layers
    }

    /// Initialized layers can serve a shorter page only when its entire prefix matches.
    pub(super) fn covers(&self, page: &ActorTexturePage) -> bool {
        self.page.width == page.width
            && self.page.height == page.height
            && self.page.layers >= page.layers
            && self.page.rgba8.starts_with(&page.rgba8)
    }
}

impl PartialEq for TextureKey {
    /// Source dimensions and exact pixels define texture reuse independently of material flags.
    fn eq(&self, other: &Self) -> bool {
        self.fingerprint == other.fingerprint
            && self.page.width == other.page.width
            && self.page.height == other.page.height
            && self.page.layers == other.page.layers
            && (Arc::ptr_eq(&self.page.rgba8, &other.page.rgba8)
                || self.page.rgba8 == other.page.rgba8)
    }
}
impl Eq for TextureKey {}
impl Hash for TextureKey {
    /// Cache lookup reuses the fingerprint without traversing source pixels again.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.fingerprint.hash(state);
    }
}
use render_model::ActorTexturePage;
use std::collections::{HashMap, HashSet, hash_map::RandomState};

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct AllocationKey {
    address: usize,
    bytes: usize,
    width: u16,
    height: u16,
    layers: u32,
}

impl AllocationKey {
    /// The retained content key keeps this allocation alive for the lookup's lifetime.
    fn new(page: &ActorTexturePage) -> Self {
        Self {
            address: page.rgba8.as_ptr() as usize,
            bytes: page.rgba8.len(),
            width: page.width,
            height: page.height,
            layers: page.layers,
        }
    }
}

#[derive(Default)]
pub(super) struct TextureCache {
    allocations: HashMap<AllocationKey, TextureKey>,
    textures: HashMap<TextureKey, CachedTexture>,
    required: HashSet<TextureKey>,
}

struct CachedTexture {
    page: GpuArtworkPage,
    owners: usize,
}

impl TextureCache {
    /// Counts current page owners and indexes their reusable textures.
    pub(super) fn new(pages: Vec<GpuArtworkPage>) -> Self {
        let mut cache = Self::default();
        for page in pages {
            cache.insert(page);
        }
        cache
    }

    /// Finds retained source allocations without hashing pixels that no actor currently demands.
    pub(super) fn shared(&self, page: &ActorTexturePage) -> Option<GpuArtworkPage> {
        self.get(self.allocations.get(&AllocationKey::new(page))?)
    }

    /// Shared allocations avoid hashing; equal independent allocations share their texture too.
    pub(super) fn key(&mut self, page: &ActorTexturePage, hasher: &RandomState) -> TextureKey {
        self.allocations
            .entry(AllocationKey::new(page))
            .or_insert_with(|| TextureKey::new(page, hasher))
            .clone()
    }

    /// Keeps exact reuse available to later demand, even after its old route is replaced.
    pub(super) fn require(&mut self, page: &ActorTexturePage, hasher: &RandomState) {
        let key = self.key(page, hasher);
        self.required.insert(key);
    }

    /// Returns another handle to the same immutable device texture and bindings.
    pub(super) fn get(&self, key: &TextureKey) -> Option<GpuArtworkPage> {
        self.textures.get(key).map(|entry| entry.page.clone())
    }

    /// Counts live page owners so a shared texture survives until its last route is replaced.
    fn insert(&mut self, page: GpuArtworkPage) {
        self.allocations
            .insert(AllocationKey::new(&page.source.page), page.source.clone());
        let entry = self
            .textures
            .entry(page.source.clone())
            .or_insert_with(|| CachedTexture {
                page: page.clone(),
                owners: 0,
            });
        entry.page = page;
        entry.owners += 1;
    }

    /// Retires superseded allocations immediately while preserving live and future exact reuse.
    pub(super) fn replace(&mut self, previous: Option<&TextureKey>, page: GpuArtworkPage) {
        self.insert(page);
        if let Some(previous) = previous {
            let entry = self
                .textures
                .get_mut(previous)
                .expect("previous page belongs to this cache");
            entry.owners -= 1;
            if entry.owners == 0 && !self.required.contains(previous) {
                self.textures.remove(previous);
            }
        }
    }
}
