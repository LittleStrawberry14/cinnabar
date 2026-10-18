//! Program selection, active directives and compiled interface contracts.

use crate::{glsl_has, program, reflection};
use shaderpack::{PackSource, Phase, ShaderPack, StageKind, TranslateConfig};

const VERTEX: &str = "#version 450\nvoid main() { gl_Position = vec4(0.0); }";
const FRAGMENT: &str = "#version 450\nout vec4 color; void main() { color = vec4(1.0); }";

#[test]
fn dimension_program_sets_replace_the_root_as_a_whole() {
    let pack = ShaderPack::new(PackSource::from_files([
        ("composite.vsh", VERTEX),
        ("composite.fsh", FRAGMENT),
        ("final.vsh", VERTEX),
        ("final.fsh", FRAGMENT),
        ("world0/composite.vsh", VERTEX),
        (
            "world0/begin.csh",
            "#version 450\nlayout(local_size_x=1) in; void main() {}",
        ),
    ]));
    let selected = pack.programs(&TranslateConfig::default());
    assert_eq!(
        selected.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["begin", "composite"]
    );
    let composite = selected.iter().find(|p| p.name == "composite").unwrap();
    assert_eq!(
        composite.stages,
        [(StageKind::Vertex, "world0/composite.vsh".to_string())]
    );
    let translated = pack.translate(&TranslateConfig::default());
    assert!(!program(&translated, "composite").is_ok());
    assert!(
        program(&translated, "composite")
            .diagnostics
            .iter()
            .any(|d| d.phase == Phase::Link)
    );
}

#[test]
fn explicit_dimension_mappings_use_root_for_unmapped_ids() {
    let pack = ShaderPack::new(PackSource::from_files([
        (
            "dimension.properties",
            "dimension.custom = minecraft:the_nether",
        ),
        ("final.vsh", VERTEX),
        ("final.fsh", FRAGMENT),
        ("world0/composite.vsh", VERTEX),
        ("world0/composite.fsh", FRAGMENT),
    ]));
    assert_eq!(pack.dimension_folder("minecraft:overworld"), "");
    let config = TranslateConfig {
        folder: pack.dimension_folder("minecraft:overworld"),
        ..TranslateConfig::default()
    };
    let selected = pack.programs(&config);
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "final");
}

#[test]
fn dimension_ids_normalize_namespaces_and_last_mapping_wins() {
    let pack = ShaderPack::new(PackSource::from_files([
        (
            "dimension.properties",
            "dimension.first = the_nether\ndimension.last = minecraft:the_nether\ndimension.other = the_end *",
        ),
        ("first/final.vsh", VERTEX),
        ("last/final.vsh", VERTEX),
        ("other/final.vsh", VERTEX),
    ]));
    assert_eq!(pack.dimension_folder("minecraft:the_nether"), "last");
    assert_eq!(pack.dimension_folder("the_nether"), "last");
    assert_eq!(pack.dimension_folder("minecraft:the_end"), "other");
    assert_eq!(pack.dimension_folder("custom:moon"), "other");
}

#[test]
fn active_comment_dispatch_directives_follow_options_and_source_order() {
    let pack = ShaderPack::new(PackSource::from_files([(
        "begin.csh",
        "#version 450
        #define FIXED
        layout(local_size_x=4) in;
        /*
        const ivec3 workGroups = ivec3(1, 1, 1);
        */
        #ifdef FIXED
        /*
        const ivec3 workGroups = ivec3(4, 2, 1);
        */
        #else
        /*
        const vec2 workGroupsRender = vec2(0.5, 0.25);
        */
        #endif
        void main() {}",
    )]));
    let fixed = pack.translate(&TranslateConfig::default());
    let dispatch = reflection(program(&fixed, "begin"))
        .compute
        .as_ref()
        .unwrap();
    assert_eq!(dispatch.work_groups, Some([4, 2, 1]));
    assert_eq!(dispatch.work_groups_render, None);
    let mut config = TranslateConfig::default();
    config.options.insert("FIXED".into(), "false".into());
    let relative = pack.translate(&config);
    let dispatch = reflection(program(&relative, "begin"))
        .compute
        .as_ref()
        .unwrap();
    assert_eq!(dispatch.work_groups, Some([1, 1, 1]));
    assert_eq!(dispatch.work_groups_render, Some([0.5, 0.25]));
}

#[test]
fn include_filenames_end_before_trailing_comments() {
    for include in [
        "#include \"/lib/common.glsl\" // shared",
        "#include \"/lib/common.glsl\" /* shared */",
    ] {
        let source =
            format!("#version 450\n{include}\nout vec4 color; void main() {{ color = tint(); }}");
        let pack = ShaderPack::new(PackSource::from_files([
            ("composite.vsh", VERTEX),
            ("composite.fsh", &source),
            ("lib/common.glsl", "vec4 tint() { return vec4(1.0); }"),
        ]));
        reflection(program(
            &pack.translate(&TranslateConfig::default()),
            "composite",
        ));
    }
}

#[test]
fn repeated_layout_qualifiers_preserve_entries_and_last_values() {
    let pack = ShaderPack::new(PackSource::from_files([
        (
            "composite.vsh",
            "#version 450\nlayout(location=1) layout(location=3) out vec2 uv; void main() { uv = vec2(0.5); gl_Position = vec4(0.0); }",
        ),
        (
            "composite.fsh",
            "#version 450
        layout(std430) layout(binding=1) layout(binding=2) buffer Data { vec4 value; } data;
        layout(location=3) in vec2 uv; out vec4 color;
        void main() { color = data.value + vec4(uv, 0.0, 1.0); }",
        ),
    ]));
    let translated = pack.translate(&TranslateConfig::default());
    let reflected = reflection(program(&translated, "composite"));
    assert_eq!(reflected.storage_buffers[0].binding, 2);
}

#[test]
fn unused_fragment_inputs_are_removed_from_the_compiled_interface() {
    let pack = ShaderPack::new(PackSource::from_files([
        ("composite.vsh", VERTEX),
        (
            "composite.fsh",
            "#version 450\nin vec2 unusedUv; out vec4 color; void main() { color = vec4(1.0); }",
        ),
    ]));
    let translated = pack.translate(&TranslateConfig::default());
    let pass = program(&translated, "composite");
    reflection(pass);
    let stage = pass
        .stages
        .iter()
        .find(|s| s.stage == StageKind::Fragment)
        .unwrap()
        .result
        .as_ref()
        .unwrap();
    assert!(stage.module.entry_points[0].function.arguments.is_empty());
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&stage.module)
    .unwrap();
}

#[test]
fn actual_dispatch_directives_use_preprocessed_values() {
    let pack = ShaderPack::new(PackSource::from_files([(
        "begin.csh",
        "#version 450
        #define GROUP_X 4
        layout(local_size_x=1) in;
        const ivec3 workGroups = ivec3(GROUP_X, 2, 1);
        void main() {}",
    )]));
    let translated = pack.translate(&TranslateConfig::default());
    assert_eq!(
        reflection(program(&translated, "begin"))
            .compute
            .as_ref()
            .unwrap()
            .work_groups,
        Some([4, 2, 1])
    );
}

#[test]
fn dimension_properties_replace_keys_without_reordering_them() {
    let pack = ShaderPack::new(PackSource::from_files([
        (
            "dimension.properties",
            "dimension.a = overworld\ndimension.b = the_end\ndimension.a = the_end",
        ),
        ("a/final.vsh", VERTEX),
        ("b/final.vsh", VERTEX),
    ]));
    assert_eq!(pack.dimension_folder("overworld"), "");
    assert_eq!(pack.dimension_folder("the_end"), "b");
}

#[test]
fn legacy_and_absent_override_folders_choose_one_base_set() {
    for properties in ["", "# only comments"] {
        let pack = ShaderPack::new(PackSource::from_files([
            ("dimension.properties", properties),
            ("world0/final.vsh", VERTEX),
            ("world-1/composite.vsh", VERTEX),
            ("final.vsh", VERTEX),
        ]));
        assert_eq!(pack.dimension_folder("the_nether"), "world-1");
        assert_eq!(pack.dimension_folder("the_end"), "world0");
        assert_eq!(pack.dimension_folder("custom:moon"), "world0");
        let selected = pack.programs(&TranslateConfig {
            folder: "missing".into(),
            ..TranslateConfig::default()
        });
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].folder, "world0");
    }
}

#[test]
fn include_comments_and_malformed_arguments_keep_file_boundaries() {
    for include in [
        "# include \"/lib/common.glsl\" /* shared\ncontinued */",
        "/* leading */ #include \"/lib/common.glsl\"",
        "/* previous\n*/ #include \"/lib/common.glsl\"",
    ] {
        let source = format!(
            "#version 450\n{include}\n/*\n#include \"missing.glsl\"\n*/\nout vec4 color; void main() {{ color = tint(); }}"
        );
        let pack = ShaderPack::new(PackSource::from_files([
            ("composite.vsh", VERTEX),
            ("composite.fsh", &source),
            ("lib/common.glsl", "vec4 tint() { return vec4(1.0); }"),
        ]));
        reflection(program(
            &pack.translate(&TranslateConfig::default()),
            "composite",
        ));
    }
    for include in [
        "#include",
        "#include \"\"",
        "#include \"missing",
        "#include \"/lib/common.glsl\" extra",
    ] {
        let source = format!("#version 450\n{include}\nvoid main() {{}}");
        let pack = ShaderPack::new(PackSource::from_files([("begin.csh", source.as_str())]));
        let translated = pack.translate(&TranslateConfig::default());
        let failed = program(&translated, "begin");
        assert!(!failed.is_ok());
        assert!(
            failed
                .all_diagnostics()
                .any(|d| d.phase == Phase::Include && d.location.is_some())
        );
    }
}

#[test]
fn fragment_input_cleanup_preserves_live_helpers_and_remaps_arguments() {
    let pack = ShaderPack::new(PackSource::from_files([
        ("composite.vsh", "#version 450\nlayout(location=7) out vec2 uv; void main() { uv = vec2(0.5); gl_Position = vec4(0.0); }"),
        ("composite.fsh", "#version 450\nlayout(location=1) in vec2 unusedUv; layout(location=7) in vec2 uv;
        vec2 readUv() { return uv; } out vec4 color; void main() { color = vec4(readUv(), gl_FragCoord.xy); }"),
    ]));
    let translated = pack.translate(&TranslateConfig::default());
    let pass = program(&translated, "composite");
    reflection(pass);
    let compiled = pass.stages[1].result.as_ref().unwrap();
    let arguments = &compiled.module.entry_points[0].function.arguments;
    let locations: Vec<_> = arguments
        .iter()
        .filter_map(|a| match a.binding {
            Some(naga::Binding::Location { location, .. }) => Some(location),
            _ => None,
        })
        .collect();
    assert_eq!(locations, [7]);
    assert!(arguments.iter().any(|a| matches!(
        a.binding,
        Some(naga::Binding::BuiltIn(naga::BuiltIn::Position { .. }))
    )));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&compiled.module)
    .unwrap();
}

#[test]
fn unused_flattened_inputs_drop_synthetic_copies_and_unfed_live_inputs_read_zero() {
    for fragment in [
        "#version 450\nin vec2 unusedUv[2]; out vec4 color; void main() { color = vec4(1.0); }",
        "#version 450\nin mat2 unusedBasis; out vec4 color; void main() { color = vec4(1.0); }",
    ] {
        let pack = ShaderPack::new(PackSource::from_files([
            ("composite.vsh", VERTEX),
            ("composite.fsh", fragment),
        ]));
        let translated = pack.translate(&TranslateConfig::default());
        let pass = program(&translated, "composite");
        reflection(pass);
        assert!(
            pass.stages[1].result.as_ref().unwrap().module.entry_points[0]
                .function
                .arguments
                .is_empty()
        );
    }
    let pack = ShaderPack::new(PackSource::from_files([
        ("composite.vsh", VERTEX),
        (
            "composite.fsh",
            "#version 450\nin vec2 requiredUv; vec2 readUv() { return requiredUv; } out vec4 color; void main() { color = vec4(readUv(), 0.0, 1.0); }",
        ),
    ]));
    // A live input no vertex output feeds reads zero, as Iris declares it.
    let translated = pack.translate(&TranslateConfig::default());
    let pass = program(&translated, "composite");
    reflection(pass);
    let fragment = pass.stages[1].result.as_ref().unwrap();
    assert!(
        fragment.module.entry_points[0]
            .function
            .arguments
            .is_empty()
    );
    assert!(glsl_has(&fragment.glsl, "requiredUv = vec2(0);"));
}

#[test]
fn selected_directory_owns_enable_rules_and_buffer_directives() {
    let pack = ShaderPack::new(PackSource::from_files([
        (
            "shaders.properties",
            "program.final.enabled = true\nprogram.world0/final.enabled = false",
        ),
        ("final.vsh", VERTEX),
        ("final.fsh", FRAGMENT),
    ]));
    let root = pack.translate(&TranslateConfig::default());
    let final_pass = program(&root, "final");
    assert!(final_pass.enabled);
    reflection(final_pass);

    let pack = ShaderPack::new(PackSource::from_files([
        ("composite.vsh", VERTEX),
        (
            "composite.fsh",
            "#version 450\n/*\nconst int colortex1Format = RGBA16F;\n*/\nout vec4 color; void main() { color = vec4(1.0); }",
        ),
        ("world0/composite.vsh", VERTEX),
        ("world0/composite.fsh", FRAGMENT),
    ]));
    let selected = pack.translate(&TranslateConfig::default());
    assert_eq!(
        selected.buffers.format(shaderpack::BufferId::Color(1)),
        shaderpack::BufferFormat::DEFAULT
    );
    let root = pack.translate(&TranslateConfig {
        folder: String::new(),
        ..TranslateConfig::default()
    });
    assert_eq!(
        root.buffers.format(shaderpack::BufferId::Color(1)),
        shaderpack::BufferFormat::Rgba16f
    );
}

#[test]
fn deep_includes_expand_without_call_stack_recursion() {
    let mut files = vec![("composite.vsh".to_string(), VERTEX.to_string()),
        ("composite.fsh".to_string(), "#version 450\n#include \"/lib/0.glsl\"\nout vec4 color; void main() { color = tint(); }".to_string())];
    for i in 0..1024 {
        files.push((
            format!("lib/{i}.glsl"),
            if i == 1023 {
                "vec4 tint() { return vec4(1.0); }".to_string()
            } else {
                format!("#include \"{}.glsl\"", i + 1)
            },
        ));
    }
    let pack = ShaderPack::new(PackSource::from_files(files));
    reflection(program(
        &pack.translate(&TranslateConfig::default()),
        "composite",
    ));
}

#[test]
fn unqualified_dimension_mappings_match_namespaced_queries() {
    let pack = ShaderPack::new(PackSource::from_files([
        (
            "dimension.properties",
            "dimension.customNether = the_nether",
        ),
        ("customNether/final.vsh", ""),
        ("customNether/final.fsh", ""),
        ("world-1/composite.vsh", ""),
        ("world-1/composite.fsh", ""),
    ]));
    let folder = pack.dimension_folder("minecraft:the_nether");
    assert_eq!(folder, "customNether");
    let selected = pack.programs(&TranslateConfig {
        folder,
        ..TranslateConfig::default()
    });
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "final");
    assert_eq!(selected[0].folder, "customNether");
}
