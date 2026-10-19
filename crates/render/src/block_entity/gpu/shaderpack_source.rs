//! The block-entity atlas as shader-pack crack draws read it.

use super::*;

impl BlockEntityGpu {
    /// The uploaded block-entity atlas, destroy stages included, which shader-pack crack
    /// draws read through a gamma-byte view rather than a copy of their own.
    pub(crate) fn atlas_texture(&self) -> Option<&Texture> {
        self.texture.as_ref()
    }
}
