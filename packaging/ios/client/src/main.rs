//! iOS host: UIApplication entry via Bevy/winit, sandbox Documents layout.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use launcher::install_layout::{InstallLayout, configure_ios};

#[bevy::prelude::bevy_main]
fn main() {
    let log_path = startup_log_path();
    install_panic_logger(log_path.clone());
    log_line(&log_path, "=== Cinnabar iOS startup ===");
    log_line(&log_path, &format!("version={} os={}", env!("CARGO_PKG_VERSION"), std::env::consts::OS));

    let code = match run_client(&log_path) {
        Ok(()) => {
            log_line(&log_path, "client returned normally");
            0
        }
        Err(error) => {
            let message = format!("{} failed: {error:#}", launcher::PRODUCT_NAME);
            log_line(&log_path, &message);
            eprintln!("{message}");
            1
        }
    };
    log_line(&log_path, &format!("exiting with code {code}"));
    std::process::exit(code);
}

fn run_client(log_path: &Option<PathBuf>) -> Result<()> {
    log_line(log_path, "resolving sandbox Documents directory");
    let files_dir = sandbox_files_dir()?;
    fs::create_dir_all(&files_dir)
        .with_context(|| format!("create app data directory {}", files_dir.display()))?;
    log_line(log_path, &format!("data_dir={}", files_dir.display()));

    // The core is not currently available as an iOS-compatible executable. Keep its
    // expected path explicit; this does not make core-dependent connections work.
    let core = files_dir.join("libbedrock_core.so");
    let mut layout = InstallLayout::ios(&files_dir, core);

    // Runtime carriers are read-only bundle resources. Keeping them in the .app avoids
    // copying large files into Documents on every launch and makes the IPA self-contained.
    let executable = std::env::current_exe().context("resolve bundled iOS executable path")?;
    let bundle_dir = executable
        .parent()
        .context("resolve iOS app bundle directory")?;
    let bundled_resources = bundle_dir.join("resources");
    let bundled_assets = bundled_resources.join("assets");
    if bundled_assets.is_dir() {
        let physics_name = layout
            .physics_registry
            .file_name()
            .context("resolve physics registry filename")?
            .to_owned();
        layout.resource_root = bundled_resources;
        layout.compiled_assets = bundled_assets;
        layout.physics_registry = layout.compiled_assets.join(physics_name);
        log_line(log_path, &format!("using bundled assets at {}", layout.compiled_assets.display()));
    } else {
        log_line(log_path, &format!("WARNING: bundled assets directory missing: {}", bundled_assets.display()));
    }

    log_line(log_path, "configuring iOS install layout");
    configure_ios(layout)?;
    log_line(log_path, "running lifecycle preflight");
    if cinnabar::lifecycle::before_run(false)? {
        log_line(log_path, "entering Cinnabar client");
        cinnabar::run(cinnabar::args::ClientArgs {
            force_vsync: true,
            ..Default::default()
        })?;
    } else {
        log_line(log_path, "lifecycle preflight requested a clean exit");
    }
    Ok(())
}

/// The app container's Documents folder is exposed through Files.app by Info.plist.
fn sandbox_files_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME unavailable in iOS sandbox")?;
    Ok(home.join("Documents").join("cinnabar"))
}

fn startup_log_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Documents").join("cinnabar").join("startup.log"))
}

fn install_panic_logger(path: Option<PathBuf>) {
    std::panic::set_hook(Box::new(move |panic| {
        log_line(&path, &format!("PANIC: {panic}"));
        // Preserve the platform/default panic report as well.
        eprintln!("Cinnabar panic: {panic}");
    }));
}

fn log_line(path: &Option<PathBuf>, message: &str) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let line = format!("[{timestamp}] {message}\n");
    if let Some(path) = path {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(line.as_bytes());
            let _ = file.sync_all();
            return;
        }
    }
    let _ = std::io::stderr().write_all(line.as_bytes());
}
