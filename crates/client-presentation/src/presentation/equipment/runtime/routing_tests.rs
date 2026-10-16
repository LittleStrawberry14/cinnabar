//! Equipment selection and fallback outcomes across mounts and catalog layers.

use super::*;
use crate::presentation::equipment::display::{
    LAYER_CHESTPLATE, LAYER_HELMET, LAYER_MAIN_HAND, LAYER_OFF_HAND, head_block_display,
};
use assets::ArmorSlot;

#[derive(Clone, Copy, Debug)]
enum Definition {
    Ordinary,
    OwnerAccepted,
    OwnerRejected,
    ConditionFailed,
    Missing,
    MissingArtwork,
}

#[derive(Clone, Copy, Debug)]
enum Mount {
    Hand,
    JavaHand,
    JavaOffhand,
    BlockHand,
    BlockHelmet,
    Helmet,
    BlockChest,
}

#[derive(Clone, Copy, Debug)]
enum Expected {
    Selected,
    Fallback,
    Hidden,
}

/// Compiles a single original definition with independent admission and resource failures.
fn routing_pack(
    identifier: &str,
    definition: Definition,
    color: u8,
    native: bool,
) -> Vec<(Box<str>, Vec<u8>)> {
    let mut files = fixture(identifier, "geometry.fixture.routing", color, false);
    for (path, bytes) in &mut files {
        if path.starts_with("textures/") {
            continue;
        }
        let mut json: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        if path.starts_with("attachables/") {
            let description = &mut json["minecraft:attachable"]["description"];
            match definition {
                Definition::OwnerAccepted | Definition::OwnerRejected => {
                    description["identifier"] = serde_json::json!("fixture:owner_model");
                    description["item"] = serde_json::json!({identifier: if matches!(definition, Definition::OwnerAccepted) {
                        "q.owner_identifier == 'minecraft:skeleton'"
                    } else { "q.owner_identifier == 'minecraft:player'" }});
                }
                Definition::ConditionFailed => {
                    description["item"] = serde_json::json!({identifier: "0"})
                }
                _ => {}
            }
        } else if path.starts_with("models/") && !native {
            let bone = &mut json["minecraft:geometry"][0]["bones"][0];
            bone["name"] = serde_json::json!("head");
            bone.as_object_mut().unwrap().remove("texture_meshes");
            bone["cubes"] = serde_json::json!([{"origin":[0,0,0],"size":[3,4,5],"uv":[0,0]}]);
        }
        *bytes = serde_json::to_vec(&json).unwrap();
    }
    files.retain(|(path, _)| match definition {
        Definition::Missing => !path.starts_with("attachables/"),
        Definition::MissingArtwork => !path.starts_with("textures/"),
        _ => true,
    });
    files
}

/// Builds drawable icon and block fallbacks independently of any equipment definition.
fn routing_runtime(
    identifier: &str,
    files: Vec<(Box<str>, Vec<u8>)>,
) -> (EquipmentRuntime, ActorArtworkPages) {
    let (entities, catalog) = pack_catalog(files);
    let icons = Arc::new(
        assets::RuntimeIconCatalog::decode(
            &assets::encode_icon_catalog(
                [0; 32],
                &[assets::IconSprite {
                    width: 16,
                    height: 16,
                    rgba8: [200, 30, 40, 255].repeat(16 * 16).into(),
                }],
                &[assets::IconEntry {
                    identifier: identifier.into(),
                    metadata: 0,
                    sprite: 0,
                }],
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let (mut runtime, pages, _) = EquipmentRuntime::build(
        entities,
        Some(catalog),
        icons,
        None,
        None,
        ActorArtworkPages::default(),
    );
    let [width, height] = assets::BLOCK_ITEM_SHEET_SIZE;
    let atlas = super::super::super::SpriteAtlas::pack(&[assets::IconSprite {
        width,
        height,
        rgba8: [150, 60, 70, 255]
            .repeat(usize::from(width) * usize::from(height))
            .into(),
    }]);
    let (pages, locations) = pages.with_equipment_rasters(&atlas.layers);
    let blocks = Arc::make_mut(&mut runtime.blocks.source);
    blocks.placements = atlas.placements;
    blocks.sheets.insert(7, 0);
    runtime.blocks.locations = locations;
    (runtime, pages)
}

#[test]
fn equipment_routing_fallback_table() {
    use Definition::*;
    use Expected::*;
    use Mount::*;
    // Columns: hand, Java main, Java offhand, block hand, block helmet, helmet, block chest.
    let rejected = [
        Fallback, Fallback, Fallback, Fallback, Fallback, Hidden, Hidden,
    ];
    let cases = [
        (Ordinary, [Selected; 7]),
        (OwnerAccepted, [Selected; 7]),
        (OwnerRejected, rejected),
        (ConditionFailed, rejected),
        (Missing, rejected),
        (
            MissingArtwork,
            [
                Fallback, Fallback, Fallback, Fallback, Hidden, Hidden, Hidden,
            ],
        ),
    ];
    let mounts = [
        Hand,
        JavaHand,
        JavaOffhand,
        BlockHand,
        BlockHelmet,
        Helmet,
        BlockChest,
    ];
    for (definition, outcomes) in cases {
        assert_eq!(mounts.len(), outcomes.len());
        for (mount, outcome) in mounts.into_iter().zip(outcomes) {
            for (pack, startup) in [(false, false), (true, false), (true, true)] {
                for native in [false, true] {
                    if native && !matches!(mount, Hand | JavaHand | JavaOffhand) {
                        continue;
                    }
                    let identifier = if native {
                        assets::facts::VanillaItem::Bow.identifier()
                    } else {
                        "fixture:routed_item"
                    };
                    let files = routing_pack(identifier, definition, 80, native);
                    let (mut runtime, mut pages) = routing_runtime(
                        identifier,
                        if pack {
                            routing_pack(
                                identifier,
                                if startup { Ordinary } else { Missing },
                                10,
                                native,
                            )
                        } else {
                            files.clone()
                        },
                    );
                    if pack {
                        pages = install(&mut runtime, pages, files);
                    }
                    let slot = match mount {
                        BlockHelmet | Helmet => Some(ArmorSlot::Helmet),
                        BlockChest => Some(ArmorSlot::Chestplate),
                        _ => None,
                    };
                    if let Some(slot) = slot {
                        crate::presentation::equipment::tests::declare_wearable(
                            &mut runtime,
                            identifier,
                            if slot == ArmorSlot::Helmet {
                                "slot.armor.head"
                            } else {
                                "slot.armor.chest"
                            },
                        );
                    }
                    let body = player_body(&mut runtime);
                    let actor = owner();
                    let names = [
                        Box::from("head"),
                        Box::from("rightItem"),
                        Box::from("leftItem"),
                    ];
                    let rig = owner_rig(&actor, &names);
                    let mut stack = item(identifier);
                    stack.enchanted = false;
                    if matches!(mount, BlockHand | BlockHelmet | BlockChest) {
                        stack.kind = HeldKind::Block(7);
                    }
                    let mut input = ActorEquipmentInput {
                        java: matches!(mount, JavaHand | JavaOffhand)
                            .then_some(JavaGrip::default()),
                        ..Default::default()
                    };
                    let layer = if let Some(slot) = slot {
                        input.armor[slot as usize] = Some(stack.clone());
                        if slot == ArmorSlot::Helmet {
                            LAYER_HELMET
                        } else {
                            LAYER_CHESTPLATE
                        }
                    } else if matches!(mount, JavaOffhand) {
                        input.off = Some(stack.clone());
                        LAYER_OFF_HAND
                    } else {
                        input.main = Some(stack.clone());
                        LAYER_MAIN_HAND
                    };
                    let mut fallback = Vec::new();
                    if matches!(mount, JavaHand) {
                        let (_, bones) = runtime.body_bones_for(body.input.rig).unwrap();
                        runtime.push_java_held(
                            &body,
                            &stack,
                            &bones,
                            JavaGrip::default(),
                            &mut fallback,
                        );
                    } else {
                        runtime.push_attached(
                            &body,
                            &stack,
                            layer,
                            Some(if slot.is_some() {
                                2
                            } else if matches!(mount, JavaOffhand) {
                                6
                            } else {
                                5
                            }),
                            matches!(mount, BlockHelmet).then(head_block_display),
                            &mut fallback,
                        );
                    }
                    let drawn = runtime
                        .layers_for(
                            &body,
                            &input,
                            Some(EquipmentAnimation {
                                owner: &actor,
                                rig: &rig,
                                frame_alpha: 0.5,
                                delta_seconds: 0.05,
                            }),
                        )
                        .to_vec();
                    let label = format!(
                        "{definition:?} {mount:?} pack={pack} startup={startup} native={native}"
                    );
                    let startup_artwork =
                        startup && !matches!(definition, Ordinary | OwnerAccepted);
                    let startup_definition =
                        startup_artwork && !matches!(definition, MissingArtwork);
                    let outcome =
                        if (native && matches!(mount, JavaHand) && (!pack || startup_definition))
                            || (matches!(definition, MissingArtwork) && slot.is_none())
                        {
                            Fallback
                        } else if startup_artwork {
                            Selected
                        } else {
                            outcome
                        };
                    if matches!(outcome, Hidden) {
                        assert!(drawn.is_empty(), "{label}");
                        continue;
                    }
                    assert_eq!(drawn.len(), 1, "{label}");
                    assert_eq!(drawn[0].submission.input.identity.layer, layer, "{label}");
                    if matches!(outcome, Fallback) {
                        assert_eq!(
                            drawn[0].submission.input.rig, fallback[0].submission.input.rig,
                            "{label}"
                        );
                        assert_eq!(drawn[0].location, fallback[0].location, "{label}");
                        assert_eq!(
                            drawn[0].submission.input.current_bones,
                            fallback[0].submission.input.current_bones,
                            "{label}"
                        );
                    } else {
                        let meshes = runtime.take_pending_geometries();
                        let color = if let Some(mesh) = meshes
                            .iter()
                            .find(|mesh| mesh.id == drawn[0].submission.input.rig)
                        {
                            mesh_pixel(&pages, drawn[0].location, mesh)
                        } else {
                            selected_pixel(&pages, &drawn[0])
                        };
                        assert_eq!(color[0], if startup_artwork { 10 } else { 80 }, "{label}");
                        assert!(
                            drawn[0]
                                .submission
                                .input
                                .current_bones
                                .iter()
                                .any(|bone| bone.translation_scale[3] > 0.0),
                            "{label}"
                        );
                    }
                }
            }
        }
    }
}
