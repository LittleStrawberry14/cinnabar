//! iOS: no native crash-handler attachment (crate has no apple-ios support).

use super::{Context, LogHooks};

pub(super) fn install(_context: &'static Context, _hooks: LogHooks) {}

pub(super) fn dump_this_thread() -> bool {
    false
}

pub(super) fn allow_dumps_by(_pid: u32) {}
