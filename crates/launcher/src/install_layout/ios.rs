//! iOS installs have no executable-relative layout; the sandbox Documents root holds data.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use super::{InstallLayout, LayoutError};

static CONFIGURED: OnceLock<InstallLayout> = OnceLock::new();

impl InstallLayout {
    /// Private app Documents (or other sandbox path) for settings, data, and prep kit.
    /// `core_executable` is a placeholder until a native core is shipped in the app bundle.
    #[must_use]
    pub fn ios(files_dir: &Path, core_executable: PathBuf) -> Self {
        let resource_root = files_dir.join("resources");
        let runtime_root = files_dir.join("run");
        Self {
            compiled_assets: resource_root.join("assets"),
            physics_registry: resource_root
                .join("assets")
                .join(assets::carriers::physics_registry_basename()),
            resource_root,
            core_executable,
            user_config_root: files_dir.join("config"),
            user_data_root: files_dir.join("data"),
            transient_runtime_root: runtime_root.clone(),
            runtime_root,
        }
    }
}

/// Records the iOS layout for [`InstallLayout::discover`]; it cannot change within a process.
pub fn configure_ios(layout: InstallLayout) -> Result<(), LayoutError> {
    if *CONFIGURED.get_or_init(|| layout.clone()) == layout {
        Ok(())
    } else {
        Err(LayoutError::IosReconfigured)
    }
}

pub(super) fn discover() -> Result<InstallLayout, LayoutError> {
    CONFIGURED
        .get()
        .cloned()
        .map(InstallLayout::with_prepared_assets)
        .ok_or(LayoutError::IosUnconfigured)
}
