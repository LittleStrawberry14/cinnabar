//! Per-texture sampler classes: render targets clamp, depth is nearest, custom textures
//! follow their `.mcmeta`.

use crate::{glsl_has, program, reflection};
use shaderpack::{
    CompiledStage, PackSource, ProgramTranslation, SHADOW_SAMPLER_BINDING, SamplerClass,
    ShaderPack, StageKind, TEXTURE_GROUP, TranslateConfig,
};

const VERTEX: &str = "#version 120\nvarying vec2 uv;\nvoid main() { gl_Position = ftransform(); uv = gl_MultiTexCoord0.st; }\n";

fn pack(fragment: &str, properties: &str, extra: &[(&str, &str)]) -> ShaderPack {
    let mut files = vec![
        ("composite.vsh", VERTEX.to_owned()),
        ("composite.fsh", fragment.to_owned()),
        ("shaders.properties", properties.to_owned()),
    ];
    files.extend(extra.iter().map(|(k, v)| (*k, (*v).to_owned())));
    ShaderPack::new(PackSource::from_files(files))
}

fn fragment_stage(t: &ProgramTranslation) -> &CompiledStage {
    t.stages
        .iter()
        .find(|s| s.stage == StageKind::Fragment)
        .and_then(|s| s.result.as_ref().ok())
        .expect("fragment compiles")
}

/// (texture name, sampler class) of every filtering sample in the program's fragment
/// module, read from the bindings of the texture and sampler each sample uses.
fn samples(t: &ProgramTranslation) -> Vec<(String, SamplerClass)> {
    let r = reflection(t);
    let module = &fragment_stage(t).module;
    let functions = module
        .functions
        .iter()
        .map(|(_, f)| f)
        .chain(module.entry_points.iter().map(|e| &e.function));
    let mut out = vec![];
    for f in functions {
        let binding = |h: naga::Handle<naga::Expression>| match f.expressions[h] {
            naga::Expression::GlobalVariable(g) => module.global_variables[g].binding,
            _ => None,
        };
        for (_, e) in f.expressions.iter() {
            let naga::Expression::ImageSample { image, sampler, .. } = e else {
                continue;
            };
            let (Some(image), Some(sampler)) = (binding(*image), binding(*sampler)) else {
                continue;
            };
            let texture = r
                .textures
                .iter()
                .find(|t| t.group == image.group && t.binding == image.binding);
            if let (Some(texture), Some(class)) = (
                texture,
                SamplerClass::from_binding(sampler.binding)
                    .filter(|_| sampler.group == TEXTURE_GROUP),
            ) {
                out.push((texture.name.clone(), class));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn each_texture_reads_through_the_class_iris_gives_it() {
    let fragment = "#version 120\n\
        uniform sampler2D colortex0;\n\
        uniform sampler2D depthtex0;\n\
        uniform sampler2D noisetex;\n\
        uniform sampler2D gaux1;\n\
        uniform sampler2D dust;\n\
        uniform sampler2D grain;\n\
        uniform sampler2D lut;\n\
        uniform sampler2D unknownTex;\n\
        uniform sampler2DShadow shadowtex0;\n\
        varying vec2 uv;\n\
        vec4 tap(sampler2D s) { return texture2D(s, uv); }\n\
        void main() {\n\
            gl_FragData[0] = texture2D(colortex0, uv) + texture2D(depthtex0, uv) + texture2D(noisetex, uv)\n\
                + tap(gaux1) + texture2D(dust, uv) + texture2D(grain, uv) + texture2D(lut, uv)\n\
                + texture2D(unknownTex, uv) + vec4(shadow2D(shadowtex0, vec3(uv, 0.5)).r);\n\
        }\n";
    let properties = "texture.composite.dust = tex/dust.png\n\
        texture.composite.grain = tex/grain.png\n\
        texture.composite.lut = tex/lut.dat TEXTURE_2D RGBA8 16 16 RGBA UNSIGNED_BYTE\n\
        texture.deferred.colortex0 = tex/ignored.png\n";
    let mcmeta = r#"{ "texture": { "blur": true, "clamp" : false } }"#;
    let p = pack(fragment, properties, &[("tex/grain.png.mcmeta", mcmeta)]);
    let t = p.translate(&TranslateConfig::default());
    let composite = program(&t, "composite");
    let r = reflection(composite);
    let classes: Vec<(String, SamplerClass)> = r
        .samplers
        .iter()
        .filter_map(|s| s.class().map(|c| (s.name.clone(), c)))
        .collect();
    assert!(
        classes
            .iter()
            .all(|(name, class)| name == class.glsl_name())
    );
    let mut bound: Vec<SamplerClass> = classes.iter().map(|(_, c)| *c).collect();
    bound.sort();
    assert_eq!(bound, SamplerClass::ALL, "every class is used once");
    assert!(r.samplers.iter().any(|s| s.comparison
        && s.group == TEXTURE_GROUP
        && s.binding == SHADOW_SAMPLER_BINDING
        && s.class().is_none()));

    let sampled = samples(composite);
    let read = |texture: &str, class: SamplerClass| {
        let pair = (texture.to_owned(), class);
        assert!(sampled.contains(&pair), "missing {pair:?} in {sampled:?}");
    };
    read("colortex0", SamplerClass::ClampLinear);
    // The helper was specialized for the global, so the colour target keeps its class.
    read("gaux1", SamplerClass::ClampLinear);
    read("depthtex0", SamplerClass::ClampNearest);
    read("noisetex", SamplerClass::RepeatLinear);
    read("unknownTex", SamplerClass::RepeatLinear);
    // PNG without .mcmeta: nearest, repeat. With blur and no clamp: linear, repeat.
    read("dust", SamplerClass::RepeatNearest);
    read("grain", SamplerClass::RepeatLinear);
    // Raw textures default to blur and clamp.
    read("lut", SamplerClass::ClampLinear);
}

#[test]
fn custom_texture_classes_follow_the_program_stage_and_noise_override() {
    let fragment = "#version 120\n\
        uniform sampler2D colortex0;\n\
        uniform sampler2D noisetex;\n\
        varying vec2 uv;\n\
        void main() { gl_FragData[0] = texture2D(colortex0, uv) + texture2D(noisetex, uv); }\n";
    // `colortex0` is only replaced in deferred programs; `texture.noise` applies everywhere
    // and its clamp-only .mcmeta makes it nearest.
    let properties = "texture.deferred.colortex0 = tex/a.png\ntexture.noise = tex/noise.png\n";
    let mcmeta = r#"{"texture":{"clamp":true}}"#;
    let p = pack(fragment, properties, &[("tex/noise.png.mcmeta", mcmeta)]);
    let t = p.translate(&TranslateConfig::default());
    assert_eq!(
        samples(program(&t, "composite")),
        [
            ("colortex0".to_owned(), SamplerClass::ClampLinear),
            ("noisetex".to_owned(), SamplerClass::ClampNearest),
        ]
    );
}

#[test]
fn a_custom_texture_only_reclasses_samplers_of_its_shape() {
    // A 3D volume under `colortex0`: the 2D buffer read keeps its clamp/linear class, the
    // 3D read takes the volume's .mcmeta (no blur, no clamp).
    let fragment = "#version 120\n\
        uniform sampler2D colortex0;\n\
        varying vec2 uv;\n\
        void main() { gl_FragData[0] = texture2D(colortex0, uv); }\n";
    let volume = "#version 120\n\
        uniform sampler3D colortex0;\n\
        varying vec2 uv;\n\
        void main() { gl_FragData[0] = texture3D(colortex0, vec3(uv, 0.5)); }\n";
    let properties = "texture.composite.colortex0 = v.dat TEXTURE_3D R8 2 2 2 RED UNSIGNED_BYTE\n";
    let mcmeta = r#"{"texture":{"blur":false,"clamp":false}}"#;
    let p = pack(
        fragment,
        properties,
        &[
            ("v.dat.mcmeta", mcmeta),
            ("composite1.vsh", VERTEX),
            ("composite1.fsh", volume),
        ],
    );
    let t = p.translate(&TranslateConfig::default());
    assert_eq!(
        samples(program(&t, "composite")),
        [("colortex0".to_owned(), SamplerClass::ClampLinear)]
    );
    assert_eq!(
        samples(program(&t, "composite1")),
        [("colortex0".to_owned(), SamplerClass::RepeatNearest)]
    );
}

#[test]
fn locals_and_parameters_shadowing_a_sampler_stay_plain() {
    // Complementary's GGX declares `float specular` while `specular` is the global sampler;
    // SEUS's `vec4 tex = texture2D(tex, …)` reads the global in the initializer.
    let fragment = "#version 120\n\
        uniform sampler2D specular;\n\
        varying vec2 uv;\n\
        float ggx(float x) { float specular = x * 2.0; specular = specular / (specular + 1.0); return specular; }\n\
        float scale(float specular) { return specular * 0.5; }\n\
        void main() {\n\
            float s = ggx(uv.x);\n\
            { vec4 specular = texture2D(specular, uv) + vec4(s); s += specular.x; }\n\
            gl_FragData[0] = texture2D(specular, uv) * scale(s);\n\
        }\n";
    let p = pack(fragment, "", &[]);
    let t = p.translate(&TranslateConfig::default());
    let composite = program(&t, "composite");
    let glsl = &fragment_stage(composite).glsl;
    assert!(glsl_has(glsl, "float specular = x * 2.0;"), "{glsl}");
    assert!(glsl_has(glsl, "s += specular.x;"), "{glsl}");
    // The initializer and the last read both sample the global.
    let atlas = ("specular".to_owned(), SamplerClass::RepeatNearest);
    assert_eq!(samples(composite), [atlas.clone(), atlas]);
}

#[test]
fn overloads_taking_a_sampler_each_get_a_specialized_copy() {
    // Solas's water `Raytrace(depthtex1, …)` (10 parameters) calls its 9-parameter overload
    // with the same sampler; each overload gets its own copy for the sampler.
    // A `#====` banner line is an unknown directive Iris's preprocessor skips.
    let fragment = "#version 120\n\
        #====================#\n\
        #   Copyright banner #\n\
        uniform sampler2D depthtex1;\n\
        varying vec2 uv;\n\
        vec4 trace(sampler2D depth, vec2 p, float s) { return texture2D(depth, p) * s; }\n\
        vec3 trace(sampler2D depth, vec2 p, float s, out float hit) { vec4 c = trace(depth, p, s); hit = c.a; return c.rgb; }\n\
        void main() { float hit; vec3 c = trace(depthtex1, uv, 2.0, hit); gl_FragData[0] = vec4(c, hit); }\n";
    let p = pack(fragment, "", &[]);
    let t = p.translate(&TranslateConfig::default());
    let composite = program(&t, "composite");
    reflection(composite);
    let stage = fragment_stage(composite);
    assert!(!stage.glsl.contains("Copyright"), "{}", stage.glsl);
    // One copy per overload, each without the sampler parameter.
    let arities: Vec<usize> = stage
        .module
        .functions
        .iter()
        .map(|(_, f)| f.arguments.len())
        .collect();
    assert!(arities.contains(&2) && arities.contains(&3), "{arities:?}");
}

#[test]
fn sampler_class_bindings_precede_textures_and_round_trip() {
    for class in SamplerClass::ALL {
        assert_eq!(SamplerClass::from_binding(class.binding()), Some(class));
        assert!(class.binding() < shaderpack::FIRST_TEXTURE_BINDING);
        assert_ne!(class.binding(), SHADOW_SAMPLER_BINDING);
    }
    assert_eq!(
        SamplerClass::for_name("shadowcolor1"),
        SamplerClass::ClampLinear
    );
    assert_eq!(
        SamplerClass::for_name("gdepthtex"),
        SamplerClass::ClampNearest
    );
    assert_eq!(SamplerClass::for_name("gnormal"), SamplerClass::ClampLinear);
    assert_eq!(
        SamplerClass::for_name("noisetex"),
        SamplerClass::RepeatLinear
    );
    for atlas in ["gtexture", "texture", "tex", "normals", "specular"] {
        assert_eq!(SamplerClass::for_name(atlas), SamplerClass::RepeatNearest);
    }
    assert_eq!(
        SamplerClass::for_name("lightmap"),
        SamplerClass::ClampLinear
    );
    assert_eq!(shaderpack::ProgramGroup::Final.texture_stage(), "composite");
    let unit = shaderpack::CustomTexture {
        stage: "composite".to_owned(),
        sampler: "colortex6.1".to_owned(),
        source: shaderpack::TextureSource::File("a.png".to_owned()),
    };
    assert_eq!(unit.uniform(), "colortex6");
}
