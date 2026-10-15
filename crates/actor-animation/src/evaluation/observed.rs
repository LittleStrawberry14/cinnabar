//! External reads needed to reuse a pure scratch evaluation without running its bytecode.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
struct QueryRead {
    symbol: u32,
    arguments: Box<[MolangValue]>,
    value: MolangValue,
}

/// The executed branch's input reads, excluding values the expression wrote itself.
#[derive(Debug)]
pub(crate) struct ObservedInputs {
    variables: BTreeMap<u32, Option<MolangValue>>,
    written: BTreeSet<u32>,
    queries: Vec<QueryRead>,
    random: u64,
}

impl ObservedInputs {
    /// Retains the initial random seed because scratch expressions reset it for each owner.
    pub(crate) fn new(variables: &MolangVariables) -> Self {
        Self {
            variables: BTreeMap::new(),
            written: BTreeSet::new(),
            queries: Vec::new(),
            random: variables.random,
        }
    }

    /// Charges each validated input and query argument; exhaustion leaves the result unresolved.
    pub(crate) fn matches(
        &self,
        evaluator: &Evaluator<'_>,
        variables: &MolangVariables,
        operations_left: &mut usize,
    ) -> Option<bool> {
        charge(operations_left, 1)?;
        if self.random != variables.random {
            return Some(false);
        }
        for (&symbol, expected) in &self.variables {
            charge(operations_left, 1)?;
            let Some(Place::Variable(slot)) = evaluator.layout.place(symbol) else {
                return Some(false);
            };
            if variables.values.get(slot) != Some(expected) {
                return Some(false);
            }
        }
        for read in &self.queries {
            charge(operations_left, 1 + read.arguments.len())?;
            if evaluator.query(read.symbol, &read.arguments) != read.value {
                return Some(false);
            }
        }
        Some(true)
    }

    /// Counts stored values so the owner-selection cache has a shared memory bound.
    pub(crate) fn weight(&self) -> usize {
        1 + self.variables.len()
            + self.written.len()
            + self
                .queries
                .iter()
                .map(|read| 1 + read.arguments.len())
                .sum::<usize>()
    }

    /// Captures only inherited persistent slots read before this expression assigns them.
    pub(super) fn variable(&mut self, symbol: u32, place: Place, variables: &MolangVariables) {
        if let Place::Variable(slot) = place
            && !self.written.contains(&symbol)
            && let Some(value) = variables.values.get(slot)
        {
            self.variables
                .entry(symbol)
                .or_insert_with(|| value.clone());
        }
    }

    /// Marks scratch writes so their later reads are not mistaken for inherited inputs.
    pub(super) fn written(&mut self, symbol: u32) {
        self.written.insert(symbol);
    }

    /// Records the exact arguments and value of one query along the executed branch.
    pub(super) fn query(&mut self, symbol: u32, arguments: &[MolangValue], value: &MolangValue) {
        self.queries.push(QueryRead {
            symbol,
            arguments: arguments.into(),
            value: value.clone(),
        });
    }
}

/// Reserves validation work before it runs, consuming the remainder on exhaustion.
fn charge(operations_left: &mut usize, work: usize) -> Option<()> {
    let remaining = operations_left.checked_sub(work);
    *operations_left = remaining.unwrap_or(0);
    remaining.map(|_| ())
}
