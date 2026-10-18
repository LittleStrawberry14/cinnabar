//! Translation preserves source bindings and rejects incomplete program contracts.

use crate::{glsl_has, program, reflection};
use shaderpack::{PackSource, PackTranslation, Phase, ShaderPack, TranslateConfig};

const VERTEX: &str = "#version 330\nvoid main() { gl_Position = vec4(0.0); }";
const FRAGMENT: &str = "#version 330\nout vec4 color; void main() { color = vec4(1.0); }";

/// Translates an original in-memory render program without external fixtures.
fn translate(vertex: &str, fragment: &str) -> PackTranslation {
    ShaderPack::new(PackSource::from_files([
        ("composite.vsh", vertex),
        ("composite.fsh", fragment),
    ]))
    .translate(&TranslateConfig::default())
}

/// Requires a usable program and returns its complete translation for module inspection.
fn success(vertex: &str, fragment: &str) -> PackTranslation {
    let translation = translate(vertex, fragment);
    reflection(program(&translation, "composite"));
    translation
}

/// Collects named interface locations from direct values and result structures.
fn interface_locations(module: &naga::Module, output: bool) -> Vec<(&str, u32)> {
    let function = &module.entry_points[0].function;
    let values: Vec<_> = if output {
        function
            .result
            .iter()
            .map(|value| (None, value.ty, value.binding.as_ref()))
            .collect()
    } else {
        function
            .arguments
            .iter()
            .map(|value| (value.name.as_deref(), value.ty, value.binding.as_ref()))
            .collect()
    };
    let mut locations = Vec::new();
    for (name, ty, binding) in values {
        if let Some(naga::Binding::Location { location, .. }) = binding {
            locations.push((name.unwrap_or_default(), *location));
        } else if let naga::TypeInner::Struct { members, .. } = &module.types[ty].inner {
            locations.extend(members.iter().filter_map(|member| match member.binding {
                Some(naga::Binding::Location { location, .. }) => {
                    Some((member.name.as_deref().unwrap_or_default(), location))
                }
                _ => None,
            }));
        }
    }
    locations.sort_unstable();
    locations
}

#[test]
fn malformed_delimiters_are_isolated_to_their_program() {
    for broken in [
        "#version 120\nvoid main() { gl_Position = texture2D(",
        "#version 330\nvoid main() { float a[] = float[](",
        "#version 330\nlayout(",
        "#version 330\nvoid main() { gl_Position = vec4(0.0]; }",
    ] {
        let translation = ShaderPack::new(PackSource::from_files([
            ("composite.vsh", broken),
            ("composite.fsh", FRAGMENT),
            ("final.vsh", VERTEX),
            ("final.fsh", FRAGMENT),
        ]))
        .translate(&TranslateConfig::default());
        let failed = program(&translation, "composite");
        assert!(!failed.is_ok(), "accepted malformed source: {broken}");
        assert!(failed.all_diagnostics().any(|d| d.location.is_some()));
        reflection(program(&translation, "final"));
    }
}

#[test]
fn resource_overloads_keep_opaque_types_and_nonopaque_signatures() {
    let translation = success(
        VERTEX,
        "#version 330
        uniform sampler2D plane;
        uniform sampler3D volume;
        vec4 read(sampler2D source, vec2 uv) { return texture(source, uv); }
        vec4 read(sampler3D source, vec2 uv) { return texture(source, vec3(uv, 0.5)); }
        vec4 read(sampler3D source, float u) { return texture(source, vec3(u)); }
        out vec4 color;
        void main() { color = read(plane, vec2(0.5)) + read(volume, vec2(0.5)) + read(volume, 0.5); }",
    );
    let reflected = reflection(program(&translation, "composite"));
    assert_eq!(reflected.textures.len(), 2);
    assert!(
        reflected
            .textures
            .iter()
            .any(|t| t.name == "volume" && t.dimension == shaderpack::TextureDimension::D3)
    );
}

#[test]
fn shared_uniforms_resolve_stage_local_types_and_array_constants() {
    let translation = success(
        "#version 330
        const int N = 3;
        struct State { vec2 local; };
        uniform float vertexWeights[N];
        void main() { gl_Position = vec4(vertexWeights[2]); }",
        "#version 330
        const int N = 2;
        struct State { vec4 tint; };
        uniform float weights[N];
        uniform State state;
        out vec4 color;
        void main() { color = state.tint * weights[1]; }",
    );
    let uniforms = reflection(program(&translation, "composite"))
        .uniforms
        .as_ref()
        .unwrap();
    assert!(
        uniforms
            .members
            .iter()
            .any(|m| m.name == "weights" && m.array.unwrap().0 == 2)
    );
    assert!(
        uniforms
            .members
            .iter()
            .any(|m| m.name == "vertexWeights" && m.array.unwrap().0 == 3)
    );
    assert!(uniforms.members.iter().any(|m| m.name == "state.tint"));
}

#[test]
fn conflicting_resolved_uniform_types_are_link_errors() {
    let translation = translate(
        "#version 330\nconst int N = 2; uniform float weights[N]; void main() { gl_Position = vec4(weights[0]); }",
        "#version 330\nconst int N = 3; uniform float weights[N]; out vec4 color; void main() { color = vec4(weights[0]); }",
    );
    let failed = program(&translation, "composite");
    assert!(!failed.is_ok());
    assert!(failed.diagnostics.iter().any(|d| d.phase == Phase::Link));
}

#[test]
fn stage_interface_requires_compatible_vertex_outputs() {
    let translation = translate(
        "#version 330\nout vec2 uv; void main() { uv = vec2(0.0); gl_Position = vec4(0.0); }",
        "#version 330\nin vec3 uv; out vec4 color; void main() { color = vec4(uv, 1.0); }",
    );
    let failed = program(&translation, "composite");
    assert!(!failed.is_ok());
    assert!(failed.diagnostics.iter().any(|d| d.phase == Phase::Link));
}

#[test]
fn fragment_inputs_may_read_fewer_components_than_the_vertex_writes() {
    success(
        "#version 330\nout vec4 uv; void main() { uv = vec4(0.5); gl_Position = vec4(0.0); }",
        "#version 330\nin vec2 uv; out vec4 color; void main() { color = vec4(uv, 0.0, 1.0); }",
    );
    let mismatched = translate(
        "#version 330\nout vec4 uv; void main() { uv = vec4(0.5); gl_Position = vec4(0.0); }",
        "#version 330\nflat in ivec2 uv; out vec4 color; void main() { color = vec4(uv, 0.0, 1.0); }",
    );
    let failed = program(&mismatched, "composite");
    assert!(failed.diagnostics.iter().any(|d| d.phase == Phase::Link));
}

#[test]
fn unfed_fragment_inputs_read_zero() {
    // Iris declares a fragment input no vertex output feeds and initializes it to zero;
    // WebGPU rejects a pipeline whose fragment input has no vertex output.
    for declaration in ["in vec2 missing;", "layout(location=3) in vec2 missing;"] {
        let translation = success(
            VERTEX,
            &format!(
                "#version 330\n{declaration} out vec4 color;
                void main() {{ color = vec4(missing, 0.0, 1.0); }}"
            ),
        );
        let translated = program(&translation, "composite");
        let fragment = translated.stages[1].result.as_ref().unwrap();
        assert!(interface_locations(&fragment.module, false).is_empty());
        assert!(glsl_has(&fragment.glsl, "missing = vec2(0);"));
    }
}

#[test]
fn explicit_varying_locations_link_different_names_and_skip_implicit_slots() {
    let translation = success(
        "#version 450
        out vec2 automaticUv;
        layout(location=0) out vec2 sourceUv;
        void main() { automaticUv = vec2(0.5); sourceUv = vec2(1.0); gl_Position = vec4(0.0); }",
        "#version 450
        layout(location=0) in vec2 uv;
        in vec2 automaticUv;
        out vec4 color;
        void main() { color = vec4(uv + automaticUv, 0.0, 1.0); }",
    );
    let translated = program(&translation, "composite");
    let vertex = &translated.stages[0].result.as_ref().unwrap().module;
    let fragment = &translated.stages[1].result.as_ref().unwrap().module;
    let locations = |module: &naga::Module, output: bool| {
        let mut locations: Vec<_> = interface_locations(module, output)
            .into_iter()
            .map(|(_, location)| location)
            .collect();
        locations.sort_unstable();
        locations
    };
    assert_eq!(locations(vertex, true), locations(fragment, false));
    assert!(locations(vertex, true).contains(&0));
}

#[test]
fn explicit_locations_override_crossed_names() {
    let translation = success(
        "#version 450
        layout(location=0) out vec2 a; layout(location=1) out vec2 b;
        void main() { a = vec2(0.25); b = vec2(0.75); gl_Position = vec4(0.0); }",
        "#version 450
        layout(location=1) in vec2 a; layout(location=0) in vec2 b;
        out vec4 color; void main() { color = vec4(a, b); }",
    );
    let translated = program(&translation, "composite");
    let vertex = &translated.stages[0].result.as_ref().unwrap().module;
    let fragment = &translated.stages[1].result.as_ref().unwrap().module;
    assert_eq!(interface_locations(vertex, true), [("a", 0), ("b", 1)]);
    assert_eq!(interface_locations(fragment, false), [("a", 1), ("b", 0)]);
}

#[test]
fn mixed_locations_link_by_name_and_reserve_explicit_slots() {
    for (vertex_location, fragment_location) in
        [("layout(location=2)", ""), ("", "layout(location=2)")]
    {
        success(
            &format!("#version 450\n{vertex_location} out vec3 sharedUv; out vec2 automaticUv;
            void main() {{ sharedUv = vec3(0.5); automaticUv = vec2(0.25); gl_Position = vec4(0.0); }}"),
            &format!("#version 450\n{fragment_location} in vec3 sharedUv; in vec2 automaticUv;
            out vec4 color; void main() {{ color = vec4(sharedUv + automaticUv.x, 1.0); }}"),
        );
    }
}

#[test]
fn whole_boolean_uniform_arrays_keep_their_value_types() {
    success(
        VERTEX,
        "#version 450
        const int N = 2; uniform bool flags[N]; uniform bvec2 pairs[N];
        bool choose(bool values[2]) { return values[0]; }
        bool choosePair(bvec2 values[2]) { return values[1].x; }
        out vec4 color;
        void main() { color = vec4((choose(flags) && choosePair(pairs)) ? 1.0 : 0.0); }",
    );
}

#[test]
fn partial_boolean_uniform_arrays_keep_their_value_types() {
    success(
        VERTEX,
        "#version 450
        uniform bool flags[2][2]; uniform bool selected; uniform uint row;
        bool choose(bool values[2]) { return values[0]; }
        bool chooseGrid(bool values[2][2]) { return values[1][0]; }
        out vec4 color;
        void main() {
            bool saved[2] = flags[selected ? row : 0u];
            color = vec4((choose(saved) && chooseGrid(flags)) ? 1.0 : 0.0);
        }",
    );
}

#[test]
fn boolean_uniform_arrays_convert_indexed_elements() {
    success(
        VERTEX,
        "#version 330
        uniform bool flags[2]; uniform bvec2 pairs[2]; uniform int selected;
        out vec4 color;
        void main() { color = vec4((flags[selected] && pairs[1].x) ? 1.0 : 0.0); }",
    );
}

#[test]
fn boolean_uniform_references_respect_parameters_and_nested_locals() {
    success(
        VERTEX,
        "#version 330
        uniform bool enabled;
        bool choose(bool enabled) { return enabled; }
        out vec4 color;
        void main() {
            bool result = enabled;
            { bool enabled = true; result = result || enabled; }
            for (int enabled = 0; enabled < 2; ++enabled) { result = !result; }
            color = vec4((choose(result) && enabled) ? 1.0 : 0.0);
        }",
    );
}

#[test]
fn legacy_reserved_fields_keep_matching_declarations_and_member_accesses() {
    success(
        VERTEX,
        "#version 120
        struct State { float sample; vec2 texture; };
        void main() { State state; state.sample = 0.5; state.texture = vec2(0.5); gl_FragColor = vec4(state.sample + state.texture.x); }",
    );
}

#[test]
fn increment_operands_keep_their_original_bindings() {
    success(
        VERTEX,
        "#version 330
        const float a = 1.0;
        out vec4 color;
        void main() { float b = 2.0; float c = a + ++b; float d = a - --b; color = vec4(c, d, b, 1.0); }",
    );
}

#[test]
fn boolean_array_indices_rewrite_nested_uniform_references() {
    success(
        VERTEX,
        "#version 330
        uniform bool flags[2][2]; uniform bool selected;
        out vec4 color;
        void main() { color = vec4(flags[selected ? 0 : 1][flags[0][0] ? 1 : 0] ? 1.0 : 0.0); }",
    );
}

#[test]
fn local_initializers_read_outer_bindings_before_shadowing() {
    success(
        VERTEX,
        "#version 330
        uniform bool enabled;
        out vec4 color;
        void main() {
            float result = 0.0;
            { int enabled = enabled ? 1 : 0; result = float(enabled); }
            { struct State { float value; }; State enabled = State(0.5); result += enabled.value; }
            color = vec4(result + (enabled ? 1.0 : 0.0));
        }",
    );
}

#[test]
fn imported_uniform_names_do_not_capture_other_stage_globals() {
    success(
        "#version 330\nconst float exposure = 0.5; void main() { gl_Position = vec4(exposure); }",
        "#version 330\nuniform float exposure; out vec4 color; void main() { color = vec4(exposure); }",
    );
}

#[test]
fn equivalent_resolved_uniform_types_share_one_layout() {
    success(
        "#version 330\nconst int N = 2; uniform float weights[N]; void main() { gl_Position = vec4(weights[0]); }",
        "#version 330\nuniform float weights[2]; out vec4 color; void main() { color = vec4(weights[1]); }",
    );
}

#[test]
fn shared_uniform_structs_keep_stage_local_nominal_types() {
    success(
        "#version 330\nuniform float weight; void main() { gl_Position = vec4(weight); }",
        "#version 330
        struct A { vec4 tint; }; struct B { vec4 tint; };
        uniform A first; uniform B second;
        vec4 readA(A state) { return state.tint; }
        vec4 readB(B state) { return state.tint; }
        out vec4 color;
        void main() { color = readA(first) + readB(second); }",
    );
}

#[test]
fn same_size_resource_blocks_require_matching_member_types() {
    for storage in ["uniform", "buffer"] {
        let layout = if storage == "uniform" {
            "std140"
        } else {
            "std430"
        };
        let translation = translate(
            &format!(
                "#version 430\nlayout({layout}, binding=2) {storage} Data {{ vec4 tint; }} data; void main() {{ gl_Position = data.tint; }}"
            ),
            &format!(
                "#version 430\nlayout({layout}, binding=2) {storage} Data {{ ivec4 tint; }} data; out vec4 color; void main() {{ color = vec4(data.tint); }}"
            ),
        );
        let failed = program(&translation, "composite");
        assert!(!failed.is_ok());
        assert!(failed.diagnostics.iter().any(|d| d.phase == Phase::Link));
    }
}

#[test]
fn resource_helpers_preserve_value_overloads_and_nested_resource_calls() {
    success(
        VERTEX,
        "#version 330
        uniform sampler2D source;
        vec4 lookup(float x) { return vec4(x); }
        vec4 lookup(sampler2D s, vec2 uv) { return texture(s, uv); }
        vec4 nested(sampler2D s, vec2 uv) { return lookup(s, uv); }
        out vec4 color;
        void main() { color = lookup(0.5) + nested(source, vec2(0.5)); }",
    );
}

#[test]
fn specialized_resources_cannot_be_captured_by_helper_locals() {
    success(
        VERTEX,
        "#version 330
        uniform sampler2D source;
        vec4 read(sampler2D s, vec2 uv) { return texture(s, uv); }
        vec4 helper(sampler2D s, vec2 uv) {
            float source = 1.0;
            vec4 result = read(s, uv) * source;
            { float s = 2.0; result *= s; }
            return result + texture(s, uv);
        }
        out vec4 color;
        void main() { color = helper(source, vec2(0.5)); }",
    );
}

#[test]
fn recursive_varying_types_return_diagnostics() {
    let translation = translate(
        "#version 330\nstruct Node { Node child; }; out Node value; void main() { gl_Position = vec4(0.0); }",
        FRAGMENT,
    );
    assert!(!program(&translation, "composite").is_ok());
    assert!(
        program(&translation, "composite")
            .all_diagnostics()
            .next()
            .is_some()
    );
}

#[test]
fn oversized_interface_declarations_return_diagnostics_without_overflow() {
    for declaration in [
        "in mat4 values[4294967295];",
        "out vec4 values[4294967295];",
        "layout(location=4294967295) out vec4 value;",
    ] {
        let source =
            format!("#version 330\n{declaration}\nvoid main() {{ gl_Position = vec4(0.0); }}");
        let translation = translate(&source, FRAGMENT);
        let failed = program(&translation, "composite");
        assert!(
            !failed.is_ok(),
            "accepted oversized interface: {declaration}"
        );
        assert!(failed.all_diagnostics().next().is_some());
    }
}

#[test]
fn truncated_call_and_declaration_prefixes_never_abort_sibling_programs() {
    for source in [
        "#version 120\nvoid main() { gl_Position = texture2D(tex, vec2(0.0)); }",
        "#version 330\nlayout(location=0) out vec2 uv; void main() { gl_Position = vec4(0.0); }",
        "#version 330\nconst float values[] = float[](1.0, 2.0); void main() {}",
    ] {
        for (end, _) in source.char_indices() {
            let translation = ShaderPack::new(PackSource::from_files([
                ("composite.vsh", &source[..end]),
                ("composite.fsh", FRAGMENT),
                ("final.vsh", VERTEX),
                ("final.fsh", FRAGMENT),
            ]))
            .translate(&TranslateConfig::default());
            reflection(program(&translation, "final"));
        }
    }
}

#[test]
fn specialized_helpers_follow_the_functions_and_globals_they_use() {
    // BSL's ambient occlusion: a helper taking a depth sampler calls `GetLinearDepth`,
    // defined earlier in the stage, and reads a private global.
    success(
        VERTEX,
        "#version 330
        uniform sampler2D depthtex0;
        float gain = 2.0;
        float linearDepth(float depth) { return depth * gain; }
        float sampleDepth(sampler2D depthtex, vec2 uv) { return linearDepth(texture(depthtex, uv).r); }
        float outer(sampler2D depthtex);
        out vec4 color;
        void main() { color = vec4(outer(depthtex0)); }
        float outer(sampler2D depthtex) { return sampleDepth(depthtex, vec2(0.5)); }",
    );
}

#[test]
fn forward_declared_specialized_helpers_keep_their_definition_position() {
    // Solas's raytracer: a helper declared early is defined after `scaled`, which it calls.
    success(
        VERTEX,
        "#version 330
        uniform sampler2D depthtex0;
        float tapDepth(sampler2D depthtex);
        float scaled(float depth) { return depth * 0.5; }
        float tapDepth(sampler2D depthtex) { return scaled(texture(depthtex, vec2(0.5)).r); }
        out vec4 color;
        void main() { color = vec4(tapDepth(depthtex0)); }",
    );
}
