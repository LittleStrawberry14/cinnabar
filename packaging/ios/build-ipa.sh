#!/usr/bin/env bash
# Experimental unsigned IPA builder for iOS.
# This currently produces a stub bundle. Real client integration is not done yet.
set -euo pipefail

here="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
cd "$repo_root"

triple=aarch64-apple-ios
profile="${CINNABAR_IOS_PROFILE:-release}"
out="$repo_root/target/ios"
app="$out/Payload/Cinnabar.app"

rm -rf "$out"
mkdir -p "$app"

echo "Building cinnabar-ios for $triple ($profile)..."
cargo build -p cinnabar-ios --target "$triple" --profile "$profile" --locked || {
  echo "Rust build failed (expected while the port is incomplete). Continuing with stub."
}

# Prefer the real staticlib if it exists, otherwise drop a placeholder binary.
lib_path="target/$triple/$profile/libcinnabar_ios.a"
if [[ -f "$lib_path" ]]; then
  cp "$lib_path" "$app/libcinnabar_ios.a"
fi

# Minimal executable placeholder so the IPA structure is valid.
printf '#!/bin/sh\necho "Cinnabar iOS stub – not a real client yet"\n' > "$app/cinnabar"
chmod +x "$app/cinnabar"

# Info.plist
cat > "$app/Info.plist" << 'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>
  <string>com.cinnabar.ios</string>
  <key>CFBundleName</key>
  <string>Cinnabar</string>
  <key>CFBundleDisplayName</key>
  <string>Cinnabar</string>
  <key>CFBundleExecutable</key>
  <string>cinnabar</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>0.1.0</string>
  <key>MinimumOSVersion</key>
  <string>15.0</string>
  <key>UIRequiredDeviceCapabilities</key>
  <array>
    <string>arm64</string>
  </array>
  <key>UISupportedInterfaceOrientations</key>
  <array>
    <string>UIInterfaceOrientationPortrait</string>
    <string>UIInterfaceOrientationLandscapeLeft</string>
    <string>UIInterfaceOrientationLandscapeRight</string>
  </array>
</dict>
</plist>
EOF

# Package
(cd "$out" && zip -qr cinnabar-ios-unsigned.ipa Payload)
echo "Wrote $out/cinnabar-ios-unsigned.ipa"
