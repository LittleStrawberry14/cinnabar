//! Parent-exit barriers used only by the offline helper process.
use anyhow::{Result, ensure};

/// Waits for the pipe held by the client and for its actual process exit before installing.
pub fn wait(pid: u32) -> Result<()> {
    wait_with_monitor(pid, None)
}

/// Also waits for the client's Windows crash monitor, which uses the same installed image.
pub fn wait_with_monitor(pid: u32, monitor: Option<u32>) -> Result<()> {
    ensure!(
        pid != 0 && pid != std::process::id(),
        "invalid updater parent"
    );
    #[cfg(windows)]
    return windows_wait(pid, monitor);
    #[cfg(unix)]
    {
        ensure!(monitor.is_none(), "unexpected update monitor on Unix");
        use std::io::Read;
        std::io::stdin().take(1).read_to_end(&mut Vec::new())?;
        while rustix::process::getppid()
            .is_some_and(|parent| parent.as_raw_nonzero().get() as u32 == pid)
        {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    anyhow::bail!("unsupported parent wait")
}

#[cfg(windows)]
/// Holds a process handle, so PID reuse cannot let the helper replace a live parent.
fn windows_wait(pid: u32, monitor: Option<u32>) -> Result<()> {
    use std::io::Read;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, HANDLE, WAIT_OBJECT_0},
        System::Threading::{INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };
    struct Process(HANDLE);
    impl Drop for Process {
        /// Closes the owned synchronization handle on success and on every error path.
        fn drop(&mut self) {
            // SAFETY: this handle came from a successful OpenProcess and is owned here.
            unsafe { CloseHandle(self.0) };
        }
    }
    exit_barrier(
        std::iter::once(pid).chain(monitor),
        |pid| {
            ensure!(
                pid != 0 && pid != std::process::id(),
                "invalid updater process"
            );
            // Open both before reading the exit pipe, so subsequent PID reuse cannot change either wait.
            // SAFETY: opens an owned handle with synchronization rights only.
            let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
            if handle.is_null() {
                // SAFETY: reads the error from the immediately preceding failed OpenProcess.
                ensure!(
                    unsafe { GetLastError() } == ERROR_INVALID_PARAMETER,
                    "cannot open updater process"
                );
                Ok(None)
            } else {
                Ok(Some(Process(handle)))
            }
        },
        || {
            std::io::stdin().take(1).read_to_end(&mut Vec::new())?;
            Ok(())
        },
        |process| {
            // SAFETY: this synchronization handle remains owned until the wait finishes.
            ensure!(
                unsafe { WaitForSingleObject(process.0, INFINITE) } == WAIT_OBJECT_0,
                "waiting for updater process failed"
            );
            Ok(())
        },
    )
}

/// Captures stable process identities before the pipe closes, then waits for every image to unload.
#[cfg(any(windows, test))]
fn exit_barrier<T>(
    pids: impl Iterator<Item = u32>,
    open: impl FnMut(u32) -> Result<Option<T>>,
    pipe_closed: impl FnOnce() -> Result<()>,
    mut wait: impl FnMut(T) -> Result<()>,
) -> Result<()> {
    let processes = pids.map(open).collect::<Result<Vec<_>>>()?;
    pipe_closed()?;
    for process in processes.into_iter().flatten() {
        wait(process)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn client_and_monitor_handles_precede_the_exit_pipe_and_both_exits_precede_installation() {
        let events = RefCell::new(Vec::new());
        exit_barrier(
            [10, 20].into_iter(),
            |pid| {
                events.borrow_mut().push(("open", pid));
                Ok(Some(pid))
            },
            || {
                events.borrow_mut().push(("pipe closed", 0));
                Ok(())
            },
            |pid| {
                events.borrow_mut().push(("exited", pid));
                Ok(())
            },
        )
        .unwrap();
        events.borrow_mut().push(("install", 0));
        assert_eq!(
            *events.borrow(),
            [
                ("open", 10),
                ("open", 20),
                ("pipe closed", 0),
                ("exited", 10),
                ("exited", 20),
                ("install", 0),
            ]
        );
    }

    #[test]
    fn a_missing_process_is_already_gone_but_a_failed_wait_blocks_installation() {
        let mut waited = Vec::new();
        assert!(
            exit_barrier(
                [10, 20, 30].into_iter(),
                |pid| Ok((pid != 10).then_some(pid)),
                || Ok(()),
                |pid| {
                    waited.push(pid);
                    anyhow::bail!("cannot observe process exit")
                }
            )
            .is_err()
        );
        assert_eq!(waited, [20]);
    }
}
