//! Top-level item splitting, the light declaration parser, and GLSL type helpers.

use super::TransformError;
use crate::token::{Tok, TokKind, int};

/// Maximum balanced delimiter nesting accepted by the translator.
const MAX_NESTING: usize = 256;

/// Storage, interpolation and precision qualifiers recognised before a declaration's type.
pub(super) const QUALIFIERS: &[&str] = &[
    "const",
    "uniform",
    "in",
    "out",
    "inout",
    "varying",
    "attribute",
    "flat",
    "smooth",
    "noperspective",
    "centroid",
    "invariant",
    "buffer",
    "shared",
    "readonly",
    "writeonly",
    "coherent",
    "volatile",
    "restrict",
    "highp",
    "mediump",
    "lowp",
    "precise",
    "patch",
    "sample",
];

/// A top-level declaration: `layout(...) quals type[arr] a[arr] = init, b;`, a block
/// `quals Name { members } inst;`, or a struct definition.
#[derive(Debug, Clone)]
pub(super) struct Decl {
    pub layout: Vec<Tok>,
    pub quals: Vec<String>,
    /// The type name; `"struct"` / `"block"` for those forms; empty for `layout(...) in;`.
    pub ty: String,
    pub ty_arr: Vec<Tok>,
    /// Declarators: (name, array tokens including brackets, initializer tokens).
    pub decls: Vec<(String, Vec<Tok>, Vec<Tok>)>,
    /// Struct or block: (name, member tokens, instance tokens).
    pub block: Option<(String, Vec<Tok>, Vec<Tok>)>,
}

impl Decl {
    /// Checks whether the declaration carries the requested qualifier.
    pub fn has(&self, q: &str) -> bool {
        self.quals.iter().any(|x| x == q)
    }
}

/// A top-level item of the preprocessed stream.
pub(super) enum Item {
    Func { name: String, toks: Vec<Tok> },
    Decl(Decl, Vec<Tok>),
    Other(Vec<Tok>),
}

/// Splits the token stream into top-level statements and function definitions.
pub(super) fn split_items(toks: &[Tok]) -> Vec<Vec<Tok>> {
    let mut items = vec![];
    let mut cur: Vec<Tok> = vec![];
    let mut brace = 0i32;
    let mut func = false;
    for t in toks {
        cur.push(t.clone());
        if t.is("{") {
            if brace == 0 && cur.len() >= 2 && cur[cur.len() - 2].is(")") {
                func = true;
            }
            brace += 1;
        } else if t.is("}") {
            brace -= 1;
            if brace == 0 && func {
                items.push(std::mem::take(&mut cur));
                func = false;
            }
        } else if t.is(";") && brace == 0 {
            items.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        items.push(cur);
    }
    items
}

/// Rejects unbalanced delimiter kinds before any transformation consumes token ranges.
pub(super) fn validate_delimiters(toks: &[Tok]) -> Result<(), TransformError> {
    let mut stack: Vec<&Tok> = Vec::new();
    for token in toks {
        if token.is("(") || token.is("[") || token.is("{") {
            if stack.len() >= MAX_NESTING {
                return Err(TransformError::new(
                    "shader nesting is too deep",
                    token.line,
                ));
            }
            stack.push(token);
        } else if token.is(")") || token.is("]") || token.is("}") {
            let expected = match token.text() {
                ")" => "(",
                "]" => "[",
                _ => "{",
            };
            if !stack.pop().is_some_and(|open| open.is(expected)) {
                return Err(TransformError::new(
                    "mismatched shader delimiter",
                    token.line,
                ));
            }
        }
    }
    if let Some(open) = stack.last() {
        return Err(TransformError::new("unclosed shader delimiter", open.line));
    }
    Ok(())
}

/// Finds the closing token of a delimiter pair, returning None for malformed ranges.
pub(super) fn matching(toks: &[Tok], open_idx: usize, open: &str, close: &str) -> Option<usize> {
    if !toks.get(open_idx)?.is(open) {
        return None;
    }
    let mut depth = 0usize;
    for (i, token) in toks.iter().enumerate().skip(open_idx) {
        if token.is(open) {
            depth += 1;
        } else if token.is(close) {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Splits call arguments at top-level commas.
pub(super) fn split_args(toks: &[Tok]) -> Vec<Vec<Tok>> {
    let mut out: Vec<Vec<Tok>> = vec![];
    let mut cur = vec![];
    let mut depth = 0;
    for t in toks {
        if t.is("(") || t.is("[") || t.is("{") {
            depth += 1;
        } else if t.is(")") || t.is("]") || t.is("}") {
            depth -= 1;
        } else if t.is(",") && depth == 0 {
            out.push(std::mem::take(&mut cur));
            continue;
        }
        cur.push(t.clone());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A function parameter.
#[derive(Clone)]
pub(super) struct Param {
    pub toks: Vec<Tok>,
    pub opaque: bool,
    pub name: String,
    pub ty: String,
}

/// The `(` index and parameters of a function definition's tokens.
pub(super) fn func_params(toks: &[Tok]) -> Option<(usize, Vec<Param>)> {
    let lp = toks.iter().position(|t| t.is("("))?;
    let rp = matching(toks, lp, "(", ")")?;
    let mut params = vec![];
    for a in split_args(&toks[lp + 1..rp]) {
        if a.len() == 1 && a[0].ident() == Some("void") {
            continue;
        }
        let type_index = a
            .iter()
            .position(|t| t.ident().is_some_and(|n| !QUALIFIERS.contains(&n)))?;
        let ty = a[type_index].ident()?.to_string();
        let mut name_index = type_index + 1;
        while a.get(name_index).is_some_and(|t| t.is("[")) {
            name_index = matching(&a, name_index, "[", "]")? + 1;
        }
        let name = a
            .get(name_index)
            .and_then(Tok::ident)
            .unwrap_or("")
            .to_string();
        params.push(Param {
            opaque: is_opaque(&ty),
            ty,
            toks: a,
            name,
        });
    }
    Some((lp, params))
}

/// Gives `T[] x = T[](a, b)` / `T x[] = T[](...)` explicit sizes. GLSL infers them from the
/// initializer; naga's GLSL frontend would make a runtime-sized array instead.
pub(super) fn size_unsized_arrays(src: &[Tok]) -> Vec<Tok> {
    let ctor_len = |open: usize| -> usize {
        let close = matching(src, open, "(", ")");
        close.map_or(0, |close| split_args(&src[open + 1..close]).len())
    };
    let mut out = Vec::with_capacity(src.len());
    for i in 0..src.len() {
        out.push(src[i].clone());
        if !(src[i].is("[") && src.get(i + 1).is_some_and(|t| t.is("]"))) {
            continue;
        }
        if src.get(i + 2).is_some_and(|t| t.is("(")) {
            out.push(int(ctor_len(i + 2)).at(src[i].line));
            continue;
        }
        let mut j = i + 2;
        while j + 3 < src.len() && !src[j].is(";") {
            if src[j].is("=")
                && src[j + 2].is("[")
                && src[j + 3].is("]")
                && src.get(j + 4).is_some_and(|t| t.is("("))
            {
                out.push(int(ctor_len(j + 4)).at(src[i].line));
                break;
            }
            j += 1;
        }
    }
    out
}

/// Merges layout entries in order, replacing earlier entries in the same layout class.
fn merge_layout(layout: &mut Vec<Tok>, incoming: &[Tok]) {
    let mut entries: Vec<Vec<Tok>> = layout
        .split(|t| t.is(","))
        .filter(|entry| !entry.is_empty())
        .map(<[Tok]>::to_vec)
        .collect();
    for entry in incoming
        .split(|t| t.is(","))
        .filter(|entry| !entry.is_empty())
    {
        let mut entry = entry.to_vec();
        if let Some(name) = entry[0].ident() {
            let name = name.to_ascii_lowercase();
            entries.retain(|old| {
                old[0]
                    .ident()
                    .is_none_or(|old| layout_class(old) != layout_class(&name))
            });
            entry[0] = crate::token::id(&name).at(entry[0].line);
        }
        entries.push(entry);
    }
    layout.clear();
    for entry in entries {
        if !layout.is_empty() {
            layout.push(crate::token::p(","));
        }
        layout.extend(entry);
    }
}

/// Groups mutually exclusive block packing and matrix order qualifiers.
fn layout_class(name: &str) -> &str {
    match name {
        "std140" | "std430" | "shared" | "packed" => "block_packing",
        "row_major" | "column_major" => "matrix_order",
        _ => name,
    }
}

/// Parses one top-level statement as a declaration; `None` for prototypes and anything else.
pub(super) fn parse_decl(toks: &[Tok]) -> Option<Decl> {
    let mut i = 0;
    let mut layout = vec![];
    let mut quals = vec![];
    loop {
        match toks.get(i)?.ident() {
            Some("layout") => {
                let close = matching(toks, i + 1, "(", ")")?;
                merge_layout(&mut layout, toks.get(i + 2..close)?);
                i = close + 1;
            }
            Some(q) if QUALIFIERS.contains(&q) => {
                quals.push(q.to_string());
                i += 1;
            }
            _ => break,
        }
    }
    let empty = |ty: &str, block| Decl {
        layout: layout.clone(),
        quals: quals.clone(),
        ty: ty.to_string(),
        ty_arr: vec![],
        decls: vec![],
        block,
    };
    if toks.get(i)?.is(";") {
        return Some(empty("", None));
    }
    let last = toks.len() - 1;
    if toks[i].ident() == Some("struct") {
        let name = toks.get(i + 1)?.ident()?.to_string();
        if !toks.get(i + 2)?.is("{") {
            return None;
        }
        let close = matching(toks, i + 2, "{", "}")?;
        let members = toks.get(i + 3..close)?.to_vec();
        let inst = toks.get(close + 1..last)?.to_vec();
        return Some(empty("struct", Some((name, members, inst))));
    }
    let ty = toks[i].ident()?.to_string();
    i += 1;
    if toks.get(i)?.is("{") {
        let close = matching(toks, i, "{", "}")?;
        let members = toks.get(i + 1..close)?.to_vec();
        let inst = toks.get(close + 1..last)?.to_vec();
        return Some(empty("block", Some((ty, members, inst))));
    }
    let mut ty_arr = vec![];
    while toks.get(i)?.is("[") {
        let close = matching(toks, i, "[", "]")?;
        ty_arr.extend_from_slice(&toks[i..=close]);
        i = close + 1;
    }
    let mut decls = vec![];
    while i < last {
        let name = toks[i].ident()?.to_string();
        i += 1;
        if toks.get(i)?.is("(") {
            return None;
        }
        let mut arr = vec![];
        while toks.get(i)?.is("[") {
            let close = matching(toks, i, "[", "]")?;
            arr.extend_from_slice(&toks[i..=close]);
            i = close + 1;
        }
        let mut init = vec![];
        if toks.get(i)?.is("=") {
            i += 1;
            let mut depth = 0;
            while i < last {
                let t = &toks[i];
                if t.is("(") || t.is("[") || t.is("{") {
                    depth += 1;
                } else if t.is(")") || t.is("]") || t.is("}") {
                    depth -= 1;
                } else if t.is(",") && depth == 0 {
                    break;
                }
                init.push(t.clone());
                i += 1;
            }
        }
        decls.push((name, arr, init));
        if toks.get(i)?.is(",") {
            i += 1;
        }
    }
    Some(Decl {
        layout,
        quals,
        ty,
        ty_arr,
        decls,
        block: None,
    })
}

/// Length of a single `[N]` array suffix (1 when empty).
pub(super) fn arr_len(arr: &[Tok]) -> Option<u32> {
    if arr.is_empty() {
        return Some(1);
    }
    if arr.len() == 3
        && let TokKind::Int(s) = &arr[1].kind
    {
        return unsigned_literal(s);
    }
    None
}

/// Reads the decimal or hexadecimal spelling of an unsigned interface integer.
fn unsigned_literal(text: &str) -> Option<u32> {
    let text = text.trim_end_matches(['u', 'U']);
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(digits) => u32::from_str_radix(digits, 16).ok(),
        None => text.parse().ok(),
    }
}

/// The separate texture type for a combined sampler type, and whether it compares depth.
pub(super) fn sampler_info(ty: &str) -> Option<(&'static str, bool)> {
    Some(match ty {
        "sampler1D" => ("texture1D", false),
        "sampler2D" => ("texture2D", false),
        "sampler3D" => ("texture3D", false),
        "samplerCube" => ("textureCube", false),
        "sampler1DArray" => ("texture1DArray", false),
        "sampler2DArray" => ("texture2DArray", false),
        "samplerCubeArray" => ("textureCubeArray", false),
        "sampler2DMS" => ("texture2DMS", false),
        "sampler2DRect" => ("texture2D", false),
        "sampler2DShadow" => ("texture2D", true),
        "sampler2DArrayShadow" => ("texture2DArray", true),
        "samplerCubeShadow" => ("textureCube", true),
        "isampler1D" => ("itexture1D", false),
        "usampler1D" => ("utexture1D", false),
        "isampler2D" => ("itexture2D", false),
        "usampler2D" => ("utexture2D", false),
        "isampler3D" => ("itexture3D", false),
        "usampler3D" => ("utexture3D", false),
        "isampler1DArray" => ("itexture1DArray", false),
        "usampler1DArray" => ("utexture1DArray", false),
        "isampler2DArray" => ("itexture2DArray", false),
        "usampler2DArray" => ("utexture2DArray", false),
        _ => return None,
    })
}

/// Recognises floating-point, signed and unsigned GLSL image type prefixes.
pub(super) fn is_image(ty: &str) -> bool {
    ty.starts_with("image") || ty.starts_with("iimage") || ty.starts_with("uimage")
}

/// Recognises sampler, image and atomic-counter types that need resource handling.
pub(super) fn is_opaque(ty: &str) -> bool {
    sampler_info(ty).is_some() || is_image(ty) || ty.starts_with("sampler") || ty == "atomic_uint"
}

/// Recognises integer and boolean scalar or vector types for flat interpolation.
pub(super) fn is_integer(ty: &str) -> bool {
    matches!(ty, "int" | "uint" | "bool")
        || ty.starts_with("ivec")
        || ty.starts_with("uvec")
        || ty.starts_with("bvec")
}

/// Interface locations a value of `ty` occupies (`arr` elements).
pub(super) fn loc_size(ty: &str, arr: u32) -> Option<u32> {
    let base: u32 = match ty {
        "dvec3" | "dvec4" => 2,
        t => mat_cols(t).map(|(c, _)| c).unwrap_or(1),
    };
    base.checked_mul(arr.max(1))
}

/// (columns, column type) of a `matC[xR]` type.
pub(super) fn mat_cols(ty: &str) -> Option<(u32, &'static str)> {
    let rest = ty.strip_prefix("mat")?;
    let (c, r) = match rest.split_once('x') {
        Some((c, r)) => (c.parse::<u32>().ok()?, r.parse::<u32>().ok()?),
        None => {
            let n = rest.parse::<u32>().ok()?;
            (n, n)
        }
    };
    if !(2..=4).contains(&c) {
        return None;
    }
    let col = match r {
        2 => "vec2",
        3 => "vec3",
        4 => "vec4",
        _ => return None,
    };
    Some((c, col))
}

/// The integer value of `key = N` inside a layout qualifier.
pub(super) fn layout_key(layout: &[Tok], key: &str) -> Option<u32> {
    let i = layout.iter().position(|t| t.ident() == Some(key))?;
    if let (
        Some(e),
        Some(Tok {
            kind: TokKind::Int(n),
            ..
        }),
    ) = (layout.get(i + 1), layout.get(i + 2))
        && e.is("=")
    {
        return unsigned_literal(n);
    }
    None
}

/// The layout qualifier without the entries starting with one of `keys`.
pub(super) fn strip_layout_keys(layout: &[Tok], keys: &[&str]) -> Vec<Tok> {
    let mut parts: Vec<Vec<Tok>> = vec![vec![]];
    for t in layout {
        if t.is(",") {
            parts.push(vec![]);
        } else if let Some(last) = parts.last_mut() {
            last.push(t.clone());
        }
    }
    let mut out = vec![];
    for part in parts
        .into_iter()
        .filter(|p| !p.is_empty() && !keys.contains(&p[0].ident().unwrap_or("")))
    {
        if !out.is_empty() {
            out.push(crate::token::p(","));
        }
        out.extend(part);
    }
    out
}
