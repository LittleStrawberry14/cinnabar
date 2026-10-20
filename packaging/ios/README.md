# iOS (experimental)

Builds an unsigned IPA with a real `aarch64-apple-ios` Mach-O client binary and the runtime asset carriers produced by the repository's `make assets` pipeline.

### Get the IPA

1. Open Actions → **iOS** workflow and download the `cinnabar-ios-unsigned` artifact.
2. Install the IPA with a compatible sideloading tool.
3. Launch Cinnabar once. Startup diagnostics are written to `Documents/cinnabar/startup.log`.

### Find startup logs on iPhone

The app enables iOS document sharing. In the Files app, browse **On My iPhone** (or the location exposed by your sideloading setup) → **Cinnabar** → `cinnabar/startup.log`. If LiveContainer stores the app's Documents folder inside its own container, inspect that app's exposed files instead.

The log is flushed as each startup step runs and includes Rust errors and panic messages. It cannot record a crash that happens before the Rust entry point executes, nor can it catch every native abort or operating-system kill.

### What the IPA includes

- The Rust iOS client executable
- Compiled runtime asset carriers under the app bundle's `resources/assets`
- Startup logging to the app's Documents directory, visible through Files where the sideloading host exposes it

### Known limitations

- The separate Go Bedrock core is **not** an iOS-compatible executable in this package. Core-dependent connections and gameplay are not fixed by bundling assets.
- Touch UI and iOS-specific polish are incomplete.
- The IPA is not production-signed.

This remains an experimental port. A successful build does not guarantee that every runtime subsystem works on device; use `startup.log` to diagnose remaining failures.
