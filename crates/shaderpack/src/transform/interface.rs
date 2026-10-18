//! Location and binding numbering shared by a program's stages.

use crate::programs::StageKind;
use crate::reflection::{FIRST_TEXTURE_BINDING, LOOSE_UNIFORM_BINDING};
use std::collections::BTreeMap;

/// One stage-local varying awaiting program-wide location allocation.
struct Varying {
    symbol: String,
    size: u32,
    explicit: Option<u32>,
}

/// Stage-local varying declarations and program-wide resource bindings. Stages are
/// transformed vertex first, so every vertex output is registered by the time the fragment
/// stage asks whether one feeds its input.
#[derive(Default)]
pub(crate) struct ProgramInterface {
    varyings: BTreeMap<(StageKind, String), Varying>,
    textures: BTreeMap<String, u32>,
    images: BTreeMap<String, u32>,
    uniform_blocks: BTreeMap<String, u32>,
    storage: BTreeMap<String, u32>,
}

impl ProgramInterface {
    /// Registers a varying before allocation so later explicit locations reserve their slots.
    pub(super) fn varying(
        &mut self,
        stage: StageKind,
        name: &str,
        size: u32,
        explicit: Option<u32>,
    ) -> String {
        let next = self.varyings.len();
        let varying = self
            .varyings
            .entry((stage, name.to_string()))
            .or_insert_with(|| Varying {
                symbol: format!("__shaderpack_location_{next}"),
                size,
                explicit,
            });
        varying.symbol.clone()
    }

    /// Whether a vertex output feeds fragment input `name`: one of the same name, or, for an
    /// input at an `explicit` location, an explicitly located output covering it. GLSL
    /// drivers link an unfed input and Iris declares it zeroed, but WebGPU rejects the
    /// pipeline, so the fragment stage turns unfed inputs into zeroed private globals.
    pub(super) fn fed_by_vertex(&self, name: &str, explicit: Option<u32>) -> bool {
        self.varyings
            .contains_key(&(StageKind::Vertex, name.to_string()))
            || explicit.is_some_and(|location| {
                self.varyings.iter().any(|((stage, _), varying)| {
                    *stage == StageKind::Vertex
                        && varying.explicit.is_some_and(|start| {
                            (start..start.saturating_add(varying.size)).contains(&location)
                        })
                })
            })
    }

    /// Allocates non-overlapping implicit locations after reserving every explicit range.
    pub(crate) fn location_defines(&self) -> Result<String, String> {
        let mut used = Vec::new();
        for varying in self.varyings.values() {
            if let Some(start) = varying.explicit {
                let end = start
                    .checked_add(varying.size)
                    .ok_or("varying location overflow")?;
                used.push(start..end);
            }
        }
        let mut implicit = BTreeMap::new();
        let mut definitions = String::new();
        for ((stage, name), varying) in &self.varyings {
            let other_stage = match stage {
                StageKind::Vertex => StageKind::Fragment,
                _ => StageKind::Vertex,
            };
            let peer = self.varyings.get(&(other_stage, name.clone()));
            // Explicit declarations own their locations independently of names. A name
            // links stages only when at least one declaration needs an implicit location.
            let fixed = varying
                .explicit
                .or_else(|| peer.and_then(|peer| peer.explicit));
            if varying.explicit.is_none() && peer.is_some_and(|peer| peer.size != varying.size) {
                return Err(format!(
                    "stages disagree on varying `{name}` location count"
                ));
            }
            let start = if let Some(explicit) = fixed {
                explicit
            } else if let Some(start) = implicit.get(name) {
                *start
            } else {
                let mut start = 0u32;
                loop {
                    let end = start
                        .checked_add(varying.size)
                        .ok_or("varying location overflow")?;
                    match used
                        .iter()
                        .find(|range| start < range.end && range.start < end)
                    {
                        Some(range) => start = range.end,
                        None => {
                            used.push(start..end);
                            implicit.insert(name, start);
                            break start;
                        }
                    }
                }
            };
            definitions.push_str(&format!("#define {} {start}\n", varying.symbol));
        }
        Ok(definitions)
    }

    /// Binding of sampled texture `name`, after the shared samplers.
    pub(super) fn texture_binding(&mut self, name: &str) -> Result<u32, String> {
        numbered_binding(&mut self.textures, name, FIRST_TEXTURE_BINDING)
    }

    /// Binding of storage image `name`.
    pub(super) fn image_binding(&mut self, name: &str) -> Result<u32, String> {
        numbered_binding(&mut self.images, name, 0)
    }

    /// Binding of pack uniform block `name`, after the loose-uniform block.
    pub(super) fn uniform_block_binding(&mut self, name: &str) -> Result<u32, String> {
        numbered_binding(&mut self.uniform_blocks, name, LOOSE_UNIFORM_BINDING + 1)
    }

    /// Binding of storage block `name`: the pack's own `binding`, else the next free one.
    pub(super) fn storage_binding(
        &mut self,
        name: &str,
        declared: Option<u32>,
    ) -> Result<u32, String> {
        if let Some(&existing) = self.storage.get(name) {
            if declared.is_some_and(|binding| binding != existing) {
                return Err(format!("storage block `{name}` has conflicting bindings"));
            }
            return Ok(existing);
        }
        let binding = if let Some(binding) = declared {
            if let Some((other, _)) = self.storage.iter().find(|(_, used)| **used == binding) {
                return Err(format!(
                    "storage blocks `{name}` and `{other}` both claim binding {binding}"
                ));
            }
            binding
        } else {
            match self.storage.values().max() {
                None => 0,
                Some(max) => match max.checked_add(1) {
                    Some(next) => next,
                    None => first_unused_binding(self.storage.values().copied())?,
                },
            }
        };
        self.storage.insert(name.to_string(), binding);
        Ok(binding)
    }
}

/// Reuses a named resource or assigns its next consecutive binding without wrapping.
fn numbered_binding(
    bindings: &mut BTreeMap<String, u32>,
    name: &str,
    first: u32,
) -> Result<u32, String> {
    if let Some(&existing) = bindings.get(name) {
        return Ok(existing);
    }
    let binding = u32::try_from(bindings.len())
        .ok()
        .and_then(|index| first.checked_add(index))
        .ok_or_else(|| format!("resource binding space exhausted for `{name}`"))?;
    bindings.insert(name.to_string(), binding);
    Ok(binding)
}

/// Finds the lowest unused binding by walking occupied slots, never the intervening gaps.
fn first_unused_binding(bindings: impl Iterator<Item = u32>) -> Result<u32, String> {
    let mut used: Vec<_> = bindings.collect();
    used.sort_unstable();
    let mut next = 0u32;
    for binding in used {
        if binding > next {
            break;
        }
        if binding == next {
            next = next
                .checked_add(1)
                .ok_or("storage binding space exhausted")?;
        }
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::numbered_binding;
    use std::collections::BTreeMap;

    #[test]
    fn exhausted_numbering_preserves_existing_resources_without_aliasing() {
        let mut bindings = BTreeMap::new();
        assert_eq!(
            numbered_binding(&mut bindings, "last", u32::MAX),
            Ok(u32::MAX)
        );
        assert_eq!(
            numbered_binding(&mut bindings, "last", u32::MAX),
            Ok(u32::MAX)
        );
        assert!(numbered_binding(&mut bindings, "extra", u32::MAX).is_err());
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings["last"], u32::MAX);
    }
}
