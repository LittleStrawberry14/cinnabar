/// Cargo runs tests from its build directory, which `build-dir` puts outside the
/// checkout; such a binary resolves as if it ran from its workspace's `target/debug`.
fn cargo_artifact_path(executable: PathBuf, manifest_dir: Option<&Path>) -> PathBuf {
    if development_root(&executable).is_some() {
        return executable;
    }
    let workspace = manifest_dir.and_then(|manifest| {
        manifest
            .ancestors()
            .find(|directory| directory.join("Cargo.lock").is_file())
    });
    match (workspace, executable.file_name()) {
        (Some(root), Some(name)) => root.join("target/debug/deps").join(name),
        _ => executable,
    }
}

fn development_root(executable: &Path) -> Option<(PathBuf, PathBuf)> {
    for ancestor in executable.ancestors() {
        if ancestor.file_name().is_some_and(|name| name == "target") {
            let profile = executable
                .strip_prefix(ancestor)
                .ok()?
                .components()
                .next()?
                .as_os_str();
            if !["debug", "release", "play"]
                .iter()
                .any(|known| profile == *known)
            {
                continue;
            }
            let root = ancestor.parent()?.to_owned();
            let mut binary_dir = executable.parent()?.to_owned();
            if binary_dir.file_name().is_some_and(|name| name == "deps") {
                binary_dir.pop();
            }
            return Some((root, binary_dir));
        }
    }
    None
}

fn isolated_user_roots(
    platform: Platform,
    environment: &InstallEnvironment,
) -> Result<Option<(PathBuf, PathBuf, PathBuf)>, LayoutError> {
    let Some(root) = environment.user_root.as_deref() else {
        return Ok(None);
    };
    let name = match platform {
        Platform::Windows => "Windows",
        Platform::Linux => "Linux",
        Platform::MacOs => "macOS",
    };
    require_absolute(root, platform, "CINNABAR_USER_ROOT", name)?;
    Ok(Some((
        root.join("config"),
        root.join("data"),
        root.join("run"),
    )))
}

fn user_roots(
    platform: Platform,
    environment: &InstallEnvironment,
) -> Result<(PathBuf, PathBuf, PathBuf), LayoutError> {
    if let Some(roots) = isolated_user_roots(platform, environment)? {
        return Ok(roots);
    }
    match platform {
        Platform::Windows => {
            let base = environment
                .local_app_data
                .as_deref()
                .filter(|path| !path.as_os_str().is_empty())
                .ok_or(LayoutError::MissingLocalAppData)?;
            require_absolute(base, Platform::Windows, "LOCALAPPDATA", "Windows")?;
            let root = base.join(APP_DIR);
            Ok((root.clone(), root.clone(), root.join("run")))
        }
        Platform::MacOs => {
            let home = required_home(environment, "macOS")?;
            require_absolute(home, Platform::MacOs, "HOME", "macOS")?;
            let root = home.join("Library/Application Support").join(APP_DIR);
            Ok((root.clone(), root.clone(), root.join("run")))
        }
        Platform::Linux => {
            let config_base = environment
                .xdg_config_home
                .as_deref()
                .filter(|path| absolute_for(Platform::Linux, path))
                .map(Path::to_owned);
            let data_base = environment
                .xdg_data_home
                .as_deref()
                .filter(|path| absolute_for(Platform::Linux, path))
                .map(Path::to_owned);
            let home = if config_base.is_none() || data_base.is_none() {
                let home = required_home(environment, "Linux")?;
                require_absolute(home, Platform::Linux, "HOME", "Linux")?;
                Some(home)
            } else {
                None
            };
            let config = config_base
                .unwrap_or_else(|| {
                    home.expect("home required for config fallback")
                        .join(".config")
                })
                .join("cinnabar");
            let data = data_base
                .unwrap_or_else(|| {
                    home.expect("home required for data fallback")
                        .join(".local/share")
                })
                .join("cinnabar");
            let runtime = environment
                .xdg_runtime_dir
                .as_ref()
                .filter(|path| absolute_for(Platform::Linux, path))
                .map_or_else(|| data.join("run"), |root| root.join("cinnabar"));
            Ok((config, data, runtime))
        }
    }
}

fn required_home<'a>(
    environment: &'a InstallEnvironment,
    platform: &'static str,
) -> Result<&'a Path, LayoutError> {
    environment
        .home
        .as_deref()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(LayoutError::MissingUserHome { platform })
}

fn require_absolute(
    path: &Path,
    platform: Platform,
    variable: &'static str,
    platform_name: &'static str,
) -> Result<(), LayoutError> {
    if absolute_for(platform, path) {
        Ok(())
    } else {
        Err(LayoutError::InvalidUserRoot {
            variable,
            platform: platform_name,
        })
    }
}

fn absolute_for(platform: Platform, path: &Path) -> bool {
    let value = path.as_os_str().to_string_lossy();
    if value.is_empty() {
        return false;
    }
    match platform {
        Platform::Windows => {
            let bytes = value.as_bytes();
            (bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\'))
                || value.starts_with("\\\\")
                || value.starts_with("//")
        }
        Platform::Linux | Platform::MacOs => value.starts_with('/'),
    }
}

const fn core_filename(platform: Platform) -> &'static str {
    match platform {
        Platform::Windows => "bedrock-core.exe",
        Platform::Linux | Platform::MacOs => "bedrock-core",
    }
}

const fn home_variable(platform: Platform) -> &'static str {
    match platform {
        Platform::Windows => "USERPROFILE",
        Platform::Linux | Platform::MacOs => "HOME",
    }
}

const fn current_platform() -> Platform {
    #[cfg(target_os = "windows")]
    return Platform::Windows;
    #[cfg(target_os = "linux")]
    return Platform::Linux;
    #[cfg(target_os = "macos")]
    return Platform::MacOs;
    #[allow(unreachable_code)]
    Platform::Linux
}
