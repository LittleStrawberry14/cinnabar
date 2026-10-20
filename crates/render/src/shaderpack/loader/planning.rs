//! Device-independent buffer usage and storage sizing for translated pack programs.

use super::super::plan::{MAX_COLOR_BUFFERS, PlannedProgram, color_buffer_of};
use shaderpack::{
    BlendMode, BufferId, ProgramKind, ProgramReflection, ProgramTranslation, ShaderProperties,
    StageKind,
};

/// Destination buffer per fragment output location.
fn outputs_by_location(reflection: &ProgramReflection) -> Vec<Option<u32>> {
    let len = reflection
        .color_outputs
        .iter()
        .map(|o| o.location as usize + 1)
        .max()
        .unwrap_or(0);
    let mut outputs = vec![None; len];
    for output in &reflection.color_outputs {
        outputs[output.location as usize] = output.buffer;
    }
    outputs
}

/// Whether some function of `module` can discard a fragment.
fn discards(module: &naga::Module) -> bool {
    /// Finds a discard in this block or any nested control flow.
    fn kills(block: &naga::Block) -> bool {
        block.iter().any(|statement| match statement {
            naga::Statement::Kill => true,
            naga::Statement::Block(inner) => kills(inner),
            naga::Statement::If { accept, reject, .. } => kills(accept) || kills(reject),
            naga::Statement::Switch { cases, .. } => cases.iter().any(|c| kills(&c.body)),
            naga::Statement::Loop {
                body, continuing, ..
            } => kills(body) || kills(continuing),
            _ => false,
        })
    }
    module.functions.iter().any(|(_, f)| kills(&f.body))
        || module.entry_points.iter().any(|e| kills(&e.function.body))
}

/// Plans buffer reads, writes and preservation for one runnable program.
pub(super) fn planned(
    program: &ProgramTranslation,
    reflection: &ProgramReflection,
    properties: &ShaderProperties,
) -> PlannedProgram {
    let name = program.program.name.as_str();
    let mipmaps = program
        .consts
        .iter()
        .filter(|c| c.value.trim() == "true")
        .filter_map(|c| c.name.strip_suffix("MipmapEnabled"))
        .filter_map(|b| match BufferId::parse(b) {
            Some(BufferId::Color(i)) if i < MAX_COLOR_BUFFERS => Some(i),
            _ => None,
        })
        .collect();
    let outputs = if reflection.kind == ProgramKind::Render {
        outputs_by_location(reflection)
    } else {
        vec![]
    };
    let whole = properties.scales.iter().all(|s| s.program != name)
        && !program
            .stages
            .iter()
            .filter_map(|s| s.result.as_ref().ok())
            .any(|c| c.stage == StageKind::Fragment && discards(&c.module));
    let blends = |buffer: u32| {
        let rule = |per_buffer: bool| {
            properties.blends.iter().rev().find(|r| {
                r.program == name
                    && match r.buffer.as_deref() {
                        Some(b) => per_buffer && color_buffer_of(b) == Some(buffer),
                        None => !per_buffer,
                    }
            })
        };
        rule(true)
            .or_else(|| rule(false))
            .is_some_and(|r| r.mode != BlendMode::Off)
    };
    let partial = outputs
        .iter()
        .flatten()
        .filter(|&&b| !whole || blends(b))
        .fold(0u32, |mask, b| mask | (1 << b));
    PlannedProgram {
        name: name.to_owned(),
        group: program.program.group,
        outputs,
        reads: reflection
            .textures
            .iter()
            .map(|t| t.name.as_str())
            .chain(reflection.storage_images.iter().map(|i| i.name.as_str()))
            .filter_map(color_buffer_of)
            .collect(),
        mipmaps,
        partial,
    }
}

/// Storage-buffer sizes: the reflected minimum, raised by `bufferObject.<binding>=<size>`.
/// Resolves whole-buffer binding sizes, rejecting requests whose alignment would overflow.
pub(super) fn storage_sizes(
    reflections: &[&ProgramReflection],
    properties: &shaderpack::ShaderProperties,
) -> Result<Vec<(u32, u64)>, String> {
    let mut sizes: Vec<(u32, u64)> = vec![];
    for buffer in reflections.iter().flat_map(|r| &r.storage_buffers) {
        let declared = properties
            .get(&format!("bufferObject.{}", buffer.binding))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .unwrap_or(0);
        let size = u64::from(buffer.min_size)
            .max(declared)
            .max(16)
            .checked_next_multiple_of(16)
            .ok_or_else(|| format!("storage binding {} size overflows", buffer.binding))?;
        match sizes.iter_mut().find(|(b, _)| *b == buffer.binding) {
            Some((_, s)) => *s = (*s).max(size),
            None => sizes.push((buffer.binding, size)),
        }
    }
    Ok(sizes)
}
