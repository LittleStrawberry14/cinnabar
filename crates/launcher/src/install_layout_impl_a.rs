impl InstallLayout {
    pub fn resolve(
        platform: Platform,
        environment: &InstallEnvironment,
    ) -> Result<Self, LayoutError> {
        if environment.executable.as_os_str().is_empty() {
            return Err(LayoutError::MissingExecutable);
        }
        if let Some((root, binary_dir)) = development_root(&environment.executable) {
            let local = root.join(".local");
            let isolated = isolated_user_roots(platform, environment)?;
            let custom = isolated.is_some();
            let (config, data, runtime) = isolated
                .unwrap_or_else(|| (local.join("cinnabar"), local.clone(), local.join("run")));
            return Ok(Self {
                resource_root: local.clone(),
                compiled_assets: local.join("assets/compiled"),
                physics_registry: local
                    .join("assets/compiled")
                    .join(assets::carriers::physics_registry_basename()),
                core_executable: binary_dir.join(core_filename(platform)),
                user_config_root: config,
                user_data_root: data,
                runtime_root: runtime.clone(),
                transient_runtime_root: if custom {
                    runtime
                } else {
                    local.join("cinnabar")
                },
            });
        }

        let executable_dir = environment
            .executable
            .parent()
            .ok_or(LayoutError::MissingExecutable)?;
        let (resource_root, core_executable) = match platform {
            Platform::Windows => (
                executable_dir.join("resources"),
                executable_dir.join(core_filename(platform)),
            ),
            Platform::Linux => {
                let prefix = executable_dir
                    .parent()
                    .filter(|_| executable_dir.file_name().is_some_and(|name| name == "bin"))
                    .ok_or_else(|| {
                        LayoutError::InvalidLinuxLayout(environment.executable.clone())
                    })?;
                (
                    prefix.join("share/cinnabar"),
                    executable_dir.join(core_filename(platform)),
                )
            }
            Platform::MacOs => {
                let contents = executable_dir
                    .parent()
                    .filter(|contents| contents.file_name().is_some_and(|name| name == "Contents"))
                    .filter(|_| {
                        executable_dir
                            .file_name()
                            .is_some_and(|name| name == "MacOS")
                    })
                    .filter(|contents| {
                        contents.parent().and_then(Path::extension)
                            == Some(std::ffi::OsStr::new("app"))
                    })
                    .ok_or_else(|| {
                        LayoutError::InvalidMacOsBundle(environment.executable.clone())
                    })?;
                (
                    contents.join("Resources"),
                    executable_dir.join(core_filename(platform)),
                )
            }
        };
        let (user_config_root, user_data_root, runtime_root) = user_roots(platform, environment)?;
        Ok(Self {
            compiled_assets: resource_root.join("assets"),
            physics_registry: resource_root
                .join("assets")
                .join(assets::carriers::physics_registry_basename()),
            resource_root,
            core_executable,
            user_config_root,
            user_data_root,
            transient_runtime_root: runtime_root.clone(),
            runtime_root,
        })
    }

    pub fn discover() -> Result<Self, LayoutError> {
        if cfg!(target_os = "android") {
            return android::discover();
        }
        if cfg!(target_os = "ios") {
            return ios::discover();
        }
        let platform = current_platform();
        let home = std::env::var_os(home_variable(platform)).map(PathBuf::from);
        let layout = Self::resolve(
            platform,
            &InstallEnvironment {
                executable: cargo_artifact_path(
                    std::env::current_exe().map_err(|_| LayoutError::MissingExecutable)?,
                    std::env::var_os("CARGO_MANIFEST_DIR")
                        .map(PathBuf::from)
                        .as_deref(),
                ),
                user_root: std::env::var_os("CINNABAR_USER_ROOT").map(PathBuf::from),
                home,
                local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
                xdg_config_home: std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
                xdg_data_home: std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
                xdg_runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            },
        )?;
        Ok(layout.with_prepared_assets())
    }

    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.compiled_assets != self.resource_root.join("assets/compiled")
    }

    #[must_use]
    pub fn with_prepared_assets(mut self) -> Self {
        let bundled = self.world_assets();
        if self.is_installed() && !bundled.is_file() {
            self.compiled_assets = self.prepared_assets_dir();
        }
        self
    }

    #[must_use]
    pub fn prepared_assets_dir(&self) -> PathBuf {
        self.user_data_root.join("assets/compiled")
    }

    #[must_use]
    pub fn prepare_workspace(&self) -> PathBuf {
        self.user_data_root.join("prepare")
    }

    #[must_use]
    pub fn prep_kit(&self) -> PathBuf {
        self.resource_root.join("prep-kit")
    }

    #[must_use]
    pub fn update_dir(&self) -> PathBuf {
        self.user_data_root.join("update")
    }

    #[must_use]
    pub fn log_dir(&self) -> PathBuf {
        self.user_data_root.join("logs")
    }

    #[must_use]
    pub fn screenshots_dir(&self) -> PathBuf {
        self.user_data_root.join("screenshots")
    }

    #[must_use]
    pub fn crash_dir(&self) -> PathBuf {
        self.user_data_root.join("crashes")
    }

    #[must_use]
    pub fn world_assets(&self) -> PathBuf {
        self.compiled_assets.join(assets::carriers::WORLD.output)
    }

    #[must_use]
    pub fn starter_skins_asset(&self) -> PathBuf {
        self.compiled_assets
            .join(assets::carriers::STARTER_SKINS.output)
    }

    #[must_use]
    pub fn player_skin_asset(&self) -> PathBuf {
        self.resource_root.join("assets/skin/player.png")
    }

    pub fn dressing_room_dir(&self) -> PathBuf {
        self.user_data_root.join("skins")
    }

    pub fn dressing_room_capes_dir(&self) -> PathBuf {
        self.dressing_room_dir().join("capes")
    }

    pub fn skin_selection_file(&self) -> PathBuf {
        self.user_config_root.join("skin-selection.json")
    }

    #[must_use]
    pub fn auth_cache(&self) -> PathBuf {
        self.user_data_root.join("auth/microsoft-token.json")
    }

    #[must_use]
    pub fn device_profile_file(&self) -> PathBuf {
        self.auth_cache().with_file_name("device.json")
    }

    #[must_use]
    pub fn launcher_artwork_dir(&self) -> PathBuf {
        self.auth_cache()
            .with_file_name("catalog-cache")
            .join("artwork")
    }

    #[must_use]
    pub fn server_trust_file(&self) -> PathBuf {
        self.user_data_root.join("trusted_server_public_keys.json")
    }

    #[must_use]
    pub fn vanilla_pack_dir(&self) -> PathBuf {
        self.resource_root.join(vanilla_pack_relative())
    }

    #[must_use]
    pub fn global_resource_packs_dir(&self) -> PathBuf {
        self.user_data_root.join("resource-packs/global")
    }

    #[must_use]
    pub fn mods_dir(&self) -> PathBuf {
        self.user_data_root.join("mods")
    }

    #[must_use]
    pub fn resource_pack_cache_dir(&self) -> PathBuf {
        self.user_data_root.join("resource-packs/v1/objects")
    }

    #[must_use]
    pub fn client_blob_cache_file(&self) -> PathBuf {
        self.user_data_root.join("cache/client-blobs-v1.redb")
    }

    #[must_use]
    pub fn far_terrain_cache_dir(&self) -> PathBuf {
        self.user_data_root.join("far-terrain/v1")
    }

    #[must_use]
    pub fn compiled_pack_cache_dir(&self) -> PathBuf {
        self.user_data_root.join("resource-packs/compiled")
    }

    #[must_use]
    pub fn local_worlds_dir(&self) -> PathBuf {
        self.user_data_root.join("worlds")
    }

    #[must_use]
    pub fn bds_dir(&self) -> PathBuf {
        self.user_data_root.join("bds")
    }

    #[must_use]
    pub fn server_file(&self) -> PathBuf {
        self.user_config_root.join("servers.json")
    }

    #[must_use]
    pub fn graphics_file(&self) -> PathBuf {
        self.user_config_root.join("graphics.json")
    }

    #[must_use]
    pub fn catalog_file(&self, process_id: u32) -> PathBuf {
        self.transient_runtime_root
            .join(format!("catalog-{process_id}.json"))
    }

    #[must_use]
    pub fn transient_runtime_root(&self) -> &Path {
        &self.transient_runtime_root
    }

    #[must_use]
    pub fn direct_socket_dir(&self, process_id: u32) -> PathBuf {
        self.transient_runtime_root
            .join(format!("direct-{process_id}"))
    }

    #[must_use]
    pub fn account_socket_dir(&self, pid: u32, incarnation: u64) -> PathBuf {
        self.transient_runtime_root
            .join(format!("account-{pid}-{incarnation}"))
    }

    #[must_use]
    pub fn connect_socket_dir(&self, process_id: u32, generation: u64) -> PathBuf {
        self.transient_runtime_root
            .join(format!("connect-{process_id}-{generation}"))
    }
}
