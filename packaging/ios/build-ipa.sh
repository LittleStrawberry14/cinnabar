#!/usr/bin/env bash
# Unsigned IPA: real Mach-O client binary plus the required runtime asset carriers.
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

echo "Preparing required runtime asset carriers..." | tee "$log"
# The iOS client was recently added as a workspace member, but Cargo.lock may predate
# that member. Resolve workspace metadata once so the new package is recorded before
# the asset Makefile invokes Cargo with --locked. This keeps subsequent builds locked.
cargo metadata --format-version 1 > /dev/null 2>> "$log" || {
  echo "Cargo workspace metadata resolution failed. See build.log" | tee -a "$log"
  exit 1
}
# This is the repository's supported asset pipeline. It obtains the pinned sample pack
# under the project's existing asset/EULA flow and builds the exact carriers the client loads.
set +e
make assets PROFILE="$profile" 2>&1 | tee -a "$log"
status=${PIPESTATUS[0]}
set -e
if [[ "$status" -ne 0 ]]; then
  echo "Asset preparation failed (exit $status). See build.log" | tee -a "$log"
  (cd "$out" && zip -qr cinnabar-ios-unsigned.ipa build.log) || true
  exit "$status"
fi

assets_dir="$repo_root/.local/assets/compiled"
if [[ ! -d "$assets_dir" || ! -f "$assets_dir/vanilla-v2193.mcbea" ]]; then
  echo "Required runtime carriers missing from $assets_dir after make assets" | tee -a "$log"
  exit 1
fi
mkdir -p "$app/resources/assets"
cp -a "$assets_dir/." "$app/resources/assets/"
echo "Packaged runtime carriers:" | tee -a "$log"
find "$app/resources/assets" -maxdepth 1 -type f -print | sort | tee -a "$log"

echo "Building cinnabar-ios for $triple ($profile)..." | tee -a "$log"

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
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleSupportedPlatforms</key>
  <array>
    <string>iPhoneOS</string>
  </array>
  <key>UIDeviceFamily</key>
  <array>
    <integer>1</integer>
    <integer>2</integer>
  </array>
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
  <key>UIFileSharingEnabled</key>
  <true/>
  <key>LSSupportsOpeningDocumentsInPlace</key>
  <true/>
</dict>
</plist>
PLIST

# Normalize and verify the app metadata before archiving. A successful Rust compile
# does not guarantee that iOS/LiveContainer can discover the bundle identifier.
if ! plutil -lint "$app/Info.plist" 2>&1 | tee -a "$log"; then
  echo "Generated Info.plist is invalid" | tee -a "$log"
  exit 1
fi
plutil -convert binary1 "$app/Info.plist"
bundle_id=$(plutil -extract CFBundleIdentifier raw -o - "$app/Info.plist")
bundle_executable=$(plutil -extract CFBundleExecutable raw -o - "$app/Info.plist")
if [[ "$bundle_id" != "com.cinnabar.ios" || "$bundle_executable" != "$bin_name" || ! -x "$app/$bin_name" ]]; then
  echo "Invalid app metadata: bundle_id=$bundle_id executable=$bundle_executable" | tee -a "$log"
  exit 1
fi
printf 'APPL????' > "$app/PkgInfo"
echo "Validated bundle metadata: id=$bundle_id executable=$bundle_executable" | tee -a "$log"

if command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - --deep "$app" 2>&1 | tee -a "$log" || true
fi

(cd "$out" && zip -qr cinnabar-ios-unsigned.ipa Payload build.log)

# Validate the actual archive, not just the source directory.
if ! unzip -t "$out/cinnabar-ios-unsigned.ipa" 2>&1 | tee -a "$log"; then
  echo "IPA archive validation failed" | tee -a "$log"
  exit 1
fi
archive_bundle_id=$(unzip -p "$out/cinnabar-ios-unsigned.ipa" "Payload/Cinnabar.app/Info.plist" | plutil -extract CFBundleIdentifier raw -o - - 2>> "$log") || {
  echo "Could not read CFBundleIdentifier from archived IPA" | tee -a "$log"
  exit 1
}
if [[ "$archive_bundle_id" != "$bundle_id" ]]; then
  echo "IPA bundle ID mismatch: $archive_bundle_id" | tee -a "$log"
  exit 1
fi
echo "Validated archived IPA bundle ID: $archive_bundle_id" | tee -a "$log"
size=$(wc -c < "$out/cinnabar-ios-unsigned.ipa" | tr -d ' ')
echo "Wrote $out/cinnabar-ios-unsigned.ipa ($size bytes)" | tee -a "$log"
