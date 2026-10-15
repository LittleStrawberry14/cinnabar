//! Bad owner restrictions reject only their own definitions.
use super::*;
use pack_entity_compiler::{CompileReferenceOutcome, FallbackReason};

#[test]
fn two_unreadable_owner_predicates_keep_unrelated_entities_and_equipment() {
    let pack = equipment_pack();
    for (name, predicate) in [
        ("first", "query.owner_identifier =="),
        ("second", "query.is_using_item +"),
    ] {
        let definition = serde_json::json!({"format_version":"1.10.0","minecraft:attachable":{"description":{
            "identifier":format!("fixture:{name}"), "item":{"minecraft:diamond_helmet":predicate},
            "materials":{"default":"armor"}, "textures":{"default":"textures/models/armor/diamond_1"},
            "geometry":{"default":"geometry.item"}, "render_controllers":["controller.render.item"]
        }}});
        write(
            pack.path(),
            &format!("attachables/{name}.json"),
            &serde_json::to_vec(&definition).unwrap(),
        );
    }
    let compilation = compile_entity_assets_with_report(pack.path(), MANIFEST).unwrap();
    assert_eq!(compilation.equipment_bindings.len(), 5);
    assert!(compilation.assets.rig_bindings.iter().any(|rig| {
        compilation.assets.symbols[rig.entity_symbol as usize]
            .identifier
            .as_ref()
            == "minecraft:item"
    }));
    assert_eq!(
        compilation
            .reference_outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                CompileReferenceOutcome::OptionalStaticFallback {
                    reason: FallbackReason::UnsupportedOptionalExpression,
                    ..
                }
            ))
            .count(),
        2
    );
    let encoded = encode_entity_blob(&compilation.assets).unwrap();
    assets::RuntimeEntityAssets::decode(&encoded).unwrap();
}
