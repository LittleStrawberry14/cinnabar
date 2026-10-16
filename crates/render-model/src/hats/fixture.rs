//! Original redistributable catalog fixtures, excluded from production builds.
use cosmetics::{Catalog, Item};
use std::sync::OnceLock;

/// Admits the original bunny-ear catalog item through the production asset validator.
pub fn bunny() -> Item {
    static ITEM: OnceLock<Item> = OnceLock::new();
    *ITEM.get_or_init(|| {
        let catalog: Catalog = serde_json::from_str(include_str!("fixture/catalog.json")).unwrap();
        super::admit(
            catalog.items[0].clone(),
            include_bytes!("bunny_ears.geo.json"),
            include_bytes!("bunny_ears.png"),
            include_bytes!("fixture/animation.json"),
        )
        .unwrap()
    })
}

/// Admits a second slot using the same original surface with no movement animation.
pub fn rigid() -> Item {
    static ITEM: OnceLock<Item> = OnceLock::new();
    *ITEM.get_or_init(|| {
        let mut item = bunny().descriptor().clone();
        item.id = "fixture:rigid".into();
        item.slot = "back".into();
        item.display_name = "Rigid fixture".into();
        item.assets.animation = None;
        super::admit(
            item,
            include_bytes!("bunny_ears.geo.json"),
            include_bytes!("bunny_ears.png"),
            &[],
        )
        .unwrap()
    })
}
