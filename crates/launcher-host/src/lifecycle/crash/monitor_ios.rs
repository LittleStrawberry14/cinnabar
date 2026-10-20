//! iOS: no out-of-process minidumper monitor (minidump-writer does not support apple-ios).

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use super::protocol::{Hello, Restart};

/// Present so desktop call sites type-check; never started on iOS.
#[derive(Clone, Debug)]
pub struct MonitorCommand {
    pub program: PathBuf,
    pub args: Vec<std::ffi::OsString>,
    pub stall_after: Duration,
}

impl MonitorCommand {
    pub fn current_exe() -> std::io::Result<Self> {
        Ok(Self {
            program: std::env::current_exe()?,
            args: vec![super::MONITOR_FLAG.into()],
            stall_after: Duration::from_secs(30),
        })
    }
}

pub(super) fn start(_command: &MonitorCommand, _crash_dir: &Path, _hello: &Hello, _restart: Restart) {}

pub(super) fn reconnect() -> bool {
    false
}

pub(super) fn connection() -> Option<()> {
    None
}

pub(super) fn route() -> Option<()> {
    None
}

pub(super) fn announce_dump(_client: &()) -> bool {
    false
}

pub(super) fn send(_kind: u32, _payload: &[u8]) -> bool {
    false
}

pub(super) fn send_on(_client: &(), _kind: u32, _payload: &[u8]) -> bool {
    false
}

pub fn wait_for_connection(_timeout: Duration) -> bool {
    false
}

pub fn pid() -> Option<u32> {
    None
}

pub fn socket() -> Option<&'static Path> {
    None
}
