//! Rewrites inside function bodies and initializers: separate texture/sampler pairs and
//! integer-backed bool uniforms.

use super::TransformError;
use super::declare::integer_bool;
use super::items::{arr_len, mat_cols, matching, sampler_info};
use crate::reflection::{SHADOW_SAMPLER_NAME, SamplerClass};
use crate::token::{Tok, TokKind, id, int, p};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Sampler name → (combined sampler type to construct, compares depth, filtering class).
pub(super) type SamplerKinds = HashMap<String, (String, bool, SamplerClass)>;

/// What body rewriting needs to know about the stage's globals.
pub(super) struct BodyContext<'a> {
    pub samplers: &'a SamplerKinds,
    /// Bool uniform name → constructor (`bool` or `bvecN`) of its integer storage.
    pub bools: &'a HashMap<String, String>,
    pub user_funcs: &'a HashSet<String>,
}

/// Shared samplers the stage ended up referencing.
#[derive(Default)]
pub(super) struct SamplerUse {
    pub filtering: BTreeSet<SamplerClass>,
    pub comparison: bool,
}

/// Function parameters of combined sampler types become separate texture types, read with
/// the shared repeat/linear sampler: calls with global textures were specialized already.
/// Other parameters named like a global sampler shadow it. `const in` loses the `const`,
/// which naga's parser rejects in that order (SEUS Renewed); the copy is read-only anyway.
pub(super) fn rewrite_params(header: &[Tok], samplers: &mut SamplerKinds) -> Vec<Tok> {
    let mut out: Vec<Tok> = vec![];
    for (i, t) in header.iter().enumerate() {
        if t.ident() == Some("const") && header.get(i + 1).is_some_and(|x| x.ident() == Some("in"))
        {
            continue;
        }
        if let Some(n) = t.ident()
            && let Some((tex, shadow)) = sampler_info(n)
            && let Some(Tok {
                kind: TokKind::Ident(pname),
                ..
            }) = header.get(i + 1)
        {
            samplers.insert(
                pname.clone(),
                (n.to_string(), shadow, SamplerClass::RepeatLinear),
            );
            if out.last().is_some_and(|l| l.ident() == Some("in")) {
                out.pop();
            }
            out.push(id(tex).at(t.line));
            continue;
        }
        if let Some(n) = t.ident()
            && i > 0
            && header[i - 1]
                .ident()
                .is_some_and(|ty| sampler_info(ty).is_none())
            && header
                .get(i + 1)
                .is_some_and(|x| x.is(",") || x.is(")") || x.is("["))
        {
            samplers.remove(n);
        }
        out.push(t.clone());
    }
    out
}

/// Pairs every texture use with its sampler (`sampler2D(colortex0, iris_samplerClampLinear)`)
/// unless it is passed straight to a user function, and reads bool uniforms as bools. Locals
/// that shadow a sampler or bool global (Complementary's `float specular`) are left alone in
/// their block from the end of their declarator on, as GLSL scopes them: SEUS's
/// `vec4 tex = texture2D(tex, …)` still samples the global in the initializer.
pub(super) fn rewrite_body(toks: &[Tok], cx: &BodyContext<'_>, used: &mut SamplerUse) -> Vec<Tok> {
    let mut out = vec![];
    let mut calls: Vec<Option<String>> = vec![];
    // Names declared by each open block.
    let mut scopes: Vec<Vec<&str>> = vec![vec![]];
    // A shadowing declaration whose declarator is still open, with its paren depth.
    let mut pending: Option<(&str, usize)> = None;
    // Paren depths of open writes to integer-backed bools, closed at their expression's end.
    let mut writes: Vec<usize> = vec![];
    let mut skip = false;
    for (i, t) in toks.iter().enumerate() {
        if skip {
            skip = false;
            continue;
        }
        if writes.last() == Some(&calls.len()) && (t.is(";") || t.is(",") || t.is(")")) {
            writes.pop();
            out.push(p(")").at(t.line));
        }
        if t.is("(") {
            calls.push(
                i.checked_sub(1)
                    .and_then(|j| toks[j].ident())
                    .map(str::to_string),
            );
        } else if t.is(")") {
            calls.pop();
        } else if t.is("{") {
            scopes.push(vec![]);
        } else if t.is("}") && scopes.len() > 1 {
            scopes.pop();
        } else if (t.is(";") || t.is(","))
            && let Some((name, depth)) = pending
            && depth == calls.len()
        {
            pending = None;
            if let Some(scope) = scopes.last_mut() {
                scope.push(name);
            }
        }
        let TokKind::Ident(n) = &t.kind else {
            out.push(t.clone());
            continue;
        };
        if i > 0 && toks[i - 1].is(".") {
            out.push(t.clone());
            continue;
        }
        if (cx.samplers.contains_key(n) || cx.bools.contains_key(n)) && declares_local(toks, i) {
            pending = Some((n, calls.len()));
            out.push(t.clone());
            continue;
        }
        if scopes.iter().any(|s| s.contains(&n.as_str())) {
            out.push(t.clone());
            continue;
        }
        let line = t.line;
        if let Some((ctor, shadow, class)) = cx.samplers.get(n) {
            let prev_ok = i > 0 && (toks[i - 1].is("(") || toks[i - 1].is(","));
            let next_ok = toks.get(i + 1).is_some_and(|x| x.is(")") || x.is(","));
            let in_user_call = calls
                .last()
                .and_then(|c| c.as_ref())
                .is_some_and(|c| cx.user_funcs.contains(c));
            if prev_ok && next_ok && in_user_call {
                out.push(t.clone());
            } else {
                let sampler = if *shadow {
                    used.comparison = true;
                    SHADOW_SAMPLER_NAME
                } else {
                    used.filtering.insert(*class);
                    class.glsl_name()
                };
                out.extend([
                    id(ctor).at(line),
                    p("(").at(line),
                    t.clone(),
                    p(",").at(line),
                    id(sampler).at(line),
                    p(")").at(line),
                ]);
            }
            continue;
        }
        if let Some(ctor) = cx.bools.get(n) {
            if toks.get(i + 1).is_some_and(|x| x.is("=")) {
                // A block bool written (`noSimOngoing = true;`) stores its integer.
                out.extend([
                    t.clone(),
                    p("=").at(line),
                    id(&integer_bool(ctor)).at(line),
                    p("(").at(line),
                ]);
                writes.push(calls.len());
                skip = true;
            } else if ctor == "bool" {
                out.extend([
                    p("(").at(line),
                    t.clone(),
                    p("!=").at(line),
                    int(0).at(line),
                    p(")").at(line),
                ]);
            } else {
                out.extend([
                    id(ctor).at(line),
                    p("(").at(line),
                    t.clone(),
                    p(")").at(line),
                ]);
            }
            continue;
        }
        out.push(t.clone());
    }
    out
}

/// The return type of a function header (`vec3 f(…)` → `vec3`), empty if there is none.
pub(super) fn return_type(header: &[Tok]) -> String {
    header
        .iter()
        .position(|t| t.is("("))
        .and_then(|lp| lp.checked_sub(2))
        .and_then(|i| header[i].ident())
        .unwrap_or_default()
        .to_owned()
}

/// GLSL converts a returned value to the function's type (Eclipse returns `0` from a
/// `uint` function); naga's parser does not, so every return of a numeric scalar or vector
/// function constructs the type, which is free when the value already has it.
pub(super) fn convert_returns(body: Vec<Tok>, ty: &str) -> Vec<Tok> {
    let numeric = matches!(ty, "float" | "int" | "uint")
        || ["vec", "ivec", "uvec"].iter().any(|v| {
            ty.strip_prefix(v)
                .is_some_and(|n| matches!(n, "2" | "3" | "4"))
        });
    if !numeric {
        return body;
    }
    let mut out = Vec::with_capacity(body.len());
    let mut open = false;
    for (i, t) in body.iter().enumerate() {
        if open && t.is(";") {
            out.push(p(")").at(t.line));
            open = false;
        }
        out.push(t.clone());
        if t.ident() == Some("return") && body.get(i + 1).is_some_and(|n| !n.is(";")) {
            out.extend([id(ty).at(t.line), p("(").at(t.line)]);
            open = true;
        }
    }
    out
}

/// Whether the identifier at `i` is the name in a local declaration (`float specular = …`,
/// `vec3 specular;`, `int n[2]`): a type name before it and a declarator end after it.
fn declares_local(toks: &[Tok], i: usize) -> bool {
    let ty = i.checked_sub(1).and_then(|j| toks[j].ident());
    let ends = toks
        .get(i + 1)
        .is_some_and(|x| x.is("=") || x.is(";") || x.is(",") || x.is("["));
    ends && ty.is_some_and(|ty| !matches!(ty, "return" | "else" | "do" | "case"))
}

/// Struct name → members as (type, name, array tokens).
pub(super) type Structs = HashMap<String, Vec<(String, String, Vec<Tok>)>>;

/// Flattens a matrix, array or struct value into (access path, leaf type) pairs.
pub(super) fn flatten_leaves(
    path: String,
    ty: &str,
    arr: &[Tok],
    structs: &Structs,
    line: u32,
    out: &mut Vec<(String, String)>,
) -> Result<(), TransformError> {
    if !arr.is_empty() {
        let close = matching(arr, 0, "[", "]");
        let n = arr_len(&arr[..=close]).ok_or_else(|| {
            TransformError::new(
                format!("varying `{path}` has a non-literal array size"),
                line,
            )
        })?;
        for a in 0..n {
            flatten_leaves(
                format!("{path}[{a}]"),
                ty,
                &arr[close + 1..],
                structs,
                line,
                out,
            )?;
        }
        return Ok(());
    }
    if let Some((n, col)) = mat_cols(ty) {
        for c in 0..n {
            out.push((format!("{path}[{c}]"), col.to_string()));
        }
        return Ok(());
    }
    if let Some(members) = structs.get(ty) {
        for (mty, mname, marr) in members {
            flatten_leaves(format!("{path}.{mname}"), mty, marr, structs, line, out)?;
        }
        return Ok(());
    }
    out.push((path, ty.to_string()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::lex;

    fn text(toks: &[Tok]) -> String {
        toks.iter().map(Tok::text).collect::<Vec<_>>().join(" ")
    }

    fn samplers() -> SamplerKinds {
        SamplerKinds::from([
            (
                "colortex0".to_owned(),
                ("sampler2D".to_owned(), false, SamplerClass::ClampLinear),
            ),
            (
                "shadowtex0".to_owned(),
                (
                    "sampler2DShadow".to_owned(),
                    true,
                    SamplerClass::ClampLinear,
                ),
            ),
        ])
    }

    #[test]
    fn texture_uses_pair_with_the_shared_sampler_of_their_class() {
        let samplers = samplers();
        let cx = BodyContext {
            samplers: &samplers,
            bools: &HashMap::new(),
            user_funcs: &HashSet::new(),
        };
        let mut used = SamplerUse::default();
        let body = rewrite_body(&lex("texture(colortex0, uv).r;"), &cx, &mut used);
        assert_eq!(
            text(&body),
            format!(
                "texture ( sampler2D ( colortex0 , {} ) , uv ) . r ;",
                SamplerClass::ClampLinear.glsl_name()
            )
        );
        assert_eq!(used.filtering, BTreeSet::from([SamplerClass::ClampLinear]));
        assert!(!used.comparison);

        let body = rewrite_body(&lex("texture(shadowtex0, p);"), &cx, &mut used);
        assert_eq!(
            text(&body),
            format!("texture ( sampler2DShadow ( shadowtex0 , {SHADOW_SAMPLER_NAME} ) , p ) ;")
        );
        assert!(used.comparison);
    }

    #[test]
    fn textures_passed_to_user_functions_stay_separate_and_bool_uniforms_read_as_bools() {
        let samplers = samplers();
        let bools = HashMap::from([
            ("isDay".to_owned(), "bool".to_owned()),
            ("mask".to_owned(), "bvec2".to_owned()),
        ]);
        let user_funcs = HashSet::from(["blur".to_owned()]);
        let cx = BodyContext {
            samplers: &samplers,
            bools: &bools,
            user_funcs: &user_funcs,
        };
        let mut used = SamplerUse::default();
        let body = rewrite_body(
            &lex("blur(colortex0, uv); if (isDay && mask.x) {}"),
            &cx,
            &mut used,
        );
        assert_eq!(
            text(&body),
            "blur ( colortex0 , uv ) ; if ( ( isDay != 0 ) && bvec2 ( mask ) . x ) { }"
        );
        assert!(used.filtering.is_empty());

        let mut params = SamplerKinds::new();
        let header = rewrite_params(&lex("vec4 blur(in sampler2D tex, vec2 uv)"), &mut params);
        assert_eq!(text(&header), "vec4 blur ( texture2D tex , vec2 uv )");
        assert_eq!(
            params["tex"],
            ("sampler2D".to_owned(), false, SamplerClass::RepeatLinear)
        );
    }

    #[test]
    fn values_flatten_to_their_leaves() {
        let structs = Structs::from([(
            "Light".to_owned(),
            vec![
                ("vec3".to_owned(), "dir".to_owned(), vec![]),
                ("mat2".to_owned(), "rot".to_owned(), vec![]),
            ],
        )]);
        let mut leaves = vec![];
        flatten_leaves(
            "lights".to_owned(),
            "Light",
            &lex("[2]"),
            &structs,
            1,
            &mut leaves,
        )
        .unwrap();
        let leaves: Vec<_> = leaves.iter().map(|(p, t)| format!("{p}: {t}")).collect();
        assert_eq!(
            leaves,
            [
                "lights[0].dir: vec3",
                "lights[0].rot[0]: vec2",
                "lights[0].rot[1]: vec2",
                "lights[1].dir: vec3",
                "lights[1].rot[0]: vec2",
                "lights[1].rot[1]: vec2",
            ]
        );
        let mut leaves = vec![];
        let error = flatten_leaves(
            "v".to_owned(),
            "float",
            &lex("[N]"),
            &structs,
            7,
            &mut leaves,
        )
        .unwrap_err();
        assert_eq!(error.line, 7);
    }
}
