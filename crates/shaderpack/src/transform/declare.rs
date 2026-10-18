//! Sorts top-level declarations into the translated module's sections: hoisted constants and
//! structs, the loose-uniform block, resources with fixed bindings, and interface variables
//! with locations shared between a program's stages.

use super::body::{BoolUniform, SamplerKinds, Structs, flatten_leaves};
use super::items::{
    Decl, Item, arr_len, is_image, is_integer, is_opaque, layout_key, loc_size, mat_cols,
    parse_decl, sampler_info, split_items, strip_layout_keys,
};
use super::{ProgramInterface, ResourceNames, TransformError, UboMember};
use crate::programs::StageKind;
use crate::reflection::{
    FIRST_OTHER_ATTRIBUTE_LOCATION, IMAGE_GROUP, STORAGE_BUFFER_GROUP, SamplerClass, StoredNumbers,
    TEXTURE_GROUP, TextureDimension, UNIFORM_GROUP, VertexAttribute,
};
use crate::token::{Tok, id, lex, p, resource_symbol};
use std::collections::{BTreeSet, HashMap, HashSet};

/// The sorted declarations of one stage.
#[derive(Default)]
pub(super) struct Collected {
    /// Structs and constants, in source order.
    pub hoisted: Vec<Tok>,
    pub ubo: Vec<UboMember>,
    pub resources: Vec<Tok>,
    pub io: Vec<Tok>,
    /// Functions and remaining statements.
    pub body: Vec<Item>,
    pub samplers: SamplerKinds,
    pub opaque: HashSet<String>,
    pub bools: HashMap<String, BoolUniform>,
    /// Bool members of anonymous uniform and storage blocks → their `bool`/`bvecN` type.
    pub block_bools: HashMap<String, String>,
    /// Statements run after the user's `main` (vertex varying packing, output widening).
    pub after_main: Vec<String>,
    /// Statements run before the user's `main` (fragment varying unpacking).
    pub before_main: Vec<String>,
    /// Private globals backing flattened varyings and widened outputs.
    pub split_globals: Vec<Tok>,
    /// Global initializers moved into the `main` prologue.
    pub global_inits: Vec<Tok>,
    pub frag_locations: BTreeSet<u32>,
    pub declared: HashSet<String>,
    /// Vertex inputs as (location, translated name).
    pub vertex_inputs: Vec<(u32, String)>,
    /// Fragment outputs as (location, translated name).
    pub frag_outputs: Vec<(u32, String)>,
    /// Pack-visible resource names as (group, binding, name).
    pub resource_names: Vec<(u32, u32, String)>,
}

/// Returns the first known source line, or zero for generated tokens.
fn first_line(toks: &[Tok]) -> u32 {
    toks.iter().map(|t| t.line).find(|l| *l != 0).unwrap_or(0)
}

/// An input whose declared type differs in kind from how the vertex stream stores its
/// attribute ([`VertexAttribute::stored_numbers`]): the name and type of the input the stage
/// reads instead, and the expression converting that into the declared type `ty`.
fn converted_input(
    attribute: &VertexAttribute,
    name: &str,
    ty: &str,
    arr: &[Tok],
) -> Option<(String, String, String)> {
    if !arr.is_empty() {
        return None;
    }
    let size = ty
        .trim_start_matches(['i', 'u'])
        .trim_start_matches("vec")
        .parse::<u8>()
        .ok();
    match attribute.stored_numbers()? {
        // Lightmap levels are stored as exact floats; integers round them back.
        StoredNumbers::Float if is_integer(ty) => {
            let read = size.map_or("float".to_string(), |size| format!("vec{size}"));
            let stored = format!("iris_float_{name}");
            let convert = format!("{ty}(round({stored}))");
            Some((stored, read, convert))
        }
        StoredNumbers::Int if ty == "float" || ty.starts_with("vec") => {
            let read = size.map_or("int".to_string(), |size| format!("ivec{size}"));
            let stored = format!("iris_int_{name}");
            let convert = format!("{ty}({stored})");
            Some((stored, read, convert))
        }
        _ => None,
    }
}

/// Collects declarations of `items` for `stage`.
pub(super) fn collect(
    items: Vec<Item>,
    stage: StageKind,
    iface: &mut ProgramInterface,
    names: &ResourceNames<'_>,
) -> Result<Collected, TransformError> {
    let mut c = Collected::default();
    let mut structs: Structs = HashMap::new();
    let mut next_attr = FIRST_OTHER_ATTRIBUTE_LOCATION;
    let mut frag_out_next = 0u32;
    for item in items {
        let (d, toks) = match item {
            Item::Decl(d, toks) => (d, toks),
            other => {
                c.body.push(other);
                continue;
            }
        };
        c.declared.extend(d.decls.iter().map(|x| x.0.clone()));
        let line = first_line(&toks);
        if d.ty == "precision" || toks.first().and_then(|t| t.ident()) == Some("precision") {
            continue;
        }
        if d.ty.is_empty() {
            // A default qualifier such as `layout(local_size_x = 8) in;`.
            c.body.push(Item::Other(toks));
            continue;
        }
        if d.ty == "struct" {
            if let Some((sname, members, _)) = &d.block {
                let mut ms = vec![];
                for m in split_items(members) {
                    if let Some(md) = parse_decl(&m) {
                        for (mn, marr, _) in md.decls {
                            let mut a = md.ty_arr.clone();
                            a.extend(marr);
                            ms.push((md.ty.clone(), mn, a));
                        }
                    }
                }
                structs.insert(sname.clone(), ms);
            }
            c.hoisted.extend(toks);
            continue;
        }
        if d.has("const") && d.block.is_none() {
            c.hoisted.extend(toks);
            continue;
        }
        if let Some((bname, members, inst)) = &d.block {
            if d.has("uniform") || d.has("buffer") {
                block(&mut c, &d, bname, members, inst, iface)
                    .map_err(|message| TransformError::new(message, line))?;
                continue;
            }
            let between_stages = (stage == StageKind::Vertex && d.has("out"))
                || (stage == StageKind::Fragment && d.has("in"));
            if between_stages && !inst.iter().any(|t| t.is("[")) {
                interface_block(
                    &mut c, iface, &structs, &d, bname, members, inst, stage, line,
                )?;
                continue;
            }
            if d.has("in") || d.has("out") {
                return Err(TransformError::new(
                    format!("interface block `{bname}` is not supported"),
                    line,
                ));
            }
            c.body.push(Item::Other(toks));
            continue;
        }
        if d.has("uniform") {
            uniforms(&mut c, &d, iface, names, line)?;
            continue;
        }
        let is_in =
            d.has("in") || (d.has("varying") && stage == StageKind::Fragment) || d.has("attribute");
        let is_out = d.has("out") || (d.has("varying") && stage == StageKind::Vertex);
        if stage == StageKind::Vertex && is_in && !d.has("varying") {
            for (name, arr, _) in &d.decls {
                let size = loc_size(&d.ty, arr_len(arr).unwrap_or(1)).ok_or_else(|| {
                    TransformError::new("vertex attribute location size overflow", line)
                })?;
                let attribute = VertexAttribute::from_glsl_name(name);
                let loc = if let Some(location) =
                    layout_key(&d.layout, "location").or_else(|| attribute.location())
                {
                    location
                } else {
                    let location = next_attr;
                    next_attr = next_attr.checked_add(size).ok_or_else(|| {
                        TransformError::new("vertex attribute location overflow", line)
                    })?;
                    location
                };
                c.vertex_inputs.push((loc, name.clone()));
                if let Some((stored, read, convert)) = converted_input(&attribute, name, &d.ty, arr)
                {
                    c.io.extend(
                        lex(&format!("layout(location = {loc}) in {read} {stored};"))
                            .into_iter()
                            .map(|t| t.at(line)),
                    );
                    c.split_globals.extend(
                        lex(&format!("{} {name};", d.ty))
                            .into_iter()
                            .map(|t| t.at(line)),
                    );
                    c.before_main.push(format!("{name} = {convert};"));
                    continue;
                }
                c.io.extend(
                    lex(&format!("layout(location = {loc}) in {} {name}", d.ty))
                        .into_iter()
                        .map(|t| t.at(line)),
                );
                c.io.extend(arr.iter().cloned());
                c.io.push(p(";"));
            }
            continue;
        }
        if (stage == StageKind::Vertex && is_out) || (stage == StageKind::Fragment && is_in) {
            let mut explicit = layout_key(&d.layout, "location");
            for (name, arr, _) in &d.decls {
                let mut a = d.ty_arr.clone();
                a.extend(arr.iter().cloned());
                varying(
                    &mut c, iface, &structs, name, &d.ty, &a, &d.quals, stage, explicit, line,
                )?;
                if let Some(location) = explicit {
                    let mut leaves = Vec::new();
                    flatten_leaves(name.clone(), &d.ty, &a, &structs, line, &mut leaves)?;
                    explicit =
                        Some(location.checked_add(leaves.len() as u32).ok_or_else(|| {
                            TransformError::new("varying location overflow", line)
                        })?);
                }
            }
            continue;
        }
        if stage == StageKind::Fragment && is_out {
            for (name, arr, _) in &d.decls {
                let loc = layout_key(&d.layout, "location").unwrap_or_else(|| {
                    while c.frag_locations.contains(&frag_out_next) {
                        frag_out_next += 1;
                    }
                    frag_out_next
                });
                c.frag_locations.insert(loc);
                if !arr.is_empty() {
                    return Err(TransformError::new(
                        format!("fragment output array `{name}` is not supported"),
                        line,
                    ));
                }
                fragment_output(&mut c, name, &d.ty, loc, line);
            }
            continue;
        }
        let plain = d
            .quals
            .iter()
            .all(|q| matches!(q.as_str(), "highp" | "mediump" | "lowp" | "precise"));
        if plain && d.decls.iter().any(|x| !x.2.is_empty()) {
            // GLSL runs global initializers before main and they may read uniforms; naga
            // needs constant initializers, so the assignment moves into main's prologue.
            for (name, arr, init) in &d.decls {
                let mut t: Vec<Tok> = d.quals.iter().map(|q| id(q).at(line)).collect();
                t.push(id(&d.ty).at(line));
                t.extend(d.ty_arr.iter().cloned());
                t.push(id(name).at(line));
                t.extend(arr.iter().cloned());
                t.push(p(";").at(line));
                c.body.push(Item::Other(t));
                if !init.is_empty() {
                    c.global_inits.extend([id(name).at(line), p("=").at(line)]);
                    c.global_inits.extend(init.iter().cloned());
                    c.global_inits.push(p(";").at(line));
                }
            }
            continue;
        }
        c.body.push(Item::Decl(d, toks));
    }
    Ok(c)
}

/// Emits a uniform or storage block with a shared binding and default layout.
fn block(
    c: &mut Collected,
    d: &Decl,
    bname: &str,
    members: &[Tok],
    inst: &[Tok],
    iface: &mut ProgramInterface,
) -> Result<(), String> {
    let storage = d.has("buffer");
    let (set, binding) = if storage {
        (
            STORAGE_BUFFER_GROUP,
            iface.storage_binding(bname, layout_key(&d.layout, "binding"))?,
        )
    } else {
        (UNIFORM_GROUP, iface.uniform_block_binding(bname)?)
    };
    let instance = inst.iter().find_map(|t| t.ident()).unwrap_or(bname);
    c.resource_names.push((set, binding, instance.to_string()));
    let mut lay = strip_layout_keys(&d.layout, &["binding", "set"]);
    if !lay
        .iter()
        .any(|t| matches!(t.ident(), Some("std140" | "std430" | "packed" | "shared")))
    {
        if !lay.is_empty() {
            lay.push(p(","));
        }
        lay.push(id(if storage { "std430" } else { "std140" }));
    }
    lay.extend(lex(&format!(", set = {set}, binding = {binding}u")));
    c.resources.extend([id("layout"), p("(")]);
    c.resources.extend(lay);
    c.resources.push(p(")"));
    for q in d.quals.iter().filter(|q| supported_qualifier(q)) {
        c.resources.push(id(q));
    }
    c.resources.extend([id(bname), p("{")]);
    // GLSL stores a block's bools as 32-bit integers, and WGSL has no host-shareable bool:
    // the members are declared as integers and accessed through conversions, as bool
    // uniforms are. A named instance's members are reached through `.` and stay bools.
    let anonymous = inst.iter().all(|t| t.ident().is_none());
    for m in split_items(members) {
        match parse_decl(&m).filter(|md| anonymous && is_bool(&md.ty)) {
            Some(md) => {
                let integer = integer_bool(&md.ty);
                for (name, _, _) in &md.decls {
                    c.block_bools.insert(name.clone(), md.ty.clone());
                }
                c.resources.extend(m.iter().map(|t| {
                    if t.ident() == Some(md.ty.as_str()) {
                        id(&integer).at(t.line)
                    } else {
                        t.clone()
                    }
                }));
            }
            None => c.resources.extend(m),
        }
    }
    c.resources.push(p("}"));
    c.resources.extend(inst.iter().cloned());
    c.resources.push(p(";"));
    Ok(())
}

/// Whether naga's parser takes a resource qualifier. It has no `coherent` or `volatile`;
/// without them a store is still visible after the barriers packs pair them with.
fn supported_qualifier(q: &str) -> bool {
    !matches!(q, "coherent" | "volatile")
}

/// Whether `ty` is `bool` or a `bvecN`.
fn is_bool(ty: &str) -> bool {
    ty == "bool" || ty.starts_with("bvec")
}

/// The integer type storing a bool type in a block (`int`, `ivecN`).
pub(super) fn integer_bool(ty: &str) -> String {
    if ty == "bool" {
        "int".to_owned()
    } else {
        format!("i{}", &ty[1..])
    }
}

/// Declares built-in vertex input `name` of type `ty` in `io` at its fixed attribute
/// location and records it in `inputs`; names without a fixed location are skipped.
pub(super) fn vertex_input(io: &mut String, inputs: &mut Vec<(u32, String)>, name: &str, ty: &str) {
    if let Some(loc) = VertexAttribute::from_glsl_name(name).location() {
        io.push_str(&format!("layout(location = {loc}) in {ty} {name};\n"));
        inputs.push((loc, name.to_string()));
    }
}

/// Assigns opaque-resource bindings and collects ordinary uniforms for the shared block.
fn uniforms(
    c: &mut Collected,
    d: &Decl,
    iface: &mut ProgramInterface,
    names: &ResourceNames<'_>,
    line: u32,
) -> Result<(), TransformError> {
    for (name, arr, _) in &d.decls {
        if is_opaque(&d.ty) {
            c.opaque.insert(name.clone());
            if let Some((tex_ty, shadow)) = sampler_info(&d.ty) {
                let b = iface
                    .texture_binding(name)
                    .map_err(|message| TransformError::new(message, line))?;
                c.resource_names.push((TEXTURE_GROUP, b, name.clone()));
                let classes = names.sampler_classes;
                let shaped = (name.clone(), Some(texture_dimension(tex_ty)));
                let class = classes
                    .get(&shaped)
                    .or_else(|| classes.get(&(name.clone(), None)))
                    .copied()
                    .unwrap_or_else(|| SamplerClass::for_name(name));
                c.samplers
                    .insert(name.clone(), (d.ty.clone(), shadow, class));
                c.resources.extend(
                    lex(&format!(
                        "layout(set = {TEXTURE_GROUP}, binding = {b}) uniform {tex_ty} {}",
                        resource_symbol(name)
                    ))
                    .into_iter()
                    .map(|t| t.at(line)),
                );
                c.resources.extend(arr.iter().cloned());
                c.resources.push(p(";"));
            } else if is_image(&d.ty) {
                let b = iface
                    .image_binding(name)
                    .map_err(|message| TransformError::new(message, line))?;
                c.resource_names.push((IMAGE_GROUP, b, name.clone()));
                let mut lay = strip_layout_keys(&d.layout, &["binding", "set"]);
                if lay.is_empty() {
                    // OpenGL allows format-less write-only images; WebGPU needs the format,
                    // which Iris knows from `image.<name>` in shaders.properties.
                    if let Some(f) = names.image_formats.get(name) {
                        lay.push(id(f));
                    }
                }
                if !lay.is_empty() {
                    lay.push(p(","));
                }
                lay.extend(lex(&format!("set = {IMAGE_GROUP}, binding = {b}")));
                c.resources.extend([id("layout").at(line), p("(")]);
                c.resources.extend(lay);
                c.resources.push(p(")"));
                for q in d.quals.iter().filter(|q| supported_qualifier(q)) {
                    c.resources.push(id(q));
                }
                c.resources.extend([id(&d.ty), id(&resource_symbol(name))]);
                c.resources.extend(arr.iter().cloned());
                c.resources.push(p(";"));
            } else {
                return Err(TransformError::new(
                    format!("unsupported opaque uniform type `{}`", d.ty),
                    line,
                ));
            }
            continue;
        }
        let ty = if is_bool(&d.ty) {
            c.bools.insert(
                name.clone(),
                BoolUniform {
                    constructor: d.ty.clone(),
                    dimensions: d.ty_arr.iter().chain(arr).filter(|t| t.is("[")).count(),
                },
            );
            integer_bool(&d.ty)
        } else {
            d.ty.clone()
        };
        let mut toks = vec![id(&ty).at(line)];
        toks.extend(d.ty_arr.iter().cloned());
        toks.push(id(name).at(line));
        toks.extend(arr.iter().cloned());
        toks.push(p(";").at(line));
        c.ubo.push(UboMember::new(name, toks));
    }
    Ok(())
}

/// Shape of a separate texture type (`texture3D`, `utexture2DArray` …).
fn texture_dimension(tex_ty: &str) -> TextureDimension {
    let shape = tex_ty
        .trim_start_matches(['i', 'u'])
        .trim_start_matches("texture");
    match shape {
        "1D" | "1DArray" => TextureDimension::D1,
        "2DArray" => TextureDimension::D2Array,
        "3D" => TextureDimension::D3,
        "Cube" => TextureDimension::Cube,
        "CubeArray" => TextureDimension::CubeArray,
        _ => TextureDimension::D2,
    }
}

/// Emits a vertex output or fragment input. Matrices, arrays and structs are not legal
/// inter-stage types in WGSL, so they become vector leaves copied in a wrapper `main`.
#[allow(clippy::too_many_arguments)]
fn varying(
    c: &mut Collected,
    iface: &mut ProgramInterface,
    structs: &Structs,
    name: &str,
    ty: &str,
    arr: &[Tok],
    quals: &[String],
    stage: StageKind,
    explicit: Option<u32>,
    line: u32,
) -> Result<(), TransformError> {
    let dir = if stage == StageKind::Vertex {
        "out"
    } else {
        "in"
    };
    let interp: Vec<&str> = quals
        .iter()
        .map(String::as_str)
        .filter(|q| {
            matches!(
                *q,
                "flat" | "smooth" | "noperspective" | "centroid" | "sample"
            )
        })
        .collect();
    let qualifiers = |leaf_ty: &str| {
        let mut q = interp.clone();
        if is_integer(leaf_ty) && !q.contains(&"flat") {
            q.push("flat");
        }
        q.join(" ")
    };
    if dir == "in" && !iface.fed_by_vertex(name, explicit) {
        // No vertex output feeds this input: GL links it and Iris declares it zeroed, but
        // WebGPU rejects the pipeline. It becomes a zeroed private global.
        let mut leaves = vec![];
        flatten_leaves(name.to_string(), ty, arr, structs, line, &mut leaves)?;
        c.split_globals.extend([id(ty).at(line), id(name).at(line)]);
        c.split_globals.extend_from_slice(arr);
        c.split_globals.push(p(";"));
        for (path, leaf_ty) in leaves {
            c.before_main.push(format!("{path} = {leaf_ty}(0);"));
        }
        return Ok(());
    }
    if arr.is_empty() && mat_cols(ty).is_none() && !structs.contains_key(ty) {
        let loc = iface.varying(
            stage,
            name,
            loc_size(ty, 1)
                .ok_or_else(|| TransformError::new("varying location size overflow", line))?,
            explicit,
        );
        let q = if dir == "in" {
            qualifiers(ty)
        } else {
            interp.join(" ")
        };
        c.io.extend(
            lex(&format!("layout(location = {loc}) {q} {dir} {ty} {name};"))
                .into_iter()
                .map(|t| t.at(line)),
        );
        return Ok(());
    }
    let mut leaves = vec![];
    flatten_leaves(name.to_string(), ty, arr, structs, line, &mut leaves)?;
    c.split_globals.extend([id(ty).at(line), id(name).at(line)]);
    c.split_globals.extend_from_slice(arr);
    c.split_globals.push(p(";"));
    let base = iface.varying(stage, name, leaves.len() as u32, explicit);
    for (k, (path, leaf_ty)) in leaves.iter().enumerate() {
        let vname = format!("iris_v_{name}_{k}");
        let loc = format!("{base} + {k}");
        c.io.extend(
            lex(&format!(
                "layout(location = {loc}) {} {dir} {leaf_ty} {vname};",
                qualifiers(leaf_ty)
            ))
            .into_iter()
            .map(|t| t.at(line)),
        );
        if dir == "out" {
            c.after_main.push(format!("{vname} = {path};"));
        } else {
            c.before_main.push(format!("{path} = {vname};"));
        }
    }
    Ok(())
}

/// Declares an `in`/`out` interface block between the vertex and fragment stages. GLSL
/// matches blocks by block and member names whatever their instance names, so each member
/// is a varying keyed `Block.member`; a named instance (`out DATA { … } data_out;`) is a
/// private global of a struct with the block's members.
#[allow(clippy::too_many_arguments)]
fn interface_block(
    c: &mut Collected,
    iface: &mut ProgramInterface,
    structs: &Structs,
    d: &Decl,
    bname: &str,
    members: &[Tok],
    inst: &[Tok],
    stage: StageKind,
    line: u32,
) -> Result<(), TransformError> {
    let dir = if stage == StageKind::Vertex {
        "out"
    } else {
        "in"
    };
    let mut fields = vec![];
    for m in split_items(members) {
        if let Some(md) = parse_decl(&m) {
            for (name, arr, _) in md.decls {
                let mut a = md.ty_arr.clone();
                a.extend(arr);
                fields.push((md.quals.clone(), md.ty.clone(), name, a));
            }
        }
    }
    let instance = inst.iter().find_map(|t| t.ident());
    if let Some(instance) = instance {
        let sname = format!("iris_block_{bname}");
        c.hoisted
            .extend([id("struct").at(line), id(&sname).at(line), p("{").at(line)]);
        for (_, ty, name, arr) in &fields {
            c.hoisted.extend([id(ty).at(line), id(name).at(line)]);
            c.hoisted.extend(arr.iter().cloned());
            c.hoisted.push(p(";").at(line));
        }
        c.hoisted.extend([p("}").at(line), p(";").at(line)]);
        c.split_globals
            .extend([id(&sname).at(line), id(instance).at(line), p(";").at(line)]);
    }
    for (quals, ty, name, arr) in &fields {
        if instance.is_none() {
            c.split_globals.extend([id(ty).at(line), id(name).at(line)]);
            c.split_globals.extend(arr.iter().cloned());
            c.split_globals.push(p(";").at(line));
        }
        let key = format!("{bname}.{name}");
        let path = instance.map_or_else(|| name.clone(), |i| format!("{i}.{name}"));
        let mut leaves = vec![];
        flatten_leaves(path, ty, arr, structs, line, &mut leaves)?;
        if dir == "in" && !iface.fed_by_vertex(&key, None) {
            for (path, leaf_ty) in leaves {
                c.before_main.push(format!("{path} = {leaf_ty}(0);"));
            }
            continue;
        }
        let interp: Vec<&str> = d
            .quals
            .iter()
            .chain(quals)
            .map(String::as_str)
            .filter(|q| {
                matches!(
                    *q,
                    "flat" | "smooth" | "noperspective" | "centroid" | "sample"
                )
            })
            .collect();
        let base = iface.varying(stage, &key, leaves.len() as u32, None);
        for (k, (path, leaf_ty)) in leaves.iter().enumerate() {
            let vname = format!("iris_b_{bname}_{name}_{k}");
            let mut q = interp.clone();
            if is_integer(leaf_ty) && !q.contains(&"flat") {
                q.push("flat");
            }
            c.io.extend(
                lex(&format!(
                    "layout(location = {base} + {k}) {} {dir} {leaf_ty} {vname};",
                    q.join(" ")
                ))
                .into_iter()
                .map(|t| t.at(line)),
            );
            if dir == "out" {
                c.after_main.push(format!("{vname} = {path};"));
            } else {
                c.before_main.push(format!("{path} = {vname};"));
            }
        }
    }
    Ok(())
}

/// Declares a fragment output. GL lets a vec3 output feed an RGBA buffer; WebGPU needs at
/// least the target's component count, so narrow outputs widen to 4 components after main.
fn fragment_output(c: &mut Collected, name: &str, ty: &str, loc: u32, line: u32) {
    let pad = match ty {
        "float" => Some(("vec4", "0.0, 0.0, 1.0")),
        "vec2" => Some(("vec4", "0.0, 1.0")),
        "vec3" => Some(("vec4", "1.0")),
        "int" => Some(("ivec4", "0, 0, 1")),
        "ivec2" => Some(("ivec4", "0, 1")),
        "ivec3" => Some(("ivec4", "1")),
        "uint" => Some(("uvec4", "0u, 0u, 1u")),
        "uvec2" => Some(("uvec4", "0u, 1u")),
        "uvec3" => Some(("uvec4", "1u")),
        _ => None,
    };
    match pad {
        Some((wide, rest)) => {
            let wname = format!("iris_out_{name}");
            c.split_globals.extend(
                lex(&format!("{ty} {name};"))
                    .into_iter()
                    .map(|t| t.at(line)),
            );
            c.io.extend(
                lex(&format!("layout(location = {loc}) out {wide} {wname};"))
                    .into_iter()
                    .map(|t| t.at(line)),
            );
            c.after_main
                .push(format!("{wname} = {wide}({name}, {rest});"));
            c.frag_outputs.push((loc, wname));
        }
        None => {
            c.io.extend(
                lex(&format!("layout(location = {loc}) out {ty} {name};"))
                    .into_iter()
                    .map(|t| t.at(line)),
            );
            c.frag_outputs.push((loc, name.to_string()));
        }
    }
}
