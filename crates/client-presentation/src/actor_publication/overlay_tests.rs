//! Controller styling reaches worn equipment without inheriting body visibility masks.
use super::*;
use render_api::CombatVisuals;
use render_model::{ActorRigSubmission, RenderBoneTransform, pack_overlay_rgba8};

const INHERITED: [f32; 4] = [0.0, 1.0, 0.0, 0.5];
const BODY_OVERLAY: [f32; 4] = [1.0, 0.0, 0.0, 0.5];
const ARMOR_OVERLAY: [f32; 4] = [0.0, 0.0, 1.0, 0.5];

/// Gives only the body two controllers and optionally gives armour its own overlay.
fn overlay_files(masked: bool, authored_armor: bool) -> Vec<(Box<str>, Vec<u8>)> {
    let mut files = variant_files(true, Some("q.is_sneaking"));
    for (path, bytes) in &mut files {
        let mut json: serde_json::Value = match path.as_ref() {
            "render_controllers/fixture.json"
            | "entity/player.json"
            | "attachables/armour.json"
            | "models/entity/fixture.json" => serde_json::from_slice(bytes).unwrap(),
            _ => continue,
        };
        match path.as_ref() {
            "render_controllers/fixture.json" => {
                let controllers = &mut json["render_controllers"];
                let base = controllers["controller.render.fixture"].clone();
                let mut primary = base.clone();
                primary["overlay_color"] = serde_json::json!({
                    "r": BODY_OVERLAY[0], "g": BODY_OVERLAY[1],
                    "b": BODY_OVERLAY[2], "a": BODY_OVERLAY[3]
                });
                primary["part_visibility"] =
                    serde_json::json!([{"body": if masked { "0" } else { "1" }}]);
                controllers["controller.render.body_overlay"] = primary;
                controllers["controller.render.body_extra"] = base.clone();
                let mut armor = base;
                if authored_armor {
                    armor["overlay_color"] = serde_json::json!({
                        "r": ARMOR_OVERLAY[0], "g": ARMOR_OVERLAY[1],
                        "b": ARMOR_OVERLAY[2], "a": ARMOR_OVERLAY[3]
                    });
                }
                controllers["controller.render.armor_overlay"] = armor;
            }
            "entity/player.json" => {
                json["minecraft:client_entity"]["description"]["render_controllers"] =
                    serde_json::json!([
                        "controller.render.body_overlay",
                        "controller.render.body_extra"
                    ]);
            }
            "models/entity/fixture.json" => {
                let bones = json["minecraft:geometry"][0]["bones"]
                    .as_array_mut()
                    .unwrap();
                for name in ["head", "rightarm", "leftarm", "rightleg", "leftleg"] {
                    bones.push(serde_json::json!({"name": name, "pivot": [0, 24, 0]}));
                }
            }
            "attachables/armour.json" => {
                json["minecraft:attachable"]["description"]["render_controllers"] =
                    serde_json::json!(["controller.render.armor_overlay"]);
            }
            _ => unreachable!(),
        }
        *bytes = serde_json::to_vec(&json).unwrap();
    }
    files
}

/// Runs local or remote publication with the same Java pose marker used by normal assembly.
fn publish_overlay(
    stream: &WorldStream,
    entities: &RuntimeEntityAssets,
    runtime: &mut EquipmentRuntime,
    pages: &ActorArtworkPages,
    state: &mut ActorFrameState,
    local: bool,
    profile: CombatVisuals,
    java_mode: bool,
) -> ActorPresentationBatch {
    let authority = stream.authority();
    let actor = authority.actor(2).unwrap();
    let rig = authority.actor_rig(2).unwrap();
    let input = ActorEquipmentInput {
        is_local_player: local,
        ..equipment::remote_input(stream, 2)
    };
    let mut body =
        crate::presentation::actors::actor_rig_presentation(&rig, actor, None, 1.0).unwrap();
    let mut java_posed = Vec::new();
    if java_mode {
        let third = java::third_person(
            stream,
            &rig,
            actor,
            local.then_some(&input),
            1.0,
            &mut java::PoseScratch::default(),
        )
        .expect("the original player fixture supports Java posing");
        java::apply_pose(&mut body, &third.bones, local, actor, 1.0);
        java_posed.push(third.posed);
    }
    body.submission.overlay_rgba8 = pack_overlay_rgba8(INHERITED);
    let mut batch = if local {
        crate::presentation::actors::select_actor_presentations(2, true, Some(body), [])
    } else {
        crate::presentation::actors::select_actor_presentations(1, false, None, [body])
    };
    let mut frame = authority.actor_render_frame(1.0);
    state.layer_poses.begin_frame();
    layers::attach(
        &mut batch,
        Some(stream),
        Some(&mut frame),
        layers::LayerPoses {
            local_runtime_id: if local { 2 } else { 1 },
            local_equipment: &input,
            java_posed: &java_posed,
            combat_visuals: profile,
            local_emote_pose: None,
            native_body_sampled: false,
            remote_skin_layers: &Default::default(),
            native_pose: &state.java_hand.native_pose,
        },
        layers::LayerCaches {
            equipment: Some(runtime),
            layer_poses: &mut state.layer_poses,
            cape_state: &mut state.cape_state,
            skin_layers: &mut state.skin_layers,
            skin_rigs: &mut state.skin_rigs,
        },
        pages,
        Some(entities),
        &mut ActorRenderScene::default(),
        (1.0, 0.05, None),
        &mut Vec::new(),
    );
    batch
}

/// Returns one emitted layer, requiring publication rather than inspecting selection alone.
fn drawn(batch: &ActorPresentationBatch, layer: u8) -> &ActorRigSubmission {
    batch
        .submissions
        .iter()
        .find(|draw| draw.input.identity.layer == layer)
        .unwrap()
}

#[test]
fn inherited_and_authored_overlays_preserve_unmasked_worn_categories_for_each_profile() {
    for authored_armor in [false, true] {
        for local in [false, true] {
            for profile in [CombatVisuals::Legacy112, CombatVisuals::Modern] {
                for java_mode in [false, true] {
                    let mut unmasked: Vec<Arc<[RenderBoneTransform]>> = Vec::new();
                    for masked in [false, true] {
                        let (entities, catalog) =
                            compile_fixture(overlay_files(masked, authored_armor));
                        let (mut runtime, pages) = build_runtime(&entities, Some(catalog));
                        let mut state = ActorFrameState::default();
                        let mut stream = equipped_stream(Arc::clone(&entities), &mut state);
                        for (step, wings) in [false, true, false].into_iter().enumerate() {
                            stream
                                .submit(
                                    4 + step as u64,
                                    WorldEvent::Actor(ActorEvent::Metadata(
                                        protocol::ActorMetadataUpdateEvent {
                                            runtime_id: 2,
                                            dimension: 0,
                                            tick: step as u64,
                                            metadata: Arc::from([protocol::ActorMetadata {
                                                key: protocol_ids::ActorDataKey::Flags.wire_value(),
                                                value: protocol::ActorMetadataValue::Flags(
                                                    if wings {
                                                        1 << protocol_ids::ActorFlag::Sneaking
                                                            .wire_value()
                                                    } else {
                                                        0
                                                    },
                                                ),
                                            }]),
                                            properties: Arc::from([]),
                                        },
                                    )),
                                )
                                .unwrap();
                            stream.advance_actor_interpolation_ticks(1);
                            for repeat in 0..2 {
                                let batch = publish_overlay(
                                    &stream,
                                    &entities,
                                    &mut runtime,
                                    &pages,
                                    &mut state,
                                    local,
                                    profile,
                                    java_mode,
                                );
                                let expected_rig = render_model::equipment_rig_id(
                                    render_model::find_geometry_index(
                                        &entities,
                                        if wings {
                                            assets::ELYTRA_GEOMETRY_IDENTIFIER
                                        } else {
                                            ARMOR_GEOMETRY
                                        },
                                    )
                                    .unwrap(),
                                );
                                assert_category(&batch, Some(expected_rig), wings);
                                let body = drawn(&batch, render_model::ACTOR_LAYER_BODY);
                                assert_eq!(body.overlay_rgba8, pack_overlay_rgba8(BODY_OVERLAY));
                                assert_eq!(
                                    body.input.current_bones[0].translation_scale[3],
                                    if masked { 0.0 } else { 1.0 }
                                );
                                let extra = drawn(
                                    &batch,
                                    crate::presentation::entity_layers::ACTOR_LAYER_TEXTURE_BASE,
                                );
                                assert_eq!(
                                    extra.overlay_rgba8,
                                    pack_overlay_rgba8(INHERITED),
                                    "the primary controller must not restyle a later body layer"
                                );
                                let worn = drawn(&batch, equipment::ELYTRA_LAYER);
                                let own_overlay = if authored_armor && !wings {
                                    pack_overlay_rgba8(ARMOR_OVERLAY)
                                } else {
                                    0
                                };
                                let expected_overlay = if profile == CombatVisuals::Modern
                                    && java_mode
                                    || profile == CombatVisuals::Legacy112 && own_overlay == 0
                                {
                                    pack_overlay_rgba8(BODY_OVERLAY)
                                } else {
                                    own_overlay
                                };
                                assert_eq!(worn.overlay_rgba8, expected_overlay);
                                assert!(
                                    worn.input
                                        .current_bones
                                        .iter()
                                        .all(|bone| bone.translation_scale[3] > 0.0)
                                );
                                let index = step * 2 + repeat;
                                if masked {
                                    assert_eq!(
                                        worn.input.current_bones, unmasked[index],
                                        "body visibility cannot change equipment's attachment pose"
                                    );
                                } else {
                                    unmasked.push(Arc::clone(&worn.input.current_bones));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
