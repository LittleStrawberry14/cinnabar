# iOS (experimental)

Builds an **unsigned .ipa** with a real `aarch64-apple-ios` Mach-O client binary.

### Get the IPA
1. Actions → **iOS** workflow → download `cinnabar-ios-unsigned`
2. Install with SideStore / TrollStore / similar (you need JIT for Wasmtime mods)

### What works so far
- Full Rust client compiles for iOS
- App package uses a real executable (not a shell stub)
- Sandbox layout under `Documents/cinnabar`
- Desktop crash-monitor / Wayland / arboard gated off

### Still incomplete
- First-run assets / Go core may be missing on device
- Touch UI and iOS-specific polish
- Production signing

Expect crashes or a black screen until assets + core path are filled in. This is the first bootable packaging step.
