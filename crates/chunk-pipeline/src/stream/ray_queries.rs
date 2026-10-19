use super::*;

impl WorldStream {
    /// Distance along the unit `direction` from `origin` to the first block cell holding
    /// anything but air in any layer, walking cell by cell up to `max_distance`. Unloaded
    /// sub-chunks read as air. `None` when nothing is hit or the inputs are not finite.
    #[must_use]
    pub fn first_block_distance(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        max_distance: f32,
    ) -> Option<f32> {
        if !origin
            .iter()
            .chain(&direction)
            .all(|value| value.is_finite())
        {
            return None;
        }
        let dimension = self.authority.current_dimension();
        let mut cell = origin.map(floor_to_i32);
        let step = direction.map(|d| if d < 0.0 { -1 } else { 1 });
        // Distance to cross one cell, and to the first boundary, per axis.
        let across = direction.map(|d| {
            if d == 0.0 {
                f32::INFINITY
            } else {
                d.abs().recip()
            }
        });
        let mut next: [f32; 3] = std::array::from_fn(|axis| {
            let d = direction[axis];
            if d == 0.0 {
                return f32::INFINITY;
            }
            let boundary = if d > 0.0 {
                cell[axis] as f32 + 1.0
            } else {
                cell[axis] as f32
            };
            (boundary - origin[axis]) / d
        });
        let mut travelled = 0.0;
        let mut cached: Option<(SubChunkKey, Option<Arc<SubChunk>>)> = None;
        while travelled <= max_distance {
            let key = SubChunkKey::new(
                dimension,
                cell[0].div_euclid(16),
                cell[1].div_euclid(16),
                cell[2].div_euclid(16),
            );
            if cached.as_ref().is_none_or(|(cached, _)| *cached != key) {
                cached = Some((key, self.authority.terrain().sub_chunk(key)));
            }
            if let Some((_, Some(sub_chunk))) = &cached {
                let local = |axis: usize| cell[axis].rem_euclid(16) as u8;
                let (x, y, z) = (local(0), local(1), local(2));
                let solid = (0..sub_chunk.storages().len()).any(|layer| {
                    sub_chunk
                        .runtime_id(layer, x, y, z)
                        .is_some_and(|id| !self.classifier.is_air(id))
                });
                if solid {
                    return Some(travelled);
                }
            }
            let axis = (0..3)
                .min_by(|a, b| next[*a].total_cmp(&next[*b]))
                .unwrap_or(0);
            travelled = next[axis];
            next[axis] += across[axis];
            cell[axis] += step[axis];
        }
        None
    }
}
