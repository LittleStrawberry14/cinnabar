#[cfg(not(target_arch = "wasm32"))]
pub mod args;
#[cfg(not(target_arch = "wasm32"))]
pub mod asset_bindings;
#[cfg(not(target_arch = "wasm32"))]
mod audio;
#[cfg(not(target_arch = "wasm32"))]
mod audio_observations;
#[cfg(not(target_arch = "wasm32"))]
mod block_entities;
#[cfg(not(target_arch = "wasm32"))]
mod block_selection;
#[cfg(not(target_arch = "wasm32"))]
mod block_use;
#[cfg(not(target_arch = "wasm32"))]
pub mod camera;

#[cfg(feature = "developer-control")]
#[cfg(not(target_arch = "wasm32"))]
mod developer_control;
#[cfg(feature = "enhanced-diagnostics")]
#[cfg(not(target_arch = "wasm32"))]
mod enhanced_diagnostics;
#[cfg(not(target_arch = "wasm32"))]
mod environment;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod first_run_tests;
#[cfg(not(target_arch = "wasm32"))]
mod frame_pacing;
#[cfg(not(target_arch = "wasm32"))]
mod global_resources;
#[cfg(not(target_arch = "wasm32"))]
mod hotbar;
#[cfg(not(target_arch = "wasm32"))]
mod hud_tools;
#[cfg(not(target_arch = "wasm32"))]
mod install_layout;
#[cfg(not(target_arch = "wasm32"))]
mod interaction_authority;
#[cfg(not(target_arch = "wasm32"))]
mod item_use;
#[cfg(not(target_arch = "wasm32"))]
pub mod lifecycle;
#[cfg(not(target_arch = "wasm32"))]
pub mod local_player;
#[cfg(not(target_arch = "wasm32"))]
mod melee;
#[cfg(not(target_arch = "wasm32"))]
mod modding;
#[cfg(not(target_arch = "wasm32"))]
pub mod movement;
#[cfg(not(target_arch = "wasm32"))]
mod named_audio;

#[cfg(not(target_arch = "wasm32"))]
mod particles;
#[cfg(not(target_arch = "wasm32"))]
mod pick_block;
#[cfg(not(target_arch = "wasm32"))]
mod present_mode;
#[cfg(not(target_arch = "wasm32"))]
mod primitive_shapes;
#[cfg(not(target_arch = "wasm32"))]
mod render_mode;
#[cfg(not(target_arch = "wasm32"))]
pub mod semantic_controls;
#[cfg(not(target_arch = "wasm32"))]
mod server_experiences;
#[cfg(not(target_arch = "wasm32"))]
mod session;
#[cfg(not(target_arch = "wasm32"))]
pub mod session_audio;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod session_cleanup_tests;
#[cfg(not(target_arch = "wasm32"))]
mod survival_mining;
#[cfg(not(target_arch = "wasm32"))]
mod thread_budget;
#[cfg(feature = "tracy")]
#[cfg(not(target_arch = "wasm32"))]
mod tracy;
#[cfg(not(target_arch = "wasm32"))]
pub mod ui_runtime;
#[cfg(not(target_arch = "wasm32"))]
mod window_icon;

#[cfg(not(target_arch = "wasm32"))]
mod acceptance;
#[cfg(not(target_arch = "wasm32"))]
mod app;
#[cfg(not(target_arch = "wasm32"))]
mod presentation;
#[cfg(not(target_arch = "wasm32"))]
mod runtime;

#[cfg(not(target_arch = "wasm32"))]
pub use app::run;

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests;

#[cfg(test)]
#[global_allocator]
#[cfg(not(target_arch = "wasm32"))]
static ALLOCATOR: test_alloc::CountingAllocator = test_alloc::CountingAllocator;

#[cfg(not(target_arch = "wasm32"))]
mod far_terrain;
#[cfg(not(target_arch = "wasm32"))]
mod render_distance;
#[cfg(not(target_arch = "wasm32"))]
mod shell_adapters;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod shell_tests;

#[cfg(target_arch = "wasm32")]
pub mod browser;
