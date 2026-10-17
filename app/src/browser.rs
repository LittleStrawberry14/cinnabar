//! Browser application composition over the client's shared renderer and world owners.

use std::sync::Arc;

use assets::carriers::startup::delivery::StartupCarriers;
use bevy::{
    anti_alias::AntiAliasPlugin,
    app::TerminalCtrlCHandlerPlugin,
    prelude::*,
    render::{RenderPlugin, settings::RenderCreation},
    window::WindowPlugin,
};
use render_scene::{AtmosphereTextureAssets, ChunkTextureAssets};

/// Verified startup inputs retained for the world's first network bootstrap.
#[derive(Resource)]
pub struct PreparedStartup(pub StartupCarriers);

/// Starts the renderer on an existing canvas, retaining the verified world inputs.
pub fn run(startup: StartupCarriers, canvas: String) -> anyhow::Result<()> {
    let mut settings = bevy::render::settings::WgpuSettings::default();
    render::device_settings::configure(&mut settings);
    let world = Arc::clone(&startup.world);
    let atmosphere = Arc::clone(&startup.atmosphere);
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    canvas: Some(canvas),
                    fit_canvas_to_parent: true,
                    prevent_default_event_handling: false,
                    ..default()
                }),
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(settings),
                ..default()
            })
            .disable::<AntiAliasPlugin>()
            .disable::<TerminalCtrlCHandlerPlugin>(),
    )
    .insert_resource(ChunkTextureAssets::new(world))
    .insert_resource(AtmosphereTextureAssets::new(
        atmosphere,
        startup.atmosphere_identity,
    ))
    .insert_resource(PreparedStartup(startup))
    .add_plugins((
        render::AtmospherePlugin,
        render::ChunkRenderPlugin::default(),
        render::startup_presentation::StartupPresentationPlugin,
    ))
    .add_systems(Startup, camera);
    let exit = app.run();
    anyhow::ensure!(!exit.is_error(), "browser client stopped with an error");
    Ok(())
}

/// Installs the world view before the first surface is presented.
fn camera(mut commands: Commands) {
    commands.spawn((Camera3d::default(), Transform::default(), Msaa::Off));
}
