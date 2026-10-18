//! Specializes opaque arguments while leaving value overload resolution to naga.

use super::TransformError;
use super::bindings::Bindings;
use super::items::{Item, Param, func_params, is_opaque, matching, split_args};
use crate::token::{Tok, TokKind, bound_resource, id, p};
use std::collections::{HashMap, HashSet};

/// Maximum resource-helper nesting expanded before reporting an unsupported call graph.
const MAX_ROUNDS: usize = 32;

/// One stable source overload, independent of insertion positions in the output item list.
struct Template {
    name: String,
    toks: Vec<Tok>,
    params: Vec<Param>,
    /// Index of the first non-template item after the template: its instances go there,
    /// after the globals and helpers it may use.
    anchor: usize,
}

/// An instantiated source overload and the global resource bound to each opaque parameter.
#[derive(Clone, Hash, PartialEq, Eq)]
struct Instance {
    template: usize,
    resources: Vec<(usize, String)>,
}

/// Specializes every compatible opaque overload, preserving value signatures for naga.
pub(super) fn specialize_opaque_params(
    items: Vec<Item>,
    types: &HashSet<String>,
) -> Result<Vec<Item>, TransformError> {
    let globals: HashMap<String, String> = items
        .iter()
        .filter_map(|item| match item {
            Item::Decl(d, _) if d.has("uniform") && is_opaque(&d.ty) => {
                Some(d.decls.iter().map(|decl| (decl.0.clone(), d.ty.clone())))
            }
            _ => None,
        })
        .flatten()
        .collect();
    let mut templates: Vec<Template> = Vec::new();
    let mut output = Vec::new();
    for item in items {
        if let Item::Func { name, toks } = &item
            && let Some((_, params)) = func_params(toks)
            && params.iter().any(|p| p.opaque)
        {
            templates.push(Template {
                name: name.clone(),
                toks: toks.clone(),
                params,
                anchor: output.len(),
            });
        } else if let Item::Other(toks) = &item
            && func_params(toks).is_some_and(|(_, params)| params.iter().any(|p| p.opaque))
        {
            // The generated definitions replace resource-helper forward declarations too.
        } else {
            output.push(item);
        }
    }
    // Source anchor of every output item, nondecreasing: instances are inserted in order.
    let mut origin: Vec<usize> = (0..output.len()).collect();
    let mut created = HashSet::new();
    let mut pending = Vec::new();
    for _ in 0..MAX_ROUNDS {
        for item in &mut output {
            rewrite_calls(
                item,
                &templates,
                &globals,
                types,
                &mut created,
                &mut pending,
            )?;
        }
        if pending.is_empty() {
            return Ok(output);
        }
        // Each instance takes its template's place, or the place of its first caller when
        // that comes earlier (a caller instance, or a caller after a forward declaration).
        for instance in pending.drain(..) {
            let template = &templates[instance.template];
            let name = instance_name(&template.name, &instance.resources);
            let own = origin.partition_point(|o| *o < template.anchor);
            let at = output[..own]
                .iter()
                .position(|item| calls(item, &name))
                .unwrap_or(own);
            let anchor = origin.get(at).copied().unwrap_or(template.anchor);
            output.insert(at, instantiate(template, &instance, types)?);
            origin.insert(at, anchor.min(template.anchor));
        }
    }
    Err(TransformError::new(
        "opaque helper nesting exceeds the translation limit",
        0,
    ))
}

/// Whether `item` calls the function `name`.
fn calls(item: &Item, name: &str) -> bool {
    let toks = match item {
        Item::Func { toks, .. } | Item::Decl(_, toks) | Item::Other(toks) => toks,
    };
    toks.windows(2)
        .any(|pair| pair[0].ident() == Some(name) && pair[1].is("("))
}

/// Rewrites calls using exact opaque argument types, retaining all matching value overloads.
fn rewrite_calls(
    item: &mut Item,
    templates: &[Template],
    globals: &HashMap<String, String>,
    types: &HashSet<String>,
    created: &mut HashSet<Instance>,
    pending: &mut Vec<Instance>,
) -> Result<(), TransformError> {
    let (toks, start) = match item {
        Item::Func { toks, .. } => {
            let start = toks.iter().position(|t| t.is("{")).unwrap_or(toks.len());
            (toks, start)
        }
        Item::Decl(_, toks) | Item::Other(toks) => (toks, 0),
    };
    let params = func_params(toks).map(|(_, p)| p).unwrap_or_default();
    let mut i = start;
    while i < toks.len() {
        let Some(callee) = toks[i].ident() else {
            i += 1;
            continue;
        };
        if !templates.iter().any(|t| t.name == callee)
            || !toks.get(i + 1).is_some_and(|t| t.is("("))
            || (i > 0 && toks[i - 1].is("."))
        {
            i += 1;
            continue;
        }
        let close = matching(toks, i + 1, "(", ")")
            .ok_or_else(|| TransformError::new("unclosed helper call", toks[i].line))?;
        let args = split_args(&toks[i + 2..close]);
        let bindings = Bindings::new(&toks[start..], &params, types);
        let mut matches = Vec::new();
        for (index, template) in templates.iter().enumerate() {
            if template.name != callee || template.params.len() != args.len() {
                continue;
            }
            let mut resources = Vec::new();
            let mut arg_start = i + 2;
            let mut compatible = true;
            for (position, (param, arg)) in template.params.iter().zip(&args).enumerate() {
                if param.opaque {
                    let name = (arg.len() == 1).then(|| arg[0].ident()).flatten();
                    if let Some(name) = name.filter(|name| {
                        globals.get(*name) == Some(&param.ty)
                            && (matches!(arg[0].kind, TokKind::BoundResource { .. })
                                || bindings.global(name, arg_start - start))
                    }) {
                        resources.push((position, name.to_string()));
                    } else {
                        compatible = false;
                        break;
                    }
                } else if arg.len() == 1
                    && arg[0].ident().is_some_and(|name| {
                        globals.contains_key(name)
                            && (matches!(arg[0].kind, TokKind::BoundResource { .. })
                                || bindings.global(name, arg_start - start))
                    })
                {
                    compatible = false;
                    break;
                }
                arg_start += arg.len() + 1;
            }
            if compatible {
                matches.push(Instance {
                    template: index,
                    resources,
                });
            }
        }
        let Some(first) = matches.first() else {
            // Value-only overloads keep their original calls and naga resolves their types.
            i += 1;
            continue;
        };
        if matches
            .iter()
            .any(|instance| instance.resources != first.resources)
        {
            return Err(TransformError::new(
                format!("ambiguous opaque arguments to `{callee}`"),
                toks[i].line,
            ));
        }
        let name = instance_name(&templates[first.template].name, &first.resources);
        let line = toks[i].line;
        let retained: Vec<_> = args
            .into_iter()
            .enumerate()
            .filter(|(position, _)| !first.resources.iter().any(|(p, _)| p == position))
            .collect();
        let mut replacement = vec![id(&name).at(line), p("(").at(line)];
        for (j, (_, arg)) in retained.into_iter().enumerate() {
            if j != 0 {
                replacement.push(p(",").at(line));
            }
            replacement.extend(arg);
        }
        replacement.push(p(")").at(line));
        toks.splice(i..=close, replacement);
        for instance in matches {
            if created.insert(instance.clone()) {
                pending.push(instance);
            }
        }
        i += 1;
    }
    Ok(())
}

/// Names a resource-bound overload family; value parameter types remain overloaded normally.
fn instance_name(name: &str, resources: &[(usize, String)]) -> String {
    let mut out = format!("__shaderpack_fn_{}_{name}", name.len());
    for (position, resource) in resources {
        out.push_str(&format!("_{position}_{}_{}", resource.len(), resource));
    }
    out
}

/// Copies a stable template, substituting only references bound to its opaque parameters.
fn instantiate(
    template: &Template,
    instance: &Instance,
    types: &HashSet<String>,
) -> Result<Item, TransformError> {
    let toks = &template.toks;
    let (lp, _) =
        func_params(toks).ok_or_else(|| TransformError::new("invalid helper parameters", 0))?;
    let brace = toks
        .iter()
        .position(|t| t.is("{"))
        .ok_or_else(|| TransformError::new("helper has no body", 0))?;
    let name = instance_name(&template.name, &instance.resources);
    let mut out = toks[..lp - 1].to_vec();
    out.extend([id(&name), p("(")]);
    let mut first = true;
    for param in template.params.iter().filter(|p| !p.opaque) {
        if !first {
            out.push(p(","));
        }
        first = false;
        out.extend(param.toks.iter().cloned());
    }
    out.push(p(")"));
    let bindings = Bindings::new(&toks[brace..], &template.params, types);
    for (i, token) in toks[brace..].iter().enumerate() {
        let resource = token
            .ident()
            .filter(|_| i == 0 || !toks[brace + i - 1].is("."))
            .filter(|name| !bindings.local(name, i))
            .and_then(|name| {
                instance
                    .resources
                    .iter()
                    .find(|(position, _)| template.params[*position].name == name)
            });
        if let Some((_, resource)) = resource {
            out.push(bound_resource(resource).at(token.line));
        } else {
            out.push(token.clone());
        }
    }
    Ok(Item::Func { name, toks: out })
}
