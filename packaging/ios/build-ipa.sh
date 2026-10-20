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

# Don't use --locked yet — the ios crate is new and the lockfile needs to settle.
if cargo build -p cinnabar-ios --target "$triple" --profile "$profile"; then
  echo "Rust build succeeded"
  built=1
else
  echo "Rust build failed. Continuing with stub."
  built=0
fi

# Prefer the real staticlib if it exists
lib_path="target/$triple/$profile/libcinnabar_ios.a"
if [[ -f "$lib_path" ]]; then
  cp "$lib_path" "$app/libcinnabar_ios.a"
  echo "Included static library"
fi

# Minimal executable placeholder so the IPA structure is valid.
# Real binary will replace this later.
cat > "$app/cinnabar" << 'EOF'
#!/bin/sh
echo "Cinnabar iOS stub – not a real client yet"
echo "This is only packaging scaffolding."
EOF
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
size=$(wc -c < "$out/cinnabar-ios-unsigned.ipa" | tr -d ' ')
echo "Wrote $out/cinnabar-ios-unsigned.ipa ($size bytes)"
