//! iOS host: UIApplication entry via Bevy/winit, sandbox Documents layout.

use std::path::PathBuf;

use anyhow::{Context, Result};
use launcher::install_layout::{InstallLayout, configure_ios};

#[bevy::prelude::bevy_main]
fn main() {
    let code = match run_client() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{} failed: {error:#}", launcher::PRODUCT_NAME);
            1
        }
    };
    std::process::exit(code);
}

fn run_client() -> Result<()> {
    let files_dir = sandbox_files_dir()?;
    std::fs::create_dir_all(&files_dir).ok();
    // No separate Go core binary on iOS yet; point at a placeholder under the sandbox.
    let core = files_dir.join("libbedrock_core.so");
    configure_ios(InstallLayout::ios(&files_dir, core))?;
    if cinnabar::lifecycle::before_run(false)? {
        cinnabar::run(cinnabar::args::ClientArgs {
            force_vsync: true,
            ..Default::default()
        })?;
    }
    Ok(())
}

/// App sandbox data root (Documents/cinnabar). HOME is the container on iOS.
fn sandbox_files_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME unavailable in iOS sandbox")?;
    Ok(home.join("Documents").join("cinnabar"))
}
