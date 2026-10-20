#!/usr/bin/env bash
# Experimental unsigned IPA builder for iOS.
set -euo pipefail

here="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
cd "$repo_root"

triple=aarch64-apple-ios
profile="${CINNABAR_IOS_PROFILE:-release}"
out="$repo_root/target/ios"
app="$out/Payload/Cinnabar.app"
log="$out/build.log"

rm -rf "$out"
mkdir -p "$app"

echo "Building cinnabar-ios for $triple ($profile)..." | tee "$log"

set +e
cargo build -p cinnabar-ios --target "$triple" --profile "$profile" 2>&1 | tee -a "$log"
status=${PIPESTATUS[0]}
set -e

if [[ "$status" -eq 0 ]]; then
  echo "Rust build succeeded" | tee -a "$log"
else
  echo "Rust build failed (exit $status). See build.log" | tee -a "$log"
fi

lib_path="target/$triple/$profile/libcinnabar_ios.a"
if [[ -f "$lib_path" ]]; then
  cp "$lib_path" "$app/libcinnabar_ios.a"
  ls -la "$lib_path" | tee -a "$log"
fi

cat > "$app/cinnabar" << 'EOF'
#!/bin/sh
echo "Cinnabar iOS – port in progress"
EOF
chmod +x "$app/cinnabar"

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

(cd "$out" && zip -qr cinnabar-ios-unsigned.ipa Payload build.log)
size=$(wc -c < "$out/cinnabar-ios-unsigned.ipa" | tr -d ' ')
echo "Wrote $out/cinnabar-ios-unsigned.ipa ($size bytes)" | tee -a "$log"
