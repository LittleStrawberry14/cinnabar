//! First-person hand placement and artwork share one admitted definition.
use super::*;

#[test]
fn java_hand_reuses_its_cold_selection_when_drawing_the_bow() {
    let (stream, mut equipment, pages, input) =
        fixture(Some(assets::facts::VanillaItem::Bow.identifier()));
    let mut files: Vec<(Box<str>, Vec<u8>)> = vec![
        ("models/item.json".into(), br#"{"format_version":"1.12.0","minecraft:geometry":[{"description":{"identifier":"geometry.fixture.held","texture_width":16,"texture_height":16},"bones":[{"name":"item","binding":"q.item_slot_to_bone_name(c.item_slot)","cubes":[{"origin":[0,0,0],"size":[2,3,4],"uv":[0,0]}]}]}]}"#.to_vec()),
        ("render_controllers/item.json".into(), br#"{"format_version":"1.8.0","render_controllers":{"controller.render.fixture":{"geometry":"Geometry.default","materials":[{"*":"Material.default"}],"textures":["Texture.default"]}}}"#.to_vec()),
    ];
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(16, 16, image::Rgba([40, 190, 100, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    files.push(("textures/fixture.png".into(), png.into_inner()));
    let cost = assets::MAX_MOLANG_OPS_PER_EXPRESSION;
    let count = actor_animation::MAX_MOLANG_OPS_PER_ACTOR_TICK / cost;
    let definitions = std::iter::once(("fixture:accepted".to_owned(), "1".to_owned())).chain(
        (0..count).map(|index| {
            (
                format!("zfixture:rejected_{index}"),
                format!("{}!v.rejected_{index}", "0;".repeat((cost - 4) / 2)),
            )
        }),
    );
    for (name, condition) in definitions {
        files.push((format!("attachables/{}.json", name.replace(':', "_")).into(),
            serde_json::to_vec(&serde_json::json!({
                "format_version":"1.10.0", "minecraft:attachable":{"description":{
                    "identifier":name,"item":{(assets::facts::VanillaItem::Bow.identifier()):condition},
                    "materials":{"default":"entity_alphatest"},"textures":{"default":"textures/fixture"},
                    "geometry":{"default":"geometry.fixture.held"},"render_controllers":["controller.render.fixture"]
                }}
            })).unwrap()));
    }
    let compiled = pack_entity_compiler::compile_actor_pack(files)
        .unwrap()
        .unwrap();
    let catalog = Arc::new(
        assets::RuntimeEquipmentCatalog::from_parts(
            compiled.identity,
            compiled.equipment_bindings,
            compiled.equipment_textures,
        )
        .unwrap(),
    );
    let entities = Arc::new(assets::RuntimeEntityAssets::from_compiled(compiled.entities).unwrap());
    let (pages, locations) =
        pages.with_equipment_rasters(&EquipmentRuntime::pack_rasters(&catalog));
    equipment.set_pack_layer(Some((entities, Some(catalog), locations)));
    let mut cache = java::HandCache::default();
    for camera_space in [true, false, false] {
        let source = hand_source_for_mode(
            &stream,
            &mut equipment,
            &pages,
            &input,
            &mut cache,
            1.0,
            true,
        );
        let drawn = &source.items[0].as_ref().unwrap().0;
        assert_eq!(
            drawn.camera_space, camera_space,
            "hand placement must use the definition admitted before drawing"
        );
    }
}
