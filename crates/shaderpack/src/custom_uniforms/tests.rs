use std::collections::HashMap;
use std::f32::consts::PI;

use super::{CustomUniformError, CustomUniforms, UniformValue};
use crate::{CustomUniform, CustomUniformKind, Expr};

/// Builds definitions from `kind.type.name = expression` lines, numbered from 1.
fn definitions(lines: &[&str]) -> Vec<CustomUniform> {
    lines
        .iter()
        .zip(1..)
        .map(|(line, number)| {
            let (key, expr) = line.split_once('=').expect("definition has `=`");
            let mut parts = key.trim().splitn(3, '.');
            let kind = match parts.next() {
                Some("uniform") => CustomUniformKind::Uniform,
                Some("variable") => CustomUniformKind::Variable,
                other => panic!("bad kind {other:?}"),
            };
            let ty = parts.next().expect("type").to_owned();
            let name = parts.next().expect("name").to_owned();
            CustomUniform {
                kind,
                ty,
                name,
                expr: Expr::parse(expr).expect("test expression parses"),
                line: number,
            }
        })
        .collect()
}

fn compile(lines: &[&str]) -> (CustomUniforms, Vec<CustomUniformError>) {
    CustomUniforms::new(&definitions(lines))
}

/// Compiles `lines`, asserting every definition compiles.
fn compile_ok(lines: &[&str]) -> CustomUniforms {
    let (uniforms, errors) = compile(lines);
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    uniforms
}

/// A built-in lookup answering from `pairs`.
fn builtins(pairs: &[(&str, UniformValue)]) -> impl Fn(&str) -> Option<UniformValue> + use<> {
    let map: HashMap<String, UniformValue> = pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), *value))
        .collect();
    move |name: &str| map.get(name).copied()
}

fn no_builtins(_: &str) -> Option<UniformValue> {
    None
}

fn get(results: &[(String, UniformValue)], name: &str) -> UniformValue {
    results
        .iter()
        .find(|(result, _)| result == name)
        .map(|(_, value)| *value)
        .unwrap_or_else(|| panic!("no result `{name}` in {results:?}"))
}

fn float(results: &[(String, UniformValue)], name: &str) -> f32 {
    match get(results, name) {
        UniformValue::Float(value) => value,
        other => panic!("`{name}` is {other:?}, expected a float"),
    }
}

fn assert_close(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= 1e-4,
        "{what}: expected {expected}, got {actual}"
    );
}

#[test]
fn scalar_results_convert_to_their_declared_types() {
    let mut uniforms = compile_ok(&[
        "uniform.float.sum = 1 + 2 * 3 - 8 / 4",
        "uniform.int.half = 7 / 2",
        "uniform.int.negative_half = -7 / 2",
        "uniform.int.huge = 1e12",
        "uniform.bool.fraction = 0.25",
        "uniform.bool.zero = 0",
        "uniform.float.truth = true",
        "uniform.float.remainder = -7 % 3",
        "uniform.bool.logic = !(2 > 3) && (1 != 1 || 2 <= 2)",
    ]);
    let results = uniforms.evaluate(&no_builtins, 0.0);
    assert_eq!(get(results, "sum"), UniformValue::Float(5.0));
    assert_eq!(get(results, "half"), UniformValue::Int(3));
    assert_eq!(get(results, "negative_half"), UniformValue::Int(-3));
    assert_eq!(get(results, "huge"), UniformValue::Int(i32::MAX));
    assert_eq!(get(results, "fraction"), UniformValue::Bool(true));
    assert_eq!(get(results, "zero"), UniformValue::Bool(false));
    assert_eq!(get(results, "truth"), UniformValue::Float(1.0));
    assert_eq!(get(results, "remainder"), UniformValue::Float(-1.0));
    assert_eq!(get(results, "logic"), UniformValue::Bool(true));
}

#[test]
fn builtin_components_matrix_elements_and_unknown_names_resolve() {
    let mut uniforms = compile_ok(&[
        "uniform.float.height = cameraPosition.y",
        "uniform.float.green = cameraPosition.g + cameraPosition.t + cameraPosition.1",
        "uniform.float.element = gbufferModelView.3.1 * 2",
        "uniform.float.unknown = notAUniform + notAUniform.x + 1",
        "uniform.int.water = isEyeInWater",
        "uniform.float.past_end = cameraPosition.w + 5",
    ]);
    assert_eq!(
        uniforms.inputs().collect::<Vec<_>>(),
        [
            "cameraPosition",
            "gbufferModelView.3.1",
            "notAUniform",
            "isEyeInWater"
        ]
    );
    let lookup = builtins(&[
        ("cameraPosition", UniformValue::Vec3([1.0, 64.5, -3.0])),
        ("gbufferModelView.3.1", UniformValue::Float(-0.25)),
        ("isEyeInWater", UniformValue::Int(2)),
    ]);
    let results = uniforms.evaluate(&lookup, 0.0);
    assert_eq!(float(results, "height"), 64.5);
    assert_eq!(float(results, "green"), 64.5 * 3.0);
    assert_eq!(float(results, "element"), -0.5);
    assert_eq!(float(results, "unknown"), 1.0);
    assert_eq!(get(results, "water"), UniformValue::Int(2));
    // `.w` of a vec3 is a run-time type error, so the uniform falls back to zero.
    assert_eq!(float(results, "past_end"), 0.0);
}

#[test]
fn vectors_operate_per_component_with_scalar_broadcast() {
    let mut uniforms = compile_ok(&[
        "variable.vec3.base = vec3(1, 2, 3) * 2 + 1",
        "uniform.vec3.scaled = base",
        "uniform.float.middle = base.y + base.b",
        "uniform.vec2.product = vec2(1, 2) * vec2(3, 4)",
        "uniform.vec2.absolute = abs(vec2(-1.5, 2))",
        "uniform.vec3.clamped = clamp(vec3(-1, 0.5, 2), 0, 1)",
        "uniform.vec2.smallest = min(vec2(1, 5), vec2(3, 2), vec2(4, 4))",
        "uniform.vec4.mixed = mix(vec4(0, 0, 0, 0), vec4(4, 8, 12, 16), 0.25)",
        "uniform.bool.same = vec2(1, 2) == vec2(1, 2) && vec2(1, 2) != vec2(2, 1)",
        "uniform.vec3.shifted = sunDirection + 1",
    ]);
    let lookup = builtins(&[("sunDirection", UniformValue::Vec3([0.0, -1.0, 0.5]))]);
    let results = uniforms.evaluate(&lookup, 0.0);
    assert_eq!(get(results, "scaled"), UniformValue::Vec3([3.0, 5.0, 7.0]));
    assert_eq!(float(results, "middle"), 12.0);
    assert_eq!(get(results, "product"), UniformValue::Vec2([3.0, 8.0]));
    assert_eq!(get(results, "absolute"), UniformValue::Vec2([1.5, 2.0]));
    assert_eq!(get(results, "clamped"), UniformValue::Vec3([0.0, 0.5, 1.0]));
    assert_eq!(get(results, "smallest"), UniformValue::Vec2([1.0, 2.0]));
    assert_eq!(
        get(results, "mixed"),
        UniformValue::Vec4([1.0, 2.0, 3.0, 4.0])
    );
    assert_eq!(get(results, "same"), UniformValue::Bool(true));
    assert_eq!(get(results, "shifted"), UniformValue::Vec3([1.0, 0.0, 1.5]));
}

#[test]
fn if_chains_pick_the_first_true_branch() {
    let mut uniforms = compile_ok(&[
        "uniform.float.brightness = if(phase == 0, 1.0, phase == 1, 0.75, phase <= 3, 0.5, 0.1)",
        "uniform.vec2.pick = if(phase > 2, vec2(1, 0), vec2(0, 1))",
    ]);
    for (phase, brightness, pick) in [
        (0, 1.0, [0.0, 1.0]),
        (1, 0.75, [0.0, 1.0]),
        (3, 0.5, [1.0, 0.0]),
        (6, 0.1, [1.0, 0.0]),
    ] {
        let lookup = builtins(&[("phase", UniformValue::Int(phase))]);
        let results = uniforms.evaluate(&lookup, 0.0);
        assert_eq!(float(results, "brightness"), brightness, "phase {phase}");
        assert_eq!(
            get(results, "pick"),
            UniformValue::Vec2(pick),
            "phase {phase}"
        );
    }
}

#[test]
fn functions_match_optifine_and_iris_definitions() {
    let cases: &[(&str, f32)] = &[
        ("sin(pi / 2)", 1.0),
        ("cos(0)", 1.0),
        ("tan(0)", 0.0),
        ("asin(1)", PI / 2.0),
        ("acos(1)", 0.0),
        ("atan(1)", PI / 4.0),
        ("atan2(1, -1)", 3.0 * PI / 4.0),
        ("atan(1, -1)", 3.0 * PI / 4.0),
        ("torad(180)", PI),
        ("radians(90)", PI / 2.0),
        ("todeg(pi)", 180.0),
        ("degrees(pi / 4)", 45.0),
        ("exp(0)", 1.0),
        ("exp2(3)", 8.0),
        ("exp10(2)", 100.0),
        ("log(1)", 0.0),
        ("log(2, 8)", 3.0),
        ("log2(32)", 5.0),
        ("log10(1000)", 3.0),
        ("pow(2, 10)", 1024.0),
        ("sqrt(16)", 4.0),
        ("abs(-2.5)", 2.5),
        ("signum(-3)", -1.0),
        ("sign(4)", 1.0),
        ("sign(0)", 0.0),
        ("floor(-1.5)", -2.0),
        ("ceil(1.2)", 2.0),
        ("frac(-1.25)", 0.75),
        ("round(2.5)", 3.0),
        ("round(-2.5)", -2.0),
        ("rint(2.5)", 2.0),
        ("rint(3.5)", 4.0),
        ("fmod(-1, 3)", 2.0),
        ("fmod(7, 3)", 1.0),
        ("min(4, 1, 3)", 1.0),
        ("max(4, 9, 3)", 9.0),
        ("clamp(5, 0, 1)", 1.0),
        ("clamp(-5, 0, 1)", 0.0),
        ("mix(0, 10, 0.25)", 2.5),
        ("edge(0.5, 0.4)", 0.0),
        ("edge(0.5, 0.5)", 1.0),
        ("if(between(5, 1, 10), 1, 0)", 1.0),
        ("if(between(11, 1, 10), 1, 0)", 0.0),
        ("if(equals(1.0, 1.05, 0.1), 1, 0)", 1.0),
        ("if(equals(1.0, 1.5, 0.1), 1, 0)", 0.0),
        ("if(in(3, 1, 2, 3), 1, 0)", 1.0),
        ("if(in(4, 1, 2, 3), 1, 0)", 0.0),
    ];
    let lines: Vec<String> = cases
        .iter()
        .enumerate()
        .map(|(index, (expr, _))| format!("uniform.float.case{index} = {expr}"))
        .collect();
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut uniforms = compile_ok(&lines);
    let results = uniforms.evaluate(&no_builtins, 0.0);
    for (index, (expr, expected)) in cases.iter().enumerate() {
        assert_close(float(results, &format!("case{index}")), *expected, expr);
    }
}

#[test]
fn smooth_converges_on_a_step_over_its_fade_time() {
    let mut uniforms = compile_ok(&["uniform.float.level = smooth(target, 1.0, 1.0)"]);
    let at = |value: f32| builtins(&[("target", UniformValue::Float(value))]);
    // The first evaluation starts at the current value.
    assert_eq!(float(uniforms.evaluate(&at(0.0), 0.1), "level"), 0.0);
    let mut previous = 0.0;
    let mut history = Vec::new();
    for _ in 0..10 {
        let level = float(uniforms.evaluate(&at(1.0), 0.1), "level");
        assert!(level > previous && level < 1.0, "{level} after {previous}");
        previous = level;
        history.push(level);
    }
    assert!(
        (0.3..0.45).contains(&history[0]),
        "first step {}",
        history[0]
    );
    assert!(history[4] < 0.95, "half way {}", history[4]);
    assert!(previous > 0.98, "after the fade time {previous}");
    // No elapsed time holds the value.
    assert_eq!(float(uniforms.evaluate(&at(0.0), 0.0), "level"), previous);
}

#[test]
fn smooth_uses_separate_rise_and_fall_times() {
    let mut uniforms = compile_ok(&[
        "uniform.float.instant_fall = smooth(target, 2.0, 0.0)",
        "uniform.float.default_fade = smooth(target)",
    ]);
    let at = |value: f32| builtins(&[("target", UniformValue::Float(value))]);
    uniforms.evaluate(&at(0.0), 0.05);
    let results = uniforms.evaluate(&at(1.0), 0.05);
    let rising = float(results, "instant_fall");
    assert!(rising > 0.0 && rising < 0.2, "rising {rising}");
    let default_rise = float(results, "default_fade");
    assert!(
        default_rise > rising,
        "a 1 s fade rises faster than a 2 s one"
    );
    let results = uniforms.evaluate(&at(0.0), 0.05);
    assert_eq!(float(results, "instant_fall"), 0.0);
    assert!(float(results, "default_fade") > 0.0);
}

#[test]
fn smooth_ids_share_state_and_skipped_calls_accumulate_time() {
    let mut uniforms = compile_ok(&[
        "uniform.float.first = smooth(7, target, 1, 1)",
        "uniform.float.second = smooth(7, target * 3, 1, 1)",
        "uniform.float.gated = if(open, smooth(target, 1, 1), -1)",
    ]);
    let at = |value: f32, open: bool| {
        builtins(&[
            ("target", UniformValue::Float(value)),
            ("open", UniformValue::Bool(open)),
        ])
    };
    uniforms.evaluate(&at(0.0, true), 0.1);
    let results = uniforms.evaluate(&at(1.0, true), 0.1);
    // The second call with id 7 runs in the same frame, so no time has passed for it.
    assert_eq!(float(results, "first"), float(results, "second"));
    assert!(float(results, "gated") < 0.5);
    for _ in 0..20 {
        assert_eq!(
            float(uniforms.evaluate(&at(1.0, false), 0.1), "gated"),
            -1.0
        );
    }
    // Two seconds passed since the gated call last ran: longer than its fade time.
    assert_eq!(float(uniforms.evaluate(&at(1.0, true), 0.1), "gated"), 1.0);
}

#[test]
fn smooth_filters_vectors_per_component() {
    let mut uniforms = compile_ok(&["uniform.vec2.pair = smooth(vec2(target, -target), 1, 1)"]);
    let at = |value: f32| builtins(&[("target", UniformValue::Float(value))]);
    uniforms.evaluate(&at(0.0), 0.1);
    let UniformValue::Vec2([x, y]) = get(uniforms.evaluate(&at(2.0), 0.1), "pair") else {
        panic!("pair is not a vec2");
    };
    assert!(x > 0.0 && x < 2.0, "x {x}");
    assert_close(y, -x, "components move symmetrically");
}

#[test]
fn variables_feed_uniforms_without_being_reported() {
    let mut uniforms = compile_ok(&[
        "variable.int.whole = 2.75",
        "variable.bool.flag = 3",
        "uniform.float.doubled = whole * 2",
        "uniform.float.flagged = if(flag, 10, 20)",
        "variable.vec3.direction = vec3(0, height, 0)",
        "uniform.float.up = direction.y",
    ]);
    let lookup = builtins(&[("height", UniformValue::Float(4.0))]);
    let results = uniforms.evaluate(&lookup, 0.0);
    let names: Vec<&str> = results.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["doubled", "flagged", "up"]);
    assert_eq!(float(results, "doubled"), 4.0);
    assert_eq!(float(results, "flagged"), 10.0);
    assert_eq!(float(results, "up"), 4.0);
}

#[test]
fn invalid_definitions_are_dropped_with_located_errors() {
    let (mut uniforms, errors) = compile(&[
        "uniform.float.early = later + 1",
        "variable.float.later = 2",
        "uniform.float.selfish = selfish + 1",
        "uniform.float.mystery = frobnicate(1)",
        "uniform.float.short = clamp(1, 2)",
        "uniform.mat4.matrix = 1",
        "uniform.float.dependent = early * 2",
        "uniform.float.later = 3",
        "uniform.float.flat = vec3(1, 2, 3)",
        "uniform.vec3.mixed = vec2(1, 2) + vec3(1, 2, 3)",
        "uniform.float.scalar_part = later.x",
        "variable.vec2.pair = vec2(1, 2)",
        "uniform.float.past_end = pair.z",
        "uniform.bool.vector_order = pair < 1",
        "uniform.float.odd_if = if(true, 1)",
        "uniform.float.branches = if(true, 1, vec2(1, 1))",
        "uniform.float.macro = if(defined FOO, 1, 0)",
        "uniform.float.fine = later + pair.y + onlyFine",
        "uniform.float.dropped_input = onlyDropped + frobnicate(2)",
    ]);
    let failed: Vec<(u32, &str)> = errors
        .iter()
        .map(|error| (error.line, error.name.as_str()))
        .collect();
    assert_eq!(
        failed,
        [
            (1, "early"),
            (3, "selfish"),
            (4, "mystery"),
            (5, "short"),
            (6, "matrix"),
            (7, "dependent"),
            (8, "later"),
            (9, "flat"),
            (10, "mixed"),
            (11, "scalar_part"),
            (13, "past_end"),
            (14, "vector_order"),
            (15, "odd_if"),
            (16, "branches"),
            (17, "macro"),
            (19, "dropped_input"),
        ]
    );
    assert!(
        errors[0].message.contains("line 2"),
        "{}",
        errors[0].message
    );
    assert_eq!(uniforms.inputs().collect::<Vec<_>>(), ["onlyFine"]);
    let lookup = builtins(&[("onlyFine", UniformValue::Float(0.5))]);
    let results = uniforms.evaluate(&lookup, 0.0);
    assert_eq!(results.len(), 1);
    assert_eq!(float(results, "fine"), 4.5);
}

#[test]
fn builtins_of_unexpected_type_yield_zero() {
    let mut uniforms = compile_ok(&[
        "uniform.float.scalar = sunPosition",
        "uniform.vec2.wrong_length = sunPosition",
        "uniform.vec3.right_length = sunPosition",
        "uniform.float.not_a_matrix = gbufferProjection.0.0",
    ]);
    let lookup = builtins(&[
        ("sunPosition", UniformValue::Vec3([1.0, 2.0, 3.0])),
        ("gbufferProjection.0.0", UniformValue::Vec2([1.0, 1.0])),
    ]);
    let results = uniforms.evaluate(&lookup, 0.0);
    assert_eq!(get(results, "scalar"), UniformValue::Float(0.0));
    assert_eq!(get(results, "wrong_length"), UniformValue::Vec2([0.0; 2]));
    assert_eq!(
        get(results, "right_length"),
        UniformValue::Vec3([1.0, 2.0, 3.0])
    );
    assert_eq!(get(results, "not_a_matrix"), UniformValue::Float(0.0));
}

#[test]
fn random_is_reproducible_and_within_range() {
    let lines = [
        "uniform.float.unit = random()",
        "uniform.float.ranged = random(2, 4)",
        "uniform.int.dice = randomInt(1, 7)",
        "uniform.int.any = randomInt()",
    ];
    let mut first = compile_ok(&lines);
    let mut second = compile_ok(&lines);
    let mut seen_unit = Vec::new();
    for _ in 0..50 {
        let a = first.evaluate(&no_builtins, 0.016).to_vec();
        let b = second.evaluate(&no_builtins, 0.016).to_vec();
        assert_eq!(a, b);
        let unit = float(&a, "unit");
        assert!((0.0..1.0).contains(&unit), "unit {unit}");
        let ranged = float(&a, "ranged");
        assert!((2.0..4.0).contains(&ranged), "ranged {ranged}");
        let UniformValue::Int(dice) = get(&a, "dice") else {
            panic!("dice is not an int");
        };
        assert!((1..7).contains(&dice), "dice {dice}");
        seen_unit.push(unit);
    }
    seen_unit.dedup();
    assert!(seen_unit.len() > 1, "random() changes between evaluations");
}

#[test]
fn hostile_frame_times_and_values_never_panic() {
    let mut uniforms = compile_ok(&[
        "uniform.float.held = smooth(target, 1, 1)",
        "uniform.int.nan = 0 / 0",
        "uniform.float.infinite = 1 / 0",
        "uniform.int.rounded = round(1 / 0)",
    ]);
    let at = |value: f32| builtins(&[("target", UniformValue::Float(value))]);
    uniforms.evaluate(&at(1.0), 0.1);
    for frame_time in [f32::NAN, -1.0, f32::INFINITY] {
        let results = uniforms.evaluate(&at(5.0), frame_time);
        assert_eq!(float(results, "held"), 1.0, "frame time {frame_time}");
    }
    let results = uniforms.evaluate(&at(5.0), 0.1);
    assert_eq!(get(results, "nan"), UniformValue::Int(0));
    assert_eq!(float(results, "infinite"), f32::INFINITY);
    assert_eq!(get(results, "rounded"), UniformValue::Int(i32::MAX));
}
