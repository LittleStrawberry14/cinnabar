//! Version-bound stack entries supplied by the client's installed base content.

use serde::Deserialize;
use std::sync::LazyLock;

#[derive(Deserialize)]
struct Catalog {
    protocol_version: i32,
    #[cfg(test)]
    baseline_module_version: String,
    packs: Vec<BuiltinPack>,
}

#[derive(Deserialize)]
struct BuiltinPack {
    id: uuid::Uuid,
    version: String,
}

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../../data/builtin-packs.json"))
        .expect("checked built-in pack catalog")
});

/// Recognizes only exact built-in identities; downloaded overrides remain ordinary archives.
pub(super) fn contains(id: uuid::Uuid, version: &str) -> bool {
    CATALOG.protocol_version == crate::PROTOCOL_VERSION
        && CATALOG
            .packs
            .iter()
            .any(|pack| pack.id == id && pack.version == version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_is_bound_to_the_protocol_and_behavioral_baseline() {
        assert_eq!(CATALOG.protocol_version, crate::PROTOCOL_VERSION);
        let manifest = include_str!("../../../../../../core/go.mod");
        let baseline = manifest
            .lines()
            .find(|line| {
                line.split_whitespace()
                    .take(2)
                    .eq(["github.com/sandertv/gophertunnel", "=>"])
            })
            .expect("pinned behavioral baseline")
            .split_whitespace()
            .last()
            .unwrap();
        assert_eq!(
            CATALOG.baseline_module_version, baseline,
            "regenerate and verify the built-in catalog when the baseline changes"
        );
        let mut identities = std::collections::BTreeSet::new();
        for pack in &CATALOG.packs {
            assert!(identities.insert((pack.id, &pack.version)));
        }
    }
}
