//! Interface variables standing in for the compatibility profile's built-in inputs,
//! varyings and `gl_FragData` outputs.

use super::declare::Collected;
use super::legacy::Legacy;
use super::{ProgramInterface, TransformError};
use crate::programs::StageKind;

/// Declares the built-in vertex inputs, varyings and fragment outputs `lg` uses, recording
/// them in `c` and `iface`, and returns their GLSL declarations. A fragment varying no vertex
/// output feeds becomes a zeroed private global.
pub(super) fn declare(
    stage: StageKind,
    lg: &Legacy,
    c: &mut Collected,
    iface: &mut ProgramInterface,
) -> Result<String, TransformError> {
    let mut builtin_io = String::new();
    if stage == StageKind::Vertex {
        // Inputs Iris supplies without the pack declaring them.
        for (n, ty) in [("mc_chunkFade", "float"), ("dhMaterialId", "int")] {
            if !c.declared.contains(n) && lg.toks.iter().any(|t| t.ident() == Some(n)) {
                super::declare::vertex_input(&mut builtin_io, &mut c.vertex_inputs, n, ty);
            }
        }
        for (g, n, ty) in [
            ("gl_Vertex", "iris_Vertex", "vec4"),
            ("gl_Color", "iris_Color", "vec4"),
            ("gl_Normal", "iris_Normal", "vec3"),
            ("gl_MultiTexCoord0", "iris_MultiTexCoord0", "vec4"),
            ("gl_MultiTexCoord1", "iris_MultiTexCoord1", "vec4"),
            ("gl_MultiTexCoord2", "iris_MultiTexCoord2", "vec4"),
            ("gl_MultiTexCoord3", "iris_MultiTexCoord3", "vec4"),
        ] {
            if lg.builtins.contains(g) {
                super::declare::vertex_input(&mut builtin_io, &mut c.vertex_inputs, n, ty);
            }
        }
    }
    let varying_dir = match stage {
        StageKind::Vertex => Some("out"),
        StageKind::Fragment => Some("in"),
        _ => None,
    };
    if let Some(dir) = varying_dir {
        let front = (stage == StageKind::Vertex && lg.builtins.contains("gl_FrontColor"))
            || (stage == StageKind::Fragment && lg.builtins.contains("gl_Color"));
        let mut varying = |name: &str, ty: &str| {
            if dir == "in" && !iface.fed_by_vertex(name, None) {
                builtin_io.push_str(&format!("{ty} {name};\n"));
                c.before_main.push(format!("{name} = {ty}(0);"));
                return;
            }
            let loc = iface.varying(stage, name, 1, None);
            builtin_io.push_str(&format!("layout(location = {loc}) {dir} {ty} {name};\n"));
        };
        if front {
            varying("iris_FrontColor", "vec4");
        }
        if lg.builtins.contains("gl_FogFragCoord") {
            varying("iris_FogFragCoord", "float");
        }
        for t in &lg.tex_coords {
            varying(&format!("iris_TexCoord{t}"), "vec4");
        }
    }
    if stage == StageKind::Fragment {
        for f in &lg.frag_data {
            if c.frag_locations.contains(f) {
                return Err(TransformError::new(
                    format!("gl_FragData[{f}] collides with a declared output"),
                    0,
                ));
            }
            let name = format!("iris_FragData{f}");
            builtin_io.push_str(&format!("layout(location = {f}) out vec4 {name};\n"));
            c.frag_outputs.push((*f, name));
        }
    }
    Ok(builtin_io)
}
