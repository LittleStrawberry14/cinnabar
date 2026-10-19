use bevy::{ecs::world::CommandQueue, prelude::World};
use {super::*, launcher::menu::auth::AuthState};

fn child_guard(exited: bool) -> CoreProcessGuard {
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("cmd.exe");
        command.args(["/Q", "/C", if exited { "exit 0" } else { "set /p hold=" }]);
        command
    };
    #[cfg(unix)]
    let mut command = {
        let mut command = Command::new("sh");
        command.args(["-c", if exited { "exit 0" } else { "read -r hold" }]);
        command
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    if exited {
        assert!(child.wait().unwrap().success());
    }
    let mut guard = CoreProcessGuard::default();
    guard.replace(child);
    guard
}

fn fixture(
    exited: bool,
    runtime: &tokio::runtime::Handle,
    tasks: &tokio_util::task::TaskTracker,
) -> (LauncherCoreSlot, MenuRuntime, World, PathBuf) {
    let layout = launcher::test_support::scratch("launcher-child-recovery");
    let socket_dir = layout.account_socket_dir(std::process::id(), 0);
    let directory = SessionDirectoryGuard::bind(socket_dir.clone()).unwrap();
    std::fs::write(bridge::session_endpoint_path(&socket_dir), []).unwrap();
    #[cfg(unix)]
    std::fs::write(bridge::control_endpoint_path(&socket_dir), []).unwrap();
    let slot = LauncherCoreSlot {
        core: Some(LauncherCore {
            _guard: child_guard(exited),
            _directory: directory,
            socket_dir: socket_dir.clone(),
            authenticated: false,
            auth_cache: None,
            title_presence_owner: TitlePresenceOwner::Core,
            attached: true,
        }),
        failed: None,
        ..Default::default()
    };
    let mut menu = MenuRuntime::new_with_layout(
        true,
        Some(2),
        "Fixture".into(),
        layout,
        crate::player_skin::LocalPlayerSkin::generated_default("Fixture"),
    );
    menu = menu.with_io(runtime.clone(), tasks.clone());
    menu.control_auth = Some(AuthState::SignedOut);
    let mut world = World::new();
    world.insert_resource(LauncherAccount(
        launcher_host::launcher_account::LauncherAccount::new(
            socket_dir.clone(),
            socket_dir.join("artwork"),
            runtime.clone(),
            tasks.clone(),
        ),
    ));
    (slot, menu, world, socket_dir)
}

/// Owns the shared reactor used by actual account workers in this fixture.
fn worker_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
}

/// Retires the real fixture owners before observing their registered worker completion.
fn finish_workers(
    runtime: &tokio::runtime::Runtime,
    tasks: tokio_util::task::TaskTracker,
    slot: LauncherCoreSlot,
    menu: MenuRuntime,
    world: World,
) {
    drop(world);
    drop(menu);
    drop(slot);
    tasks.close();
    runtime.block_on(tasks.wait());
}

/// Past the first restart delay after a quick exit.
const AFTER_FIRST_DELAY: Duration = Duration::from_secs(1);

fn drive(slot: &mut LauncherCoreSlot, menu: &mut MenuRuntime, world: &mut World, idle: bool) {
    drive_at(slot, menu, world, idle, Instant::now());
}

fn drive_at(
    slot: &mut LauncherCoreSlot,
    menu: &mut MenuRuntime,
    world: &mut World,
    idle: bool,
    now: Instant,
) {
    let mut queue = CommandQueue::default();
    slot.drive(
        &mut Commands::new(&mut queue, world),
        menu,
        idle,
        false,
        None,
        now,
    );
    queue.apply(world);
}

#[test]
fn dead_child_detaches_clients_during_play_and_retries_unchanged_auth_when_idle() {
    let runtime = worker_runtime();
    let tasks = tokio_util::task::TaskTracker::new();
    let (mut slot, mut menu, mut world, socket_dir) = fixture(true, runtime.handle(), &tasks);
    // A failed-mode latch must not suppress recovery of a child that subsequently existed.
    slot.failed = Some(false);
    let exited_at = Instant::now();
    drive_at(&mut slot, &mut menu, &mut world, false, exited_at);
    assert!(
        slot.core.is_none(),
        "a dead child cannot serve another join"
    );
    assert!(slot.failed.is_none());
    assert!(!world.contains_resource::<LauncherAccount>());
    assert!(menu.control_auth.is_none());
    assert!(
        !socket_dir.exists(),
        "the dead core's stale endpoint is reclaimed"
    );
    assert!(!menu.layout.core_executable.exists());

    drive_at(&mut slot, &mut menu, &mut world, true, exited_at);
    assert!(
        slot.failed.is_none(),
        "a core that just exited is not restarted before its backoff delay"
    );
    drive_at(
        &mut slot,
        &mut menu,
        &mut world,
        true,
        exited_at + AFTER_FIRST_DELAY,
    );
    assert_eq!(
        slot.failed,
        Some(false),
        "idle must attempt the same auth mode again"
    );
    finish_workers(&runtime, tasks, slot, menu, world);
}

#[test]
fn healthy_child_and_account_client_survive_idle_and_active_frames() {
    let runtime = worker_runtime();
    let tasks = tokio_util::task::TaskTracker::new();
    let (mut slot, mut menu, mut world, socket_dir) = fixture(false, runtime.handle(), &tasks);
    let pid = slot.core.as_ref().unwrap()._guard.id();
    for idle in [false, true, false, true] {
        drive(&mut slot, &mut menu, &mut world, idle);
        assert_eq!(slot.core.as_ref().unwrap()._guard.id(), pid);
        assert!(world.contains_resource::<LauncherAccount>());
        assert_eq!(menu.control_auth, Some(AuthState::SignedOut));
        assert!(socket_dir.exists());
        assert!(slot.failed.is_none());
    }
    finish_workers(&runtime, tasks, slot, menu, world);
}

/// Installs a core executable that exits as soon as it starts and offers no IPC.
fn install_exiting_core(menu: &MenuRuntime) {
    let executable = &menu.layout.core_executable;
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    #[cfg(windows)]
    std::fs::copy(
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/where.exe"),
        executable,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn dead_child_is_replaced_without_a_sign_in_change_and_waits_for_new_readiness() {
    let runtime = worker_runtime();
    let tasks = tokio_util::task::TaskTracker::new();
    let (mut slot, mut menu, mut world, socket_dir) = fixture(true, runtime.handle(), &tasks);
    let old_pid = slot.core.as_ref().unwrap()._guard.id();
    // A harmless process fixture proves production spawn/adoption.
    install_exiting_core(&menu);
    let exited_at = Instant::now();
    drive_at(&mut slot, &mut menu, &mut world, true, exited_at);
    assert!(slot.core.is_none(), "the restart waits for its backoff");
    drive_at(
        &mut slot,
        &mut menu,
        &mut world,
        true,
        exited_at + AFTER_FIRST_DELAY,
    );
    let core = slot
        .core
        .as_ref()
        .expect("same-mode dead child must restart");
    assert_ne!(core._guard.id(), old_pid);
    assert!(!core.authenticated);
    assert!(
        !core.attached,
        "old readiness must not attach clients to the replacement"
    );
    assert!(slot.failed.is_none());
    assert!(!world.contains_resource::<LauncherAccount>());
    assert!(!bridge::bridge_endpoint_exists(&socket_dir));
    #[cfg(unix)]
    assert!(!bridge::control_endpoint_path(&socket_dir).exists());
    finish_workers(&runtime, tasks, slot, menu, world);
}

#[test]
fn a_core_that_keeps_exiting_quickly_is_given_up_on_until_a_refresh() {
    let runtime = worker_runtime();
    let tasks = tokio_util::task::TaskTracker::new();
    let (mut slot, mut menu, mut world, _) = fixture(true, runtime.handle(), &tasks);
    install_exiting_core(&menu);
    // Past the longest delay, but each run stays far below the stable run that resets it.
    let step = Duration::from_secs(9);
    let mut now = Instant::now();
    let mut restarts = 0;
    loop {
        drive_at(&mut slot, &mut menu, &mut world, true, now);
        assert!(slot.core.is_none(), "an exited core is retired");
        if slot.failed == Some(false) {
            break;
        }
        now += step;
        drive_at(&mut slot, &mut menu, &mut world, true, now);
        let core = slot.core.as_mut().expect("restarted after its delay");
        restarts += 1;
        test_time::eventually("the restarted core to exit", || {
            core._guard.exit_status().is_some()
        });
    }
    assert_eq!(
        restarts, 5,
        "five quick restarts, then the backoff gives up"
    );
    drive_at(&mut slot, &mut menu, &mut world, true, now + step * 10);
    assert!(slot.core.is_none(), "no restart after giving up");
    menu.feeds.profile_refresh_requested = true;
    drive_at(&mut slot, &mut menu, &mut world, true, now + step * 10);
    assert!(slot.core.is_some(), "a refresh retries the core");
    finish_workers(&runtime, tasks, slot, menu, world);
}

#[test]
fn a_new_account_retires_the_authenticated_core_even_when_sign_in_mode_is_unchanged() {
    let runtime = worker_runtime();
    let tasks = tokio_util::task::TaskTracker::new();
    let (mut slot, mut menu, mut world, _) = fixture(false, runtime.handle(), &tasks);
    let core = slot.core.as_mut().unwrap();
    core.authenticated = true;
    core.auth_cache = Some(menu.layout.auth_cache());
    menu.feeds.account_adding = true;
    menu.accounts.pending_ready = true;
    drive(&mut slot, &mut menu, &mut world, false);
    assert!(
        slot.core.is_some(),
        "active play keeps its owning account core"
    );
    drive(&mut slot, &mut menu, &mut world, true);
    assert!(slot.core.is_none());
    assert!(!world.contains_resource::<LauncherAccount>());
    assert!(
        menu.accounts.skip_control,
        "old profile data must not enter the new account"
    );
    // The retire signal belongs to `drive`; taking it here races the sender's drop.
    test_time::eventually_within(Duration::from_secs(5), "the retired core to stop", || {
        if slot.retiring.is_none() {
            return true;
        }
        drive(&mut slot, &mut menu, &mut world, true);
        false
    });
    assert_eq!(slot.failed, Some(true));
    finish_workers(&runtime, tasks, slot, menu, world);
}

#[test]
fn account_switch_waits_for_the_previous_profiles_credential_save() {
    let runtime = worker_runtime();
    let tasks = tokio_util::task::TaskTracker::new();
    let (mut slot, mut menu, mut world, _) = fixture(false, runtime.handle(), &tasks);
    let old_pid = slot.core.as_ref().unwrap()._guard.id();
    let (saved, pending) = crossbeam_channel::bounded(1);
    menu.accounts.remember = Some(pending);
    menu.accounts.operation = Some(launcher_host::accounts::Operation::Switch("2".into()));
    drive(&mut slot, &mut menu, &mut world, true);
    assert_eq!(slot.core.as_ref().unwrap()._guard.id(), old_pid);
    assert!(menu.accounts.operation.is_some());
    assert!(menu.accounts.work.is_none());
    drop(saved);
    menu.poll_accounts();
    assert!(
        menu.accounts.remember.is_none(),
        "an interrupted saver cannot stall account changes"
    );
    drive(&mut slot, &mut menu, &mut world, true);
    assert!(slot.core.is_none());
    assert!(menu.accounts.work.is_some());
    finish_workers(&runtime, tasks, slot, menu, world);
}

#[test]
fn realm_membership_core_exit_retires_the_pending_dialog() {
    use launcher::menu::realm_membership::{Stage, State};
    for stage in [Stage::Verifying, Stage::Joining] {
        let runtime = worker_runtime();
        let tasks = tokio_util::task::TaskTracker::new();
        let (mut slot, mut menu, mut world, _) = fixture(true, runtime.handle(), &tasks);
        menu.control_auth = Some(AuthState::Authenticated);
        menu.realm_membership.state = Some(State {
            stage,
            ..Default::default()
        });
        drive(&mut slot, &mut menu, &mut world, false);
        assert!(!world.contains_resource::<LauncherAccount>());
        assert!(
            menu.realm_membership.state.is_none(),
            "a detached account control cannot complete the dialog"
        );
        finish_workers(&runtime, tasks, slot, menu, world);
    }
}

#[test]
fn presence_policy_change_retires_the_actual_core_before_replacement() {
    let runtime = worker_runtime();
    let tasks = tokio_util::task::TaskTracker::new();
    let (mut slot, mut menu, mut world, _) = fixture(false, runtime.handle(), &tasks);
    install_exiting_core(&menu);
    let original = slot.core.as_ref().unwrap()._guard.id();
    slot.set_title_presence_owner(TitlePresenceOwner::Native);
    drive(&mut slot, &mut menu, &mut world, false);
    assert_eq!(slot.core.as_ref().unwrap()._guard.id(), original);
    assert_eq!(slot.title_presence_owner(), Some(TitlePresenceOwner::Core));
    drive(&mut slot, &mut menu, &mut world, true);
    assert!(slot.core.is_none());
    assert!(slot.retiring.is_some());
    assert!(!world.contains_resource::<LauncherAccount>());
    test_time::eventually_within(
        Duration::from_secs(5),
        "the old presence core to retire",
        || {
            drive(&mut slot, &mut menu, &mut world, true);
            slot.core.is_some()
        },
    );
    assert_eq!(
        slot.last_core_retirement(),
        Some(CoreStopOutcome::ExitedAfterGracefulClose)
    );
    assert_eq!(
        slot.title_presence_owner(),
        Some(TitlePresenceOwner::Native)
    );
    assert_ne!(slot.core.as_ref().unwrap()._guard.id(), original);
    finish_workers(&runtime, tasks, slot, menu, world);
}

#[test]
fn unconfirmed_retirement_holds_replacement_even_after_policy_changes() {
    for outcome in [Some(CoreStopOutcome::Unreaped), None] {
        let mut slot = LauncherCoreSlot::default();
        let (done, retired) = crossbeam_channel::bounded(1);
        slot.retiring = Some(retired);
        if let Some(outcome) = outcome {
            done.send(outcome).unwrap();
        }
        drop(done);
        assert!(!slot.retirement_complete());
        assert_eq!(slot.last_core_retirement(), outcome);
        slot.set_title_presence_owner(TitlePresenceOwner::Native);
        assert!(!slot.retirement_complete());
        slot.set_title_presence_owner(TitlePresenceOwner::Core);
        assert!(!slot.retirement_complete());
        assert!(slot.core.is_none());
    }
}
