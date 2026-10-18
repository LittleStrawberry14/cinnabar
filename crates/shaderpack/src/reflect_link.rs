//! Checks the cross-stage contract that per-module validation cannot establish.

use crate::compile::CompiledStage;
use crate::programs::StageKind;
use crate::reflect::value_type;
use crate::reflection::ValueType;
use naga::{Binding, TypeInner};
use std::collections::BTreeMap;

/// Requires every fragment input to have a compatible vertex output at its location: the
/// same scalar type and at most the output's component count, the rule WebGPU applies (a
/// fragment input reading `.xy` of a `vec4` output, as GL drivers also accept).
pub(crate) fn validate(stages: &[&CompiledStage]) -> Result<(), String> {
    let Some(vertex) = stages.iter().find(|stage| stage.stage == StageKind::Vertex) else {
        return Ok(());
    };
    let Some(fragment) = stages
        .iter()
        .find(|stage| stage.stage == StageKind::Fragment)
    else {
        return Ok(());
    };
    let mut outputs = BTreeMap::new();
    if let Some(entry) = vertex.module.entry_points.first()
        && let Some(result) = &entry.function.result
    {
        locations(
            &vertex.module,
            result.ty,
            result.binding.as_ref(),
            &mut outputs,
        )?;
    }
    let mut inputs = BTreeMap::new();
    if let Some(entry) = fragment.module.entry_points.first() {
        for argument in &entry.function.arguments {
            locations(
                &fragment.module,
                argument.ty,
                argument.binding.as_ref(),
                &mut inputs,
            )?;
        }
    }
    for (location, input) in inputs {
        match outputs.get(&location) {
            None => {
                return Err(format!(
                    "fragment input location {location} has no vertex output"
                ));
            }
            Some(output) if !reads_from(input, *output) => {
                return Err(format!(
                    "vertex output and fragment input at location {location} have different types"
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// Whether a fragment input of type `input` can read a vertex output of type `output`.
fn reads_from(input: ValueType, output: ValueType) -> bool {
    let scalar_and_size = |ty| match ty {
        ValueType::Scalar(scalar) => Some((scalar, 1)),
        ValueType::Vector { size, scalar } => Some((scalar, size)),
        ValueType::Matrix { .. } => None,
    };
    match (scalar_and_size(input), scalar_and_size(output)) {
        (Some((input, read)), Some((output, written))) => input == output && read <= written,
        _ => input == output,
    }
}

/// Collects typed user locations from direct values or entry-point interface structs.
fn locations(
    module: &naga::Module,
    ty: naga::Handle<naga::Type>,
    binding: Option<&Binding>,
    out: &mut BTreeMap<u32, ValueType>,
) -> Result<(), String> {
    match (binding, &module.types[ty].inner) {
        (Some(Binding::Location { location, .. }), inner) => {
            let ty = value_type(inner).ok_or_else(|| {
                format!("unsupported stage interface type at location {location}")
            })?;
            if out.insert(*location, ty).is_some() {
                return Err(format!("duplicate stage interface location {location}"));
            }
        }
        (None, TypeInner::Struct { members, .. }) => {
            for member in members {
                locations(module, member.ty, member.binding.as_ref(), out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Compares resource layouts structurally across modules, whose type handles are unrelated.
pub(crate) fn same_resource_type(
    left: &naga::Module,
    left_ty: naga::Handle<naga::Type>,
    right: &naga::Module,
    right_ty: naga::Handle<naga::Type>,
) -> bool {
    let recurse = |a, b| same_resource_type(left, a, right, b);
    match (&left.types[left_ty].inner, &right.types[right_ty].inner) {
        (TypeInner::Scalar(a), TypeInner::Scalar(b)) => a == b,
        (
            TypeInner::Vector {
                size: a,
                scalar: sa,
            },
            TypeInner::Vector {
                size: b,
                scalar: sb,
            },
        ) => a == b && sa == sb,
        (
            TypeInner::Matrix {
                columns: ca,
                rows: ra,
                scalar: sa,
            },
            TypeInner::Matrix {
                columns: cb,
                rows: rb,
                scalar: sb,
            },
        ) => ca == cb && ra == rb && sa == sb,
        (
            TypeInner::Array {
                base: a,
                size: na,
                stride: sa,
            },
            TypeInner::Array {
                base: b,
                size: nb,
                stride: sb,
            },
        ) => na == nb && sa == sb && recurse(*a, *b),
        (
            TypeInner::Struct {
                members: a,
                span: sa,
            },
            TypeInner::Struct {
                members: b,
                span: sb,
            },
        ) => {
            sa == sb
                && a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|(a, b)| a.name == b.name && a.offset == b.offset && recurse(a.ty, b.ty))
        }
        (
            TypeInner::Image {
                dim: da,
                arrayed: aa,
                class: ca,
            },
            TypeInner::Image {
                dim: db,
                arrayed: ab,
                class: cb,
            },
        ) => {
            da == db
                && aa == ab
                && match (ca, cb) {
                    (
                        naga::ImageClass::Storage { format: a, .. },
                        naga::ImageClass::Storage { format: b, .. },
                    ) => a == b,
                    _ => ca == cb,
                }
        }
        (TypeInner::Sampler { comparison: a }, TypeInner::Sampler { comparison: b }) => a == b,
        _ => false,
    }
}
