//! Preview artwork and dye policy follow the player admitted by the item condition.
use super::*;
use player_preview::PreviewEquipmentOwner;

const ITEM: &str = "fixture:chestplate";
const PLAYER: [u8; 4] = [40, 190, 100, 255];
const STAND: [u8; 4] = [220, 110, 30, 255];
const STARTUP: [u8; 4] = [70, 100, 210, 255];
const DYE: u32 = 0x00d0_3040;

struct Definition<'a> {
    name: &'a str,
    condition: Option<&'a str>,
    color: [u8; 4],
    dyed: bool,
}

/// Compiles original player and worn-item resources with independently identifiable artwork.
fn catalog(definitions: &[Definition<'_>]) -> assets::SessionEntityPack {
    let mut files = vec![
        ("models/fixture.json".into(), br#"{"format_version":"1.12.0","minecraft:geometry":[{"description":{"identifier":"geometry.fixture","texture_width":64,"texture_height":32},"bones":[{"name":"body","pivot":[0,24,0],"cubes":[{"origin":[-4,12,-2],"size":[8,12,4],"uv":[16,16]}]}]}]}"#.to_vec()),
        ("render_controllers/fixture.json".into(), br#"{"format_version":"1.8.0","render_controllers":{"controller.render.fixture":{"geometry":"Geometry.default","materials":[{"*":"Material.default"}],"textures":["Texture.default"]}}}"#.to_vec()),
        ("entity/player.json".into(), br#"{"format_version":"1.10.0","minecraft:client_entity":{"description":{"identifier":"minecraft:player","materials":{"default":"plain"},"textures":{"default":"textures/fixture/player"},"geometry":{"default":"geometry.fixture"},"render_controllers":["controller.render.fixture"]}}}"#.to_vec()),
        ("materials/fixture.material".into(), br#"{"materials":{"plain:entity_alphatest":{},"dyed:entity_alphatest":{"+defines":["USE_COLOR_MASK"]}}}"#.to_vec()),
    ];
    let mut textures = Vec::new();
    for definition in definitions {
        let texture = format!("textures/fixture/{}", definition.name.replace(':', "_"));
        let mut description = serde_json::json!({
            "identifier": definition.name,
            "materials":{"default":if definition.dyed {"dyed"} else {"plain"}},
            "textures":{"default":texture},
            "geometry":{"default":"geometry.fixture"},
            "render_controllers":["controller.render.fixture"]
        });
        if let Some(condition) = definition.condition {
            description["item"] = serde_json::json!({(ITEM):condition});
        }
        files.push((
            format!("attachables/{}.json", definition.name.replace(':', "_")).into(),
            serde_json::to_vec(&serde_json::json!({
                "format_version":"1.10.0","minecraft:attachable":{"description":description}
            }))
            .unwrap(),
        ));
        textures.push(assets::EquipmentTexture {
            identifier: texture.into(),
            width: 64,
            height: 32,
            rgba8: definition.color.repeat(64 * 32).into(),
        });
    }
    let compiled = pack_entity_compiler::compile_entity_pack(files)
        .unwrap()
        .unwrap();
    assets::SessionEntityPack {
        assets: Arc::new(assets::RuntimeEntityAssets::from_compiled(compiled.assets).unwrap()),
        textures: Arc::from([]),
        bindings: Arc::from([]),
        equipment: Some(Arc::new(
            assets::RuntimeEquipmentCatalog::from_parts(
                [1; 32],
                compiled.equipment_bindings.into_vec(),
                textures,
            )
            .unwrap(),
        )),
    }
}

/// Admits a local player through actor authority and resolves its real compiled rig.
fn owner(entities: Arc<assets::RuntimeEntityAssets>) -> client_world::WorldAuthority {
    let mut world = client_world::WorldAuthority::new(
        protocol::WorldBootstrap {
            local_player_runtime_id: 1,
            local_player_unique_id: 1,
            dimension: 0,
            player_position: [0.0; 3],
            world_spawn_position: [0; 3],
            air_network_id: 0,
            block_network_ids_are_hashes: false,
        },
        Arc::new(assets::RuntimeAssets::diagnostic()),
        Some(entities),
        [0.0; 3],
        None,
    );
    world
        .apply_ordered_event(
            protocol::WorldEvent::Actor(protocol::ActorEvent::Spawn(protocol::ActorSpawnEvent {
                dimension: 0,
                unique_id: 1,
                runtime_id: 1,
                kind: protocol::ActorKind::Player {
                    uuid: [1; 16],
                    username: "Preview fixture".into(),
                },
                position: [0.0; 3],
                velocity: [0.0; 3],
                pitch: 0.0,
                yaw: 0.0,
                head_yaw: 0.0,
                body_yaw: 0.0,
                held_item: Default::default(),
                metadata: Arc::from([]),
                attributes: Arc::from([]),
                properties: Arc::from([]),
                links: Arc::from([]),
            })),
            Some(1),
        )
        .unwrap();
    world
}

/// Dresses all preview views and checks the artwork, atlas texels and GPU dye policy.
fn check_preview(pack: assets::SessionEntityPack, expected: [u8; 4], dyed: bool, name: &str) {
    let base = catalog(&[Definition {
        name: ITEM,
        condition: None,
        color: STARTUP,
        dyed: true,
    }]);
    let world = owner(Arc::clone(&base.assets));
    let mut presentation = material_tests::presentation(base.equipment.as_ref().unwrap());
    presentation.set_preview_equipment_assets(Some(base.assets), Some(pack.assets));
    presentation.set_preview_pack_equipment(pack.equipment);
    let input = actor_animation::AttachableAnimationInput {
        is_local_player: true,
        frame_alpha: 1.0,
        ..Default::default()
    };
    presentation.set_player_preview_gear_with_owner(
        [None, Some((ITEM, Some(DYE))), None, None],
        None,
        PreviewEquipmentOwner {
            actor: world.actor(1).unwrap(),
            rig: world.actor_rig(1).unwrap(),
            input,
        },
    );
    let mut capture = image::RgbaImage::new(
        player_preview::PREVIEW_WIDTH * 3,
        player_preview::PREVIEW_HEIGHT,
    );
    for (column, view) in material_tests::views().into_iter().enumerate() {
        let pixels = player_preview::render(
            &[120, 150, 180, 255].repeat(64 * 64),
            Default::default(),
            view,
            0.0,
            &presentation.player_preview_gear,
        );
        let image = image::RgbaImage::from_raw(
            player_preview::PREVIEW_WIDTH,
            player_preview::PREVIEW_HEIGHT,
            pixels,
        )
        .unwrap();
        image::imageops::replace(
            &mut capture,
            &image,
            column as i64 * i64::from(image.width()),
            0,
        );
    }
    super::super::super::forms::snapshot::write_image(&capture, name);
    let armor = presentation.player_preview_gear.armor[1].as_ref().unwrap();
    assert_eq!(&armor.rgba[..4], &expected);
    let tint = dyed.then_some([(DYE >> 16) as u8, (DYE >> 8) as u8, DYE as u8]);
    assert_eq!(armor.tint, tint);
    let icon = presentation
        .gui_models
        .pack_equipment
        .region(1)
        .unwrap_or_else(|| {
            presentation.gui_models.textures[&atlas::key([armor.width, armor.height], &armor.rgba)]
        });
    let page = &presentation.textures.pages()[usize::from(icon.page)];
    let offset =
        (usize::from(icon.uv[1]) * page.dimensions()[0] as usize + usize::from(icon.uv[0])) * 4;
    assert_eq!(&page.pixels()[offset..offset + 4], &expected);
    for view in material_tests::views() {
        presentation.player_preview_view = view;
        let mesh = presentation.gui_player_mesh().unwrap();
        assert_eq!(mesh.batches()[1].texture_page, icon.page);
        assert_eq!(
            material_tests::armor_shading(&presentation),
            (
                vec![tint.map_or([255; 4], |[r, g, b]| [r, g, b, 255])],
                dyed
            ),
            "{view:?}"
        );
    }
}

#[test]
fn player_preview_ignores_a_stand_definition_that_sorts_after_the_player() {
    let pack = catalog(&[
        Definition {
            name: "fixture:a_player",
            condition: Some(
                "q.owner_identifier == 'minecraft:player' && c.item_slot == 'torso' && q.is_local_player",
            ),
            color: PLAYER,
            dyed: false,
        },
        Definition {
            name: "fixture:z_stand",
            condition: Some("q.owner_identifier == 'minecraft:armor_stand'"),
            color: STAND,
            dyed: true,
        },
    ]);
    assert_eq!(
        pack.equipment
            .as_ref()
            .unwrap()
            .binding(ITEM)
            .unwrap()
            .attachable_identifier(),
        "fixture:z_stand"
    );
    check_preview(pack, PLAYER, false, "armour-preview-owner");
}

#[test]
fn player_preview_keeps_startup_art_when_only_a_stand_override_is_admitted() {
    check_preview(
        catalog(&[Definition {
            name: "fixture:z_stand",
            condition: Some("q.owner_identifier == 'minecraft:armor_stand'"),
            color: STAND,
            dyed: false,
        }]),
        STARTUP,
        true,
        "armour-preview-startup",
    );
}

#[test]
fn player_preview_rejects_malformed_and_exhausted_owner_predicates() {
    for condition in [
        "q.owner_identifier ==",
        "loop(1024, { v.spin = v.spin + 1; }); return 1;",
    ] {
        check_preview(
            catalog(&[Definition {
                name: "fixture:z_invalid",
                condition: Some(condition),
                color: STAND,
                dyed: false,
            }]),
            STARTUP,
            true,
            "armour-preview-invalid",
        );
    }
}
