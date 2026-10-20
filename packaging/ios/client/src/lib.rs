// Experimental iOS entry.

#[cfg(target_os = "ios")]
mod ios {
    // Real client hooks go here as the port advances.
}

#[unsafe(no_mangle)]
pub extern "C" fn cinnabar_ios_init() -> i32 {
    0
}

/// Placeholder entry the host app can call once UIKit is up.
#[unsafe(no_mangle)]
pub extern "C" fn cinnabar_ios_start() -> i32 {
    // Full cinnabar::run path comes later; this just proves linkage.
    0
}
