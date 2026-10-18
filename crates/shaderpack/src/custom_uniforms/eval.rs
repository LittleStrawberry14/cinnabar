//! Per-frame evaluation of compiled [`Node`] trees, including OptiFine's `smooth()`
//! filter and the deterministic generator behind `random()`.

use std::f64::consts::PI;

use crate::BinaryOp;

use super::compile::{Access, Fold, Func1, Func2, Func3, Node, SmoothNode};
use super::value::{Mismatch, Value};

/// Fade time in seconds of a `smooth()` call that gives none (OptiFine default).
const DEFAULT_FADE_SECONDS: f64 = 1.0;
/// OptiFine `SmoothFloat` constants: with `n = fade / dt` steps per fade time, each step
/// moves `dt / fade * (K1 - 1 / (K2 + n / K3))` of the way to the target, which reaches
/// ~99% of a step change after the fade time (`K1` ≈ ln 100).
const SMOOTH_K1: f64 = 4.61;
const SMOOTH_K2: f64 = 0.13;
const SMOOTH_K3: f64 = 10.0;
/// Differences at or below this snap straight to the target (OptiFine).
const SMOOTH_SNAP: f64 = 1.0e-6;
/// Initial state of the `random()` generator, so evaluation is reproducible.
pub(super) const RANDOM_SEED: u64 = 0x5EED_C0FF_EE15_BEEF;

/// One `smooth()` accumulator.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SmoothState {
    /// The previous smoothed value; `None` until the call first runs.
    last: Option<Value>,
    /// Seconds since the call last ran (frames where it sat in an untaken branch add up,
    /// matching OptiFine's wall-clock timestamps).
    pub(super) elapsed: f64,
}

/// Everything a definition's evaluation reads or updates.
pub(super) struct Frame<'a> {
    /// This frame's built-in values by input slot; `None` reads as 0.
    pub(super) inputs: &'a [Option<Value>],
    /// Values of the definitions before the one being evaluated.
    pub(super) values: &'a [Value],
    pub(super) smooth: &'a mut [SmoothState],
    pub(super) rng: &'a mut u64,
}

type Eval = Result<Value, Mismatch>;

impl Node {
    /// Evaluates this node; only the taken branch of `if`, `&&` and `||` runs.
    pub(super) fn eval(&self, frame: &mut Frame<'_>) -> Eval {
        match self {
            Node::Const(value) => Ok(*value),
            Node::Input { slot, access } => {
                let input = *frame.inputs.get(*slot).ok_or(Mismatch)?;
                let Some(value) = input else {
                    return Ok(Value::Num(0.0));
                };
                match access {
                    Access::Whole => Ok(value),
                    Access::Component(index) => component(value, *index),
                    Access::Scalar => number(value).map(Value::Num),
                }
            }
            Node::Local {
                index,
                component: part,
            } => {
                let value = *frame.values.get(*index).ok_or(Mismatch)?;
                match part {
                    None => Ok(value),
                    Some(index) => component(value, *index),
                }
            }
            Node::Neg(operand) => Ok(operand.eval(frame)?.map(|n| -n)),
            Node::Not(operand) => Ok(Value::Bool(!truth(operand.eval(frame)?)?)),
            Node::Binary(op, lhs, rhs) => binary(*op, lhs, rhs, frame),
            Node::If {
                branches,
                otherwise,
            } => {
                for (condition, value) in branches {
                    if truth(condition.eval(frame)?)? {
                        return value.eval(frame);
                    }
                }
                otherwise.eval(frame)
            }
            Node::Construct(items) => {
                let mut lanes = [0.0; 4];
                for (lane, item) in lanes.iter_mut().zip(items) {
                    *lane = number(item.eval(frame)?)?;
                }
                let len = u8::try_from(items.len()).map_err(|_| Mismatch)?;
                Ok(super::value::from_lanes(Some(len), lanes))
            }
            Node::Map1(func, operand) => Ok(operand.eval(frame)?.map(|n| apply1(*func, n))),
            Node::Map2(func, args) => {
                let [a, b] = &**args;
                a.eval(frame)?
                    .zip(b.eval(frame)?, |x, y| apply2(*func, x, y))
            }
            Node::Map3(func, args) => {
                let [a, b, c] = &**args;
                let (a, b, c) = (a.eval(frame)?, b.eval(frame)?, c.eval(frame)?);
                Value::zip3(a, b, c, |x, y, z| apply3(*func, x, y, z))
            }
            Node::Fold(fold, items) => {
                let mut items = items.iter();
                let first = items.next().ok_or(Mismatch)?.eval(frame)?;
                items.try_fold(first, |acc, item| {
                    acc.zip(item.eval(frame)?, |x, y| match fold {
                        Fold::Min => x.min(y),
                        Fold::Max => x.max(y),
                    })
                })
            }
            Node::Between(args) => {
                let [x, min, max] = numbers(args, frame)?;
                Ok(Value::Bool((min..=max).contains(&x)))
            }
            Node::Equals(args) => {
                let [a, b, epsilon] = numbers(args, frame)?;
                Ok(Value::Bool((a - b).abs() <= epsilon))
            }
            Node::In(value, candidates) => {
                let value = number(value.eval(frame)?)?;
                for candidate in candidates {
                    if number(candidate.eval(frame)?)? == value {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            Node::Smooth(smooth) => smooth.eval(frame),
            Node::Random(range) => {
                let bounds = match range {
                    Some(args) => Some(numbers(args, frame)?),
                    None => None,
                };
                let unit = next_unit(frame.rng);
                Ok(Value::Num(match bounds {
                    Some([min, max]) => min + unit * (max - min),
                    None => unit,
                }))
            }
            Node::RandomInt(range) => {
                let bounds = match range {
                    Some(args) => Some(numbers(args, frame)?),
                    None => None,
                };
                let bits = next_u64(frame.rng);
                Ok(Value::Num(match bounds {
                    // Java `(int)` casts, then a value in [min, max - 1] (Iris).
                    Some([min, max]) => {
                        let (min, max) = (i64::from(min as i32), i64::from(max as i32));
                        if max <= min {
                            min as f64
                        } else {
                            (min + (bits % (max - min).unsigned_abs()) as i64) as f64
                        }
                    }
                    None => f64::from(bits as u32 as i32),
                }))
            }
        }
    }
}

impl SmoothNode {
    /// OptiFine `Smoother.getSmoothValue`: the first call returns its value; later calls
    /// move the previous result toward the value using the fade-up time when rising and
    /// the fade-down time when falling (defaulting to the fade-up time, itself 1 s).
    /// Vectors smooth per component; a change of shape restarts from the value.
    fn eval(&self, frame: &mut Frame<'_>) -> Eval {
        let value = self.value.eval(frame)?.to_numeric();
        let fade_up = match &self.fade_up {
            Some(fade) => number(fade.eval(frame)?)?,
            None => DEFAULT_FADE_SECONDS,
        };
        let fade_down = match &self.fade_down {
            Some(fade) => number(fade.eval(frame)?)?,
            None => fade_up,
        };
        let state = frame.smooth.get_mut(self.slot).ok_or(Mismatch)?;
        let elapsed = std::mem::take(&mut state.elapsed);
        let smoothed = match state.last {
            Some(previous) if previous.lane_count() == value.lane_count() => previous
                .zip(value, |previous, target| {
                    smooth_step(previous, target, elapsed, fade_up, fade_down)
                })?,
            _ => value,
        };
        state.last = Some(smoothed);
        Ok(smoothed)
    }
}

/// One OptiFine `SmoothFloat` step from `previous` toward `target` after `elapsed`
/// seconds. No time passed keeps `previous`; a fade time of 0 or shorter than the step
/// jumps to `target`.
fn smooth_step(previous: f64, target: f64, elapsed: f64, fade_up: f64, fade_down: f64) -> f64 {
    if elapsed <= 0.0 {
        return previous;
    }
    let fade = if target >= previous {
        fade_up
    } else {
        fade_down
    };
    let delta = target - previous;
    if fade > 0.0 && elapsed < fade && delta.abs() > SMOOTH_SNAP {
        let steps = fade / elapsed;
        let correction = SMOOTH_K1 - 1.0 / (SMOOTH_K2 + steps / SMOOTH_K3);
        let fraction = (elapsed / fade * correction).clamp(0.0, 1.0);
        previous + delta * fraction
    } else {
        target
    }
}

/// Evaluates a binary operator; `&&` and `||` short-circuit.
fn binary(op: BinaryOp, lhs: &Node, rhs: &Node, frame: &mut Frame<'_>) -> Eval {
    let l = lhs.eval(frame)?;
    match op {
        BinaryOp::And => Ok(Value::Bool(truth(l)? && truth(rhs.eval(frame)?)?)),
        BinaryOp::Or => Ok(Value::Bool(truth(l)? || truth(rhs.eval(frame)?)?)),
        BinaryOp::Mul => l.zip(rhs.eval(frame)?, |a, b| a * b),
        BinaryOp::Div => l.zip(rhs.eval(frame)?, |a, b| a / b),
        BinaryOp::Rem => l.zip(rhs.eval(frame)?, |a, b| a % b),
        BinaryOp::Add => l.zip(rhs.eval(frame)?, |a, b| a + b),
        BinaryOp::Sub => l.zip(rhs.eval(frame)?, |a, b| a - b),
        BinaryOp::Lt => compare(l, rhs.eval(frame)?, |a, b| a < b),
        BinaryOp::Le => compare(l, rhs.eval(frame)?, |a, b| a <= b),
        BinaryOp::Gt => compare(l, rhs.eval(frame)?, |a, b| a > b),
        BinaryOp::Ge => compare(l, rhs.eval(frame)?, |a, b| a >= b),
        BinaryOp::Eq => l.equals(rhs.eval(frame)?).map(Value::Bool),
        BinaryOp::Ne => l.equals(rhs.eval(frame)?).map(|equal| Value::Bool(!equal)),
    }
}

/// An ordering comparison of two scalars.
fn compare(lhs: Value, rhs: Value, op: impl Fn(f64, f64) -> bool) -> Eval {
    Ok(Value::Bool(op(number(lhs)?, number(rhs)?)))
}

/// `value` as a number, or a mismatch for a vector.
fn number(value: Value) -> Result<f64, Mismatch> {
    value.number().ok_or(Mismatch)
}

/// `value` as a condition, or a mismatch for a vector.
fn truth(value: Value) -> Result<bool, Mismatch> {
    value.truth().ok_or(Mismatch)
}

/// Component `index` of a vector, or a mismatch for a scalar or a missing component.
fn component(value: Value, index: usize) -> Eval {
    value.component(index).map(Value::Num).ok_or(Mismatch)
}

/// Evaluates `N` scalar arguments in order.
fn numbers<const N: usize>(args: &[Node; N], frame: &mut Frame<'_>) -> Result<[f64; N], Mismatch> {
    let mut out = [0.0; N];
    for (slot, arg) in out.iter_mut().zip(args) {
        *slot = number(arg.eval(frame)?)?;
    }
    Ok(out)
}

/// Applies a one-argument function to one component.
fn apply1(func: Func1, x: f64) -> f64 {
    match func {
        Func1::Sin => x.sin(),
        Func1::Cos => x.cos(),
        Func1::Tan => x.tan(),
        Func1::Asin => x.asin(),
        Func1::Acos => x.acos(),
        Func1::Atan => x.atan(),
        Func1::Radians => x * (PI / 180.0),
        Func1::Degrees => x * (180.0 / PI),
        Func1::Exp => x.exp(),
        Func1::Exp2 => x.exp2(),
        Func1::Exp10 => 10.0_f64.powf(x),
        Func1::Ln => x.ln(),
        Func1::Log2 => x.log2(),
        Func1::Log10 => x.log10(),
        Func1::Sqrt => x.sqrt(),
        Func1::Abs => x.abs(),
        // Java `Math.signum`: keeps ±0 and NaN.
        Func1::Sign => {
            if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                x
            }
        }
        Func1::Floor => x.floor(),
        Func1::Ceil => x.ceil(),
        Func1::Frac => x - x.floor(),
        // Java `Math.round`: halves round up, NaN is 0, saturates to the int range.
        Func1::Round => {
            if x.is_nan() {
                0.0
            } else {
                (x + 0.5)
                    .floor()
                    .clamp(f64::from(i32::MIN), f64::from(i32::MAX))
            }
        }
        // Java `Math.rint`: halves round to even.
        Func1::Rint => x.round_ties_even(),
    }
}

/// Applies a two-argument function to one pair of components.
fn apply2(func: Func2, a: f64, b: f64) -> f64 {
    match func {
        Func2::Pow => a.powf(b),
        Func2::Atan2 => a.atan2(b),
        Func2::Fmod => a - b * (a / b).floor(),
        Func2::Edge => {
            if b < a {
                0.0
            } else {
                1.0
            }
        }
        Func2::LogBase => b.ln() / a.ln(),
    }
}

/// Applies a three-argument function to one triple of components.
fn apply3(func: Func3, a: f64, b: f64, c: f64) -> f64 {
    match func {
        // OptiFine `NumUtils.limit`: the lower bound wins when the bounds cross.
        Func3::Clamp => {
            if a < b {
                b
            } else if a > c {
                c
            } else {
                a
            }
        }
        Func3::Mix => a + (b - a) * c,
    }
}

/// Next output of the SplitMix64 generator.
fn next_u64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A uniformly distributed value in `[0, 1)`.
fn next_unit(state: &mut u64) -> f64 {
    const SCALE: f64 = 1.0 / (1_u64 << 53) as f64;
    (next_u64(state) >> 11) as f64 * SCALE
}
