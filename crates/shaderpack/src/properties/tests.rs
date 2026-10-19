use std::collections::BTreeMap;

use super::*;

/// Parses test properties without predefined macros.
fn parse(text: &str) -> ShaderProperties {
    ShaderProperties::parse(text, &BTreeMap::new())
}

/// Parses test properties with the supplied macro definitions.
fn parse_with(text: &str, defines: &[(&str, &str)]) -> ShaderProperties {
    let defines = defines
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    ShaderProperties::parse(text, &defines)
}

/// Returns property keys in source order for assertions.
fn keys(properties: &ShaderProperties) -> Vec<&str> {
    properties
        .entries
        .iter()
        .map(|(key, _)| key.as_str())
        .collect()
}

/// Returns the source lines of property diagnostics in report order.
fn diagnostic_lines(properties: &ShaderProperties) -> Vec<u32> {
    properties
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.line)
        .collect()
}

#[test]
fn java_properties_separators_comments_and_escapes() {
    let properties = parse(
        "alpha=1\n  beta : two words  \ngamma 3\ndelta=\n! bang comment\n# hash comment\n\
         \n spaced.key   =   x = y\nesc\\=key=a\\tb\\u0041\n",
    );
    assert_eq!(
        properties.entries,
        [
            ("alpha".to_owned(), "1".to_owned()),
            ("beta".to_owned(), "two words".to_owned()),
            ("gamma".to_owned(), "3".to_owned()),
            ("delta".to_owned(), String::new()),
            ("spaced.key".to_owned(), "x = y".to_owned()),
            ("esc=key".to_owned(), "a\tbA".to_owned()),
        ]
    );
    assert!(properties.diagnostics.is_empty());
}

#[test]
fn duplicates_are_kept_and_get_returns_the_last() {
    let properties = parse("mode=first\nother=1\nmode = second \n");
    assert_eq!(keys(&properties), ["mode", "other", "mode"]);
    assert_eq!(properties.get("mode"), Some("second"));
    assert_eq!(properties.get("missing"), None);
}

#[test]
fn continuation_lines_join_without_leading_whitespace() {
    let properties = parse(
        "uniform.float.total = alpha + \\\n        beta + \\\n   gamma\nafter=1\n\
         path=dir\\\\\nnext=2\n",
    );
    assert_eq!(
        properties.get("uniform.float.total"),
        Some("alpha + beta + gamma")
    );
    assert_eq!(properties.get("after"), Some("1"));
    // An even number of trailing backslashes is an escaped backslash, not a continuation.
    assert_eq!(properties.get("path"), Some("dir\\"));
    assert_eq!(properties.get("next"), Some("2"));
}

#[test]
fn comment_lines_end_a_continuation_and_content_loses_hashes() {
    let properties = parse("list=one \\\n#two\nnext=a#b\n");
    assert_eq!(properties.get("list"), Some("one"));
    assert_eq!(properties.get("next"), Some("ab"));
}

#[test]
fn directives_apply_on_every_physical_line() {
    // An inactive continued entry cannot hide the `#endif` that closes its block.
    let properties = parse("#ifdef MISSING\nhidden=a \\\n#endif\nshown=1\n");
    assert_eq!(keys(&properties), ["shown"]);
    // A continuation backslash on a directive is part of its condition, as in Iris.
    let properties = parse_with("#if FLAG \\\n&& FLAG\n#endif\nafter=1\n", &[("FLAG", "")]);
    assert_eq!(keys(&properties), ["after"]);
    assert_eq!(diagnostic_lines(&properties), [1]);
}

#[test]
fn if_elif_else_select_one_branch() {
    let text = "#if LEVEL == 1\npick=one\n#elif LEVEL == 2\npick=two\n#else\npick=other\n#endif\n";
    assert_eq!(parse_with(text, &[("LEVEL", "1")]).get("pick"), Some("one"));
    assert_eq!(parse_with(text, &[("LEVEL", "2")]).get("pick"), Some("two"));
    assert_eq!(
        parse_with(text, &[("LEVEL", "7")]).get("pick"),
        Some("other")
    );
    assert_eq!(parse(text).get("pick"), Some("other"));
    let properties = parse_with(text, &[("LEVEL", "2")]);
    assert_eq!(keys(&properties), ["pick"]);
}

#[test]
fn ifdef_ifndef_and_defined_use_the_define_table() {
    let text = "#ifdef FEATURE\na=1\n#endif\n#ifndef FEATURE\nb=1\n#endif\n\
                # if defined(FEATURE) && !defined MISSING\nc=1\n#endif\n\
                \t#if FEATURE\nd=1\n#endif\n#if WORD\ne=1\n#endif\n#if HEX == 16\nf=1\n#endif\n";
    let properties = parse_with(text, &[("FEATURE", ""), ("WORD", "abc"), ("HEX", "0x10")]);
    assert_eq!(keys(&properties), ["a", "c", "d", "f"]);
    assert!(properties.diagnostics.is_empty());
    let properties = parse(text);
    assert_eq!(keys(&properties), ["b"]);
}

#[test]
fn define_and_undef_affect_later_conditionals_only_when_active() {
    let text = "#define LOCAL 4\n#if LOCAL > 3\na=1\n#endif\n#undef LOCAL\n#ifdef LOCAL\nb=1\n#endif\n\
                #if 0\n#define HIDDEN\n#endif\n#ifdef HIDDEN\nc=1\n#endif\n";
    assert_eq!(keys(&parse(text)), ["a"]);
}

#[test]
fn nested_blocks_inside_inactive_regions_stay_inactive() {
    let text = "#if 0\n#if 1\na=1\n#else\nb=1\n#endif\nc=1\n#elif 1\nd=1\n#endif\ne=1\n";
    assert_eq!(keys(&parse(text)), ["d", "e"]);
}

#[test]
fn commented_keys_and_prose_are_plain_comments() {
    let properties = parse("#uniform.float.hidden=1\n#ifdefined stuff\n# Notes about it\nreal=1\n");
    assert_eq!(keys(&properties), ["real"]);
    assert!(properties.diagnostics.is_empty());
}

#[test]
fn unbalanced_directives_are_diagnosed_and_parsing_continues() {
    let properties =
        parse("#endif\na=1\n#else\nb=1\n#if (\nc=1\n#endif\n#ifdef\nd=1\n#endif\n#if 1\ne=1\n");
    assert_eq!(keys(&properties), ["a", "b", "e"]);
    assert_eq!(diagnostic_lines(&properties), [1, 3, 5, 8, 11]);
}

#[test]
fn duplicate_else_and_elif_after_else_are_diagnosed() {
    let properties = parse("#if 0\n#else\na=1\n#else\nb=1\n#elif 1\nc=1\n#endif\n");
    assert_eq!(keys(&properties), ["a"]);
    assert_eq!(diagnostic_lines(&properties), [4, 6]);
}

#[test]
fn program_rules_prefer_dimension_and_last_occurrence() {
    let properties = parse(
        "program.composite1.enabled = BLOOM\nprogram.world0/composite1.enabled = BLOOM && !FAST\n\
         program.world0/composite1.enabled = TAA\nprogram.shadow.enabled=SHADOW\n\
         program.world0/shadow.enabled = (\n",
    );
    assert_eq!(properties.program_rules.len(), 3);
    let rule = properties
        .program_rule("world0", "composite1")
        .expect("rule");
    assert_eq!(rule.dimension.as_deref(), Some("world0"));
    assert_eq!(rule.expr, Expr::Ident("TAA".to_owned()));
    assert_eq!(rule.line, 3);
    let rule = properties
        .program_rule("world-1", "composite1")
        .expect("rule");
    assert_eq!(rule.dimension, None);
    assert_eq!(rule.line, 1);
    assert_eq!(
        properties
            .program_rule("world0", "shadow")
            .map(|rule| rule.line),
        Some(4)
    );
    assert!(properties.program_rule("world0", "deferred").is_none());
    assert_eq!(diagnostic_lines(&properties), [5]);
}

#[test]
fn custom_textures_file_resource_raw_noise_and_malformed() {
    let properties = parse(
        "texture.noise = /textures/grain.png\ntexture.composite.colortex5 = textures/dust.png\n\
         texture.gbuffers.gaux1 = example:textures/sky/stars.png\n\
         texture.deferred.colortex9.1 = data/lut.bin TEXTURE_3D RGBA8 16 16 16 RGBA UNSIGNED_BYTE\n\
         texture.composite = textures/none.png\ntexture.deferred.colortex2 =\n\
         texture.composite.colortex5 = textures/dust2.png\n",
    );
    assert_eq!(
        properties.custom_textures,
        [
            CustomTexture {
                stage: "noise".to_owned(),
                sampler: "noisetex".to_owned(),
                source: TextureSource::File("textures/grain.png".to_owned()),
            },
            CustomTexture {
                stage: "composite".to_owned(),
                sampler: "colortex5".to_owned(),
                source: TextureSource::File("textures/dust2.png".to_owned()),
            },
            CustomTexture {
                stage: "gbuffers".to_owned(),
                sampler: "gaux1".to_owned(),
                source: TextureSource::Resource("example:textures/sky/stars.png".to_owned()),
            },
            CustomTexture {
                stage: "deferred".to_owned(),
                sampler: "colortex9.1".to_owned(),
                source: TextureSource::Raw {
                    path: "data/lut.bin".to_owned(),
                    spec: [
                        "TEXTURE_3D",
                        "RGBA8",
                        "16",
                        "16",
                        "16",
                        "RGBA",
                        "UNSIGNED_BYTE"
                    ]
                    .map(str::to_owned)
                    .to_vec(),
                },
            },
        ]
    );
    assert_eq!(diagnostic_lines(&properties), [5, 6]);
}

#[test]
fn custom_textures_bind_their_unit_uniform_in_their_stage_programs() {
    let properties = parse(
        "texture.noise = noise.png\ntexture.composite.colortex6.1 = lut.png\n\
         texture.gbuffers.gaux2 = water.png\ntexture.setup.colortex7 = seed.png\n",
    );
    let [noise, composite, gbuffers, setup] = properties.custom_textures.as_slice() else {
        panic!("{:?}", properties.custom_textures);
    };
    assert_eq!(noise.uniform(), "noisetex");
    assert_eq!(composite.uniform(), "colortex6");
    assert_eq!(gbuffers.uniform(), "gaux2");
    assert!(noise.applies_to(ProgramGroup::Shadow) && noise.applies_to(ProgramGroup::Final));
    assert!(composite.applies_to(ProgramGroup::Composite));
    assert!(composite.applies_to(ProgramGroup::Final));
    assert!(!composite.applies_to(ProgramGroup::Deferred));
    assert!(gbuffers.applies_to(ProgramGroup::Gbuffers));
    assert!(gbuffers.applies_to(ProgramGroup::Shadow));
    assert!(!gbuffers.applies_to(ProgramGroup::ShadowComposite));
    assert!(setup.applies_to(ProgramGroup::Setup));
    assert!(!setup.applies_to(ProgramGroup::Begin));
}

#[test]
fn custom_images_relative_absolute_and_malformed() {
    let properties = parse(
        "image.halfImg = halfSampler rgba rgba16f half_float true true 0.5 0.5\n\
         image.flatImg = flatSampler red r32f float false false 32 8\n\
         image.cubeImg = cubeSampler red_integer r8ui unsigned_byte true false 16 16 16\n\
         image.badBool = s rgba rgba8 unsigned_byte sometimes false 4 4\n\
         image.badRelative = s rgba rgba8 unsigned_byte true true 1 1 1\n\
         image.tooShort = s rgba rgba8 unsigned_byte true false\n\
         image.zero = s rgba rgba8 unsigned_byte true false 0 4\n\
         image.lineImg = lineSampler red r32f float false false 256\n\
         image.cubeImg = cubeSampler red r32f float false false 8 8 8\n",
    );
    let image = |name: &str, sampler: &str, format: &str, internal: &str, pixel: &str| {
        (
            name.to_owned(),
            sampler.to_owned(),
            format.to_owned(),
            internal.to_owned(),
            pixel.to_owned(),
        )
    };
    let summary: Vec<_> = properties
        .images
        .iter()
        .map(|img| {
            (
                (
                    img.name.clone(),
                    img.sampler.clone(),
                    img.format.clone(),
                    img.internal_format.clone(),
                    img.pixel_type.clone(),
                ),
                img.clear,
                img.size,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                image("halfImg", "halfSampler", "rgba", "rgba16f", "half_float"),
                true,
                ImageSize::Relative {
                    width: 0.5,
                    height: 0.5,
                },
            ),
            (
                image("flatImg", "flatSampler", "red", "r32f", "float"),
                false,
                ImageSize::Absolute {
                    width: 32,
                    height: Some(8),
                    depth: None,
                },
            ),
            (
                image("cubeImg", "cubeSampler", "red", "r32f", "float"),
                false,
                ImageSize::Absolute {
                    width: 8,
                    height: Some(8),
                    depth: Some(8),
                },
            ),
            (
                image("lineImg", "lineSampler", "red", "r32f", "float"),
                false,
                ImageSize::Absolute {
                    width: 256,
                    height: None,
                    depth: None,
                },
            ),
        ]
    );
    assert_eq!(diagnostic_lines(&properties), [4, 5, 6, 7]);
}

#[test]
fn unknown_keys_stay_only_in_entries() {
    let properties = parse("sun=true\nshadow.culling=reversed\nscreen=[MAIN] SPEED\n");
    assert_eq!(keys(&properties), ["sun", "shadow.culling", "screen"]);
    assert_eq!(properties.get("screen"), Some("[MAIN] SPEED"));
    assert!(properties.program_rules.is_empty());
    assert!(properties.diagnostics.is_empty());
}

#[test]
fn a_malformed_last_declaration_leaves_no_rule_for_its_key() {
    let properties = parse(
        "image.lut = s red r32f float false false 16\nimage.lut = s red r32f float false false 0\n",
    );
    assert!(properties.images.is_empty());
    assert_eq!(diagnostic_lines(&properties), [2]);
}

#[test]
fn malformed_entries_stay_in_entries() {
    let properties = parse("image.cimage1 = sampler rgba rgba8 unsigned_byte maybe true 1.0 1.0\n");
    assert_eq!(
        properties.get("image.cimage1"),
        Some("sampler rgba rgba8 unsigned_byte maybe true 1.0 1.0")
    );
    assert!(properties.images.is_empty());
    assert_eq!(diagnostic_lines(&properties), [1]);
}

#[test]
fn conditions_use_c_integer_arithmetic_and_macro_aliases() {
    let defines = [
        ("MC_VERSION", "12111"),
        ("ALIAS", "QUALITY"),
        ("QUALITY", "2"),
    ];
    let properties = parse_with(
        "#if MC_VERSION / 10000 == 1\ndivided=1\n#endif\n\
         #if ALIAS >= 2\naliased=1\n#endif\n#if UNDEFINED\nhidden=1\n#endif\n",
        &defines,
    );
    assert_eq!(keys(&properties), ["divided", "aliased"]);
    assert!(properties.diagnostics.is_empty());
}

#[test]
fn values_expand_object_like_macros() {
    let properties = parse_with(
        "#define LUT_SIZE SIZE\nimage.lut = s red r32f float false false LUT_SIZE\nflag=FLAG 0.5\n",
        &[("SIZE", "256"), ("FLAG", "")],
    );
    assert_eq!(
        properties.images[0].size,
        ImageSize::Absolute {
            width: 256,
            height: None,
            depth: None,
        }
    );
    assert_eq!(properties.get("flag"), Some("1 0.5"));
}
