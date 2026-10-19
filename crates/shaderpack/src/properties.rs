//! `shaders.properties`: the pack-wide settings file of OptiFine/Iris packs.
//!
//! The file is first run through a C-like conditional preprocessor (`#if`, `#ifdef`, ...),
//! then read as java.util.Properties. Program enable rules and custom images are typed; other
//! keys stay in `entries`.
//! Malformed input never fails the whole file: it becomes a [`PropertiesDiagnostic`].

mod reader;
mod rules;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use crate::expr::{Expr, macro_value};

/// Parsed shaders.properties after conditional preprocessing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShaderProperties {
    /// Every key/value in file order after `#if` processing (later duplicates kept; typed
    /// accessors use the last occurrence, matching java.util.Properties).
    pub entries: Vec<(String, String)>,
    /// `program.[<dimension>/]<program>.enabled` rules, one per key.
    pub program_rules: Vec<ProgramRule>,
    /// `image.<name>` declarations, one per key.
    pub images: Vec<CustomImage>,
    /// `uniform.<type>.<name>` and `variable.<type>.<name>` definitions, one per key.
    pub custom_uniforms: Vec<CustomUniform>,
    /// Malformed lines and expressions (never fatal), with 1-based line numbers.
    pub diagnostics: Vec<PropertiesDiagnostic>,
}

/// A malformed line, directive or value; the offending entry is skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertiesDiagnostic {
    /// 1-based line number (the first line of a continued logical line).
    pub line: u32,
    /// Human-readable description of the problem.
    pub message: String,
}

impl ShaderProperties {
    /// `text` is the raw file; `defines` maps macro names visible to `#if`/`#ifdef`
    /// (enabled boolean options → "", value options → value, standard Iris macros) to values.
    ///
    /// In `#if` conditions a macro evaluates to its value parsed as a number (int, float or
    /// hex), to 1 when its value is empty, and to 0 otherwise; undefined names are 0.
    /// `#define`/`#undef` in active regions affect later conditionals.
    pub fn parse(text: &str, defines: &BTreeMap<String, String>) -> ShaderProperties {
        let mut properties = ShaderProperties::default();
        let raw = reader::read_entries(text, defines, &mut properties.diagnostics);
        // Iris interprets java.util.Properties, so only each key's last value becomes a rule;
        // a malformed last value leaves no rule for that key.
        for entry in last_by_key(&raw, |entry| &entry.key) {
            rules::apply(&mut properties, entry.line, &entry.key, &entry.value);
        }
        properties.entries = raw
            .into_iter()
            .map(|entry| (entry.key, entry.value))
            .collect();
        properties
    }

    /// Each key once, at its first position with its last value, as java.util.Properties holds it.
    pub fn effective_entries(&self) -> Vec<(&str, &str)> {
        last_by_key(&self.entries, |(key, _)| key)
            .into_iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect()
    }

    /// Last value for `key`, trimmed.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|(entry_key, _)| entry_key == key)
            .map(|(_, value)| value.as_str())
    }

    /// The enable rule for `program` in `dimension` (folder name like "world0"): a
    /// `program.<dimension>/<program>.enabled` rule wins over `program.<program>.enabled`.
    pub fn program_rule(&self, dimension: &str, program: &str) -> Option<&ProgramRule> {
        let for_program = || {
            self.program_rules
                .iter()
                .rev()
                .filter(move |rule| rule.program == program)
        };
        for_program()
            .find(|rule| rule.dimension.as_deref() == Some(dimension))
            .or_else(|| for_program().find(|rule| rule.dimension.is_none()))
    }
}

/// Keeps the last item for each key, ordered by the key's first occurrence.
fn last_by_key<T>(items: &[T], key: impl Fn(&T) -> &str) -> Vec<&T> {
    let mut positions = BTreeMap::new();
    let mut kept: Vec<&T> = Vec::new();
    for item in items {
        match positions.entry(key(item)) {
            std::collections::btree_map::Entry::Occupied(index) => kept[*index.get()] = item,
            std::collections::btree_map::Entry::Vacant(index) => {
                index.insert(kept.len());
                kept.push(item);
            }
        }
    }
    kept
}

/// `program.[<dimension>/]<program>.enabled=<expr>`: whether a program runs.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgramRule {
    /// Dimension folder (e.g. "world-1") when the key is dimension-qualified.
    pub dimension: Option<String>,
    /// Program name, e.g. "composite2" or "shadow".
    pub program: String,
    /// Condition over option macros; non-zero means enabled.
    pub expr: Expr,
    /// 1-based line of the rule.
    pub line: u32,
}

impl ProgramRule {
    /// Evaluates this rule with the supplied macros. Empty values count as one, numeric
    /// literals keep their value, and other values or missing names count as zero.
    /// Evaluation errors leave the program enabled.
    pub fn is_enabled(&self, defines: &BTreeMap<String, String>) -> bool {
        let value = self
            .expr
            .eval(&|name| defines.get(name).map(|value| macro_value(value)));
        !matches!(value, Ok(value) if value == 0.0)
    }
}

/// `image.<name> = <sampler> <format> <internalFormat> <pixelType> <clear> <relative> <w> <h> [<d>]`.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomImage {
    /// Image uniform name.
    pub name: String,
    /// Sampler uniform name for reading the image.
    pub sampler: String,
    /// Pixel format token, e.g. "rgba" or "red_integer".
    pub format: String,
    /// Internal format token, e.g. "rgba16f" or "r8ui".
    pub internal_format: String,
    /// Pixel type token, e.g. "half_float" or "unsigned_byte".
    pub pixel_type: String,
    /// Whether the image is cleared every frame.
    pub clear: bool,
    /// Image dimensions.
    pub size: ImageSize,
}

/// Size of a custom image; relative images are 2D, absolute images 1D, 2D or 3D.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageSize {
    /// Positive fractions of the screen width and height.
    Relative { width: f32, height: f32 },
    /// Positive pixel sizes; `height` is set for 2D and 3D images, `depth` for 3D.
    Absolute {
        width: u32,
        height: Option<u32>,
        depth: Option<u32>,
    },
}

/// A `uniform.<type>.<name>=<expr>` or `variable.<type>.<name>=<expr>` definition.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomUniform {
    /// Whether the value is exposed to shaders or only usable by later expressions.
    pub kind: CustomUniformKind,
    /// GLSL type token, e.g. "float" or "vec3".
    pub ty: String,
    /// Uniform or variable name.
    pub name: String,
    /// Expression computing the value each frame.
    pub expr: Expr,
    /// 1-based line of the definition.
    pub line: u32,
}

/// Kind of a [`CustomUniform`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomUniformKind {
    /// `uniform.*`: uploaded to shaders.
    Uniform,
    /// `variable.*`: an intermediate value visible only to custom uniform expressions.
    Variable,
}
