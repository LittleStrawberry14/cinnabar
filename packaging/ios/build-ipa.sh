#!/usr/bin/env bash
# Unsigned IPA: real Mach-O client binary for aarch64-apple-ios.
set -euo pipefail

here="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
cd "$repo_root"

triple=aarch64-apple-ios
profile="${CINNABAR_IOS_PROFILE:-release}"
out="$repo_root/target/ios"
app="$out/Payload/Cinnabar.app"
log="$out/build.log"
bin_name=cinnabar

export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-15.0}"

rm -rf "$out"
mkdir -p "$app"

echo "Building cinnabar-ios for $triple ($profile)..." | tee "$log"

set +e
cargo build -p cinnabar-ios --target "$triple" --profile "$profile" 2>&1 | tee -a "$log"
status=${PIPESTATUS[0]}
set -e

if [[ "$status" -ne 0 ]]; then
  echo "Rust build failed (exit $status). See build.log" | tee -a "$log"
  (cd "$out" && zip -qr cinnabar-ios-unsigned.ipa build.log) || true
  exit "$status"
fi

echo "Rust build succeeded" | tee -a "$log"

bin_path="target/$triple/$profile/$bin_name"
if [[ ! -f "$bin_path" ]]; then
  echo "missing $bin_path" | tee -a "$log"
  exit 1
fi

cp "$bin_path" "$app/$bin_name"
chmod +x "$app/$bin_name"
ls -la "$bin_path" | tee -a "$log"
file "$app/$bin_name" | tee -a "$log" || true

# Ad-hoc sign so SideStore / TrollStore style tools can install.
if command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - --entitlements "$here/entitlements.plist" "$app/$bin_name" 2>&1 | tee -a "$log" || true
fi

cat > "$app/Info.plist" << PLIST
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
  <string>${bin_name}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>0.1.0</string>
  <key>MinimumOSVersion</key>
  <string>15.0</string>
  <key>LSRequiresIPhoneOS</key>
  <true/>
  <key>UIRequiredDeviceCapabilities</key>
  <array>
    <string>arm64</string>
    <string>metal</string>
  </array>
  <key>UIStatusBarHidden</key>
  <true/>
  <key>UIViewControllerBasedStatusBarAppearance</key>
  <false/>
  <key>UISupportedInterfaceOrientations</key>
  <array>
    <string>UIInterfaceOrientationPortrait</string>
    <string>UIInterfaceOrientationLandscapeLeft</string>
    <string>UIInterfaceOrientationLandscapeRight</string>
  </array>
  <key>UISupportedInterfaceOrientations~ipad</key>
  <array>
    <string>UIInterfaceOrientationPortrait</string>
    <string>UIInterfaceOrientationPortraitUpsideDown</string>
    <string>UIInterfaceOrientationLandscapeLeft</string>
    <string>UIInterfaceOrientationLandscapeRight</string>
  </array>
  <key>CADisableMinimumFrameDurationOnPhone</key>
  <true/>
</dict>
</plist>
PLIST

if command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - --deep "$app" 2>&1 | tee -a "$log" || true
fi

(cd "$out" && zip -qr cinnabar-ios-unsigned.ipa Payload build.log)
size=$(wc -c < "$out/cinnabar-ios-unsigned.ipa" | tr -d ' ')
echo "Wrote $out/cinnabar-ios-unsigned.ipa ($size bytes)" | tee -a "$log"
