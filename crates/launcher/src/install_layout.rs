use std::path::{Path, PathBuf};
use thiserror::Error;

mod android;
pub use android::configure_android;
mod ios;
pub use ios::configure_ios;

const APP_DIR: &str = crate::PRODUCT_NAME;

pub fn vanilla_pack_relative() -> String {
    assets::vanilla_source().installed_pack_dir("resource_pack")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum Platform {
    Windows,
    Linux,
    MacOs,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallEnvironment {
    pub executable: PathBuf,
    pub user_root: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_runtime_dir: Option<PathBuf>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum LayoutError {
    #[error("the current executable path is unavailable")]
    MissingExecutable,
    #[error("{platform} user home is unavailable")]
    MissingUserHome { platform: &'static str },
    #[error("LOCALAPPDATA is unavailable for the Windows installed layout")]
    MissingLocalAppData,
    #[error("{variable} must be an absolute path for the {platform} installed layout")]
    InvalidUserRoot {
        variable: &'static str,
        platform: &'static str,
    },
    #[error("Linux executable is not inside a supported <prefix>/bin layout: `{0}`")]
    InvalidLinuxLayout(PathBuf),
    #[error("macOS executable is not inside <name>.app/Contents/MacOS: `{0}`")]
    InvalidMacOsBundle(PathBuf),
    #[error("the Android activity has not configured the install layout")]
    AndroidUnconfigured,
    #[error("the Android install layout cannot change within a process")]
    AndroidReconfigured,
    #[error("the iOS app has not configured the install layout")]
    IosUnconfigured,
    #[error("the iOS install layout cannot change within a process")]
    IosReconfigured,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallLayout {
    pub resource_root: PathBuf,
    pub compiled_assets: PathBuf,
    pub physics_registry: PathBuf,
    pub core_executable: PathBuf,
    pub user_config_root: PathBuf,
    pub user_data_root: PathBuf,
    pub runtime_root: PathBuf,
    transient_runtime_root: PathBuf,
}

include!("install_layout_impl.rs");
