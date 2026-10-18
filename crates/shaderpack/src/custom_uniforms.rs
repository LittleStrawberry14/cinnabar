//! Per-frame evaluation of the `uniform.*` and `variable.*` custom uniforms declared in
//! `shaders.properties`, following OptiFine's `shaders.txt` and Iris.
//!
//! [`CustomUniforms::new`] compiles the definitions once, checking names, function arity
//! and every type it can know statically; [`CustomUniforms::evaluate`] then runs them each
//! frame against the host's built-in uniforms without allocating. Arithmetic is done in
//! `f64` and converted to each definition's declared type, so integer literals divide as
//! floats (OptiFine) and `int` results truncate toward zero (a Java `(int)` cast).

mod compile;
mod eval;
mod value;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::fmt;

use crate::{CustomUniform, CustomUniformKind};

use compile::{Binding, Compiler, Node, Shape};
use eval::{Frame, RANDOM_SEED, SmoothState};
use value::Value;

/// A typed custom or built-in uniform value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UniformValue {
    /// `bool`.
    Bool(bool),
    /// `int`.
    Int(i32),
    /// `float`.
    Float(f32),
    /// `vec2`.
    Vec2([f32; 2]),
    /// `vec3`.
    Vec3([f32; 3]),
    /// `vec4`.
    Vec4([f32; 4]),
}

/// A custom uniform or variable definition that failed to compile and was dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomUniformError {
    /// 1-based line of the definition in `shaders.properties`.
    pub line: u32,
    /// Name of the dropped uniform or variable.
    pub name: String,
    /// What went wrong, suitable for a pack diagnostic.
    pub message: String,
}

impl fmt::Display for CustomUniformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: `{}`: {}", self.line, self.name, self.message)
    }
}

impl std::error::Error for CustomUniformError {}

/// Compiled custom uniforms and variables with the state that persists between frames
/// (`smooth()` accumulators and the `random()` generator).
#[derive(Debug)]
pub struct CustomUniforms {
    /// Definitions that compiled, in file order.
    definitions: Vec<Definition>,
    /// Built-in names read by the definitions, in first-use order.
    inputs: Vec<String>,
    /// This frame's value of each input; `None` for a name the host does not know.
    input_values: Vec<Option<Value>>,
    /// Latest value of each definition, converted to its declared type.
    values: Vec<Value>,
    /// One accumulator per `smooth()` call site or explicit id.
    smooth: Vec<SmoothState>,
    /// State of the deterministic generator behind `random()` and `randomInt()`.
    rng: u64,
    /// Latest value of each `uniform.*` definition, in definition order.
    results: Vec<(String, UniformValue)>,
}

/// One compiled definition.
#[derive(Debug)]
struct Definition {
    ty: DeclaredType,
    node: Node,
    /// Index into [`CustomUniforms::results`] for a `uniform.*`; `None` for a `variable.*`.
    result: Option<usize>,
}

impl CustomUniforms {
    /// Compiles definitions in file order; a definition may reference built-ins and earlier
    /// definitions only (Iris rule). Unsupported types, unknown functions, wrong arity,
    /// statically mismatched types and self or forward references become errors and that
    /// definition is dropped; later definitions referencing it fail too. A name defined
    /// twice keeps its first definition.
    pub fn new(definitions: &[CustomUniform]) -> (CustomUniforms, Vec<CustomUniformError>) {
        let mut first_lines: HashMap<&str, u32> = HashMap::new();
        for definition in definitions {
            first_lines
                .entry(definition.name.as_str())
                .or_insert(definition.line);
        }
        let mut bindings: HashMap<String, Binding> = HashMap::new();
        let mut compiled = Vec::new();
        let mut inputs = Vec::new();
        let mut smooth_keys = Vec::new();
        let mut results = Vec::new();
        let mut errors = Vec::new();
        for definition in definitions {
            let name = definition.name.as_str();
            let outcome = if bindings.contains_key(name) {
                let line = first_lines.get(name).copied().unwrap_or(definition.line);
                Err(format!("`{name}` is already defined on line {line}"))
            } else {
                let inputs_len = inputs.len();
                let smooth_len = smooth_keys.len();
                let mut compiler = Compiler {
                    current: name,
                    bindings: &bindings,
                    first_lines: &first_lines,
                    inputs: &mut inputs,
                    smooth_keys: &mut smooth_keys,
                };
                let outcome = compile_definition(definition, &mut compiler);
                if outcome.is_err() {
                    inputs.truncate(inputs_len);
                    smooth_keys.truncate(smooth_len);
                }
                outcome
            };
            match outcome {
                Ok((ty, node)) => {
                    bindings.insert(
                        definition.name.clone(),
                        Binding::Defined {
                            index: compiled.len(),
                            shape: ty.shape(),
                        },
                    );
                    let result = (definition.kind == CustomUniformKind::Uniform).then(|| {
                        results.push((definition.name.clone(), ty.zero()));
                        results.len() - 1
                    });
                    compiled.push(Definition { ty, node, result });
                }
                Err(message) => {
                    bindings
                        .entry(definition.name.clone())
                        .or_insert(Binding::Failed);
                    errors.push(CustomUniformError {
                        line: definition.line,
                        name: definition.name.clone(),
                        message,
                    });
                }
            }
        }
        let uniforms = CustomUniforms {
            values: compiled.iter().map(|d| Value::from(d.ty.zero())).collect(),
            definitions: compiled,
            input_values: vec![None; inputs.len()],
            inputs,
            smooth: vec![SmoothState::default(); smooth_keys.len()],
            rng: RANDOM_SEED,
            results,
        };
        (uniforms, errors)
    }

    /// Names of the built-in uniforms the definitions read, each once, in first-use order,
    /// so the host only computes what is needed. A vector component access such as
    /// `cameraPosition.y` reads `cameraPosition`; a matrix element such as
    /// `gbufferModelView.3.1` (GLSL `gbufferModelView[3][1]`) is read by its full dotted
    /// name and must be answered with a scalar.
    pub fn inputs(&self) -> impl Iterator<Item = &str> {
        self.inputs.iter().map(String::as_str)
    }

    /// Evaluates every definition in order and returns the `uniform.*` results (not
    /// `variable.*`) in definition order.
    ///
    /// `builtin(name)` is called once per [`inputs`](Self::inputs) name and returns that
    /// built-in's value (e.g. `"cameraPosition"` → `Vec3`, `"frameTimeCounter"` → `Float`,
    /// `"isEyeInWater"` → `Int`); `None` evaluates as 0. `frame_time` is the seconds since
    /// the previous call and drives `smooth()`; a negative or non-finite value counts as 0.
    /// A definition whose value has the wrong type at run time (a built-in of unexpected
    /// type) yields its type's zero value.
    pub fn evaluate(
        &mut self,
        builtin: &dyn Fn(&str) -> Option<UniformValue>,
        frame_time: f32,
    ) -> &[(String, UniformValue)] {
        for (slot, name) in self.input_values.iter_mut().zip(&self.inputs) {
            *slot = builtin(name).map(Value::from);
        }
        let elapsed = if frame_time.is_finite() && frame_time > 0.0 {
            f64::from(frame_time)
        } else {
            0.0
        };
        for state in &mut self.smooth {
            state.elapsed += elapsed;
        }
        for (index, definition) in self.definitions.iter().enumerate() {
            let (earlier, rest) = self.values.split_at_mut(index);
            let mut frame = Frame {
                inputs: &self.input_values,
                values: earlier,
                smooth: &mut self.smooth,
                rng: &mut self.rng,
            };
            let converted = definition
                .node
                .eval(&mut frame)
                .ok()
                .and_then(|value| definition.ty.convert(value))
                .unwrap_or_else(|| definition.ty.zero());
            if let Some(slot) = rest.first_mut() {
                *slot = Value::from(converted);
            }
            if let Some(result) = definition.result
                && let Some(entry) = self.results.get_mut(result)
            {
                entry.1 = converted;
            }
        }
        &self.results
    }
}

/// Compiles one definition's type and expression, checking the expression fits the type.
fn compile_definition(
    definition: &CustomUniform,
    compiler: &mut Compiler<'_>,
) -> Result<(DeclaredType, Node), String> {
    let Some(ty) = DeclaredType::parse(&definition.ty) else {
        return Err(format!(
            "unsupported type `{}` (expected float, int, bool, vec2, vec3 or vec4)",
            definition.ty
        ));
    };
    let (node, shape) = compiler.compile(&definition.expr)?;
    match (ty.shape(), shape) {
        (_, Shape::Dynamic) => Ok((ty, node)),
        (declared, found) if declared == found => Ok((ty, node)),
        (_, found) => Err(format!(
            "the expression is {found} but the {} is declared `{}`",
            match definition.kind {
                CustomUniformKind::Uniform => "uniform",
                CustomUniformKind::Variable => "variable",
            },
            definition.ty
        )),
    }
}

/// The declared type of a definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeclaredType {
    Bool,
    Int,
    Float,
    /// `vec2`, `vec3` or `vec4` by component count.
    Vector(u8),
}

impl DeclaredType {
    /// Parses the type token of `uniform.<type>.<name>`; `None` if unsupported.
    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "bool" => Self::Bool,
            "int" => Self::Int,
            "float" => Self::Float,
            "vec2" => Self::Vector(2),
            "vec3" => Self::Vector(3),
            "vec4" => Self::Vector(4),
            _ => return None,
        })
    }

    /// The static shape a value of this type has.
    fn shape(self) -> Shape {
        match self {
            Self::Vector(len) => Shape::Vector(len),
            Self::Bool | Self::Int | Self::Float => Shape::Scalar,
        }
    }

    /// The value used before the first evaluation and when evaluation fails.
    fn zero(self) -> UniformValue {
        match self {
            Self::Bool => UniformValue::Bool(false),
            Self::Int => UniformValue::Int(0),
            Self::Float => UniformValue::Float(0.0),
            Self::Vector(2) => UniformValue::Vec2([0.0; 2]),
            Self::Vector(3) => UniformValue::Vec3([0.0; 3]),
            Self::Vector(_) => UniformValue::Vec4([0.0; 4]),
        }
    }

    /// Converts an evaluated value to this type: scalars convert freely (`int` truncates
    /// toward zero and saturates like a Java cast, `bool` is non-zero), vectors must have
    /// exactly this many components. `None` on a shape mismatch.
    fn convert(self, value: Value) -> Option<UniformValue> {
        match self {
            Self::Bool => value.truth().map(UniformValue::Bool),
            Self::Int => value.number().map(|n| UniformValue::Int(n as i32)),
            Self::Float => value.number().map(|n| UniformValue::Float(n as f32)),
            Self::Vector(len) => {
                let Value::Vector { len: found, items } = value else {
                    return None;
                };
                if found != len {
                    return None;
                }
                let [x, y, z, w] = items.map(|item| item as f32);
                Some(match len {
                    2 => UniformValue::Vec2([x, y]),
                    3 => UniformValue::Vec3([x, y, z]),
                    _ => UniformValue::Vec4([x, y, z, w]),
                })
            }
        }
    }
}
