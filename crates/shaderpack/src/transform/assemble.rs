//! Merges a program's loose uniforms and writes each stage's final GLSL.

use super::StageDraft;
use crate::reflection::{SHADOW_SAMPLER_BINDING, SHADOW_SAMPLER_NAME, TEXTURE_GROUP};
use crate::token::{Emitter, Tok};

/// GLSL text of a draft with the program's merged uniform block, and the include-expanded
/// source line of every output line (0 for generated lines).
pub(crate) fn assemble(
    draft: &StageDraft,
    ubo: &super::uniforms::UniformLayout,
    locations: &str,
) -> (String, Vec<u32>) {
    let mut e = Emitter::default();
    e.raw(&draft.header);
    e.raw(locations);
    e.tokens(&draft.hoisted);
    if draft.ubo.iter().any(|member| member.name == "iris_Fog") {
        e.raw(FOG_STRUCT);
    }
    ubo.emit(draft, &mut e);
    e.tokens(&draft.resources);
    for class in &draft.uses.filtering {
        e.raw(&format!(
            "layout(set = {TEXTURE_GROUP}, binding = {}) uniform sampler {};\n",
            class.binding(),
            class.glsl_name()
        ));
    }
    if draft.uses.comparison {
        e.raw(&format!(
            "layout(set = {TEXTURE_GROUP}, binding = {SHADOW_SAMPLER_BINDING}) uniform samplerShadow {SHADOW_SAMPLER_NAME};\n"
        ));
    }
    e.raw(&draft.builtin_io);
    e.tokens(&draft.io);
    e.tokens(&draft.split_globals);
    e.tokens(&draft.body);
    e.finish()
}
/// Fog state of the compatibility profile, used by `gl_Fog`.
pub(super) const FOG_STRUCT: &str = "struct iris_FogParameters { vec4 color; float density; float start; float end; float scale; };\n";

/// One member of the loose-uniform block: its name and full declaration tokens.
#[derive(Clone, Debug)]
pub(crate) struct UboMember {
    pub name: String,
    pub(super) toks: Vec<Tok>,
}

impl UboMember {
    /// Stores a loose-uniform name and its complete declaration tokens.
    pub(super) fn new(name: &str, toks: Vec<Tok>) -> UboMember {
        UboMember {
            name: name.to_string(),
            toks,
        }
    }
}
