#!/usr/bin/env bash
# Unsigned IPA: LiveContainer-compatible App.app containing the client binary and runtime assets.
set -euo pipefail

here="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
cd "$repo_root"

triple=aarch64-apple-ios
profile="${CINNABAR_IOS_PROFILE:-release}"
out="$repo_root/target/ios"
app="$out/Payload/App.app"
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

# LiveContainer looks for Payload/App.app during import, so preserve that exact archive path.
# Normalize and verify the bundle metadata before archiving.
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

# Keep only Payload at the IPA root. The build log is uploaded as a separate artifact file.
rm -f "$out/cinnabar-ios-unsigned.ipa"
(cd "$out" && zip -qry -y cinnabar-ios-unsigned.ipa Payload)

# Validate the archive itself: layout, plist values, architecture, and Unix executable mode.
ipa="$out/cinnabar-ios-unsigned.ipa"
if ! unzip -t "$ipa" 2>&1 | tee -a "$log"; then
  echo "IPA archive validation failed" | tee -a "$log"
  exit 1
fi

entries=$(unzip -Z1 "$ipa")
if printf '%s\n' "$entries" | grep -Eq '(^/|(^|/)\.\./|^__MACOSX/)'; then
  echo "Unexpected path found in IPA archive" | tee -a "$log"
  exit 1
fi
if ! printf '%s\n' "$entries" | grep -Fxq "Payload/App.app/Info.plist" ||
   ! printf '%s\n' "$entries" | grep -Fxq "Payload/App.app/$bin_name"; then
  echo "IPA is missing Info.plist or the app executable at the expected path" | tee -a "$log"
  exit 1
fi
if printf '%s\n' "$entries" | grep -Ev '^Payload(/|$)' | grep -q .; then
  echo "IPA contains entries outside Payload" | tee -a "$log"
  exit 1
fi

tmp_plist="$out/archived-Info.plist"
unzip -p "$ipa" "Payload/App.app/Info.plist" > "$tmp_plist"
plutil -lint "$tmp_plist" 2>&1 | tee -a "$log"
archive_bundle_id=$(plutil -extract CFBundleIdentifier raw -o - "$tmp_plist")
archive_executable=$(plutil -extract CFBundleExecutable raw -o - "$tmp_plist")
for key in CFBundleName CFBundleVersion CFBundleShortVersionString CFBundlePackageType MinimumOSVersion; do
  value=$(plutil -extract "$key" raw -o - "$tmp_plist")
  [[ -n "$value" ]] || { echo "Empty required plist key: $key" | tee -a "$log"; exit 1; }
done
[[ "$archive_bundle_id" == "com.cinnabar.ios" ]] || { echo "Bad archived bundle ID: $archive_bundle_id" | tee -a "$log"; exit 1; }
[[ "$archive_executable" == "$bin_name" ]] || { echo "Bad archived executable name: $archive_executable" | tee -a "$log"; exit 1; }
if plutil -p "$tmp_plist" | grep -Eq '\$\((PRODUCT_BUNDLE_IDENTIFIER|EXECUTABLE_NAME)\)'; then
  echo "Unresolved Xcode placeholder in Info.plist" | tee -a "$log"
  exit 1
fi
plutil -p "$tmp_plist" | tee -a "$log"

archive_bin="$out/archived-$bin_name"
unzip -p "$ipa" "Payload/App.app/$archive_executable" > "$archive_bin"
file "$archive_bin" | tee -a "$log"
if ! file "$archive_bin" | grep -Eq 'Mach-O 64-bit (executable )?arm64'; then
  echo "Archived executable is not Mach-O 64-bit arm64" | tee -a "$log"
  exit 1
fi
if ! otool -l "$archive_bin" | grep -A8 -E 'LC_BUILD_VERSION|LC_VERSION_MIN_IPHONEOS' | grep -Eq 'platform IOS([[:space:]]|$)|LC_VERSION_MIN_IPHONEOS'; then
  echo "Archived executable does not declare an iOS device platform" | tee -a "$log"
  exit 1
fi
zipinfo -l "$ipa" "Payload/App.app/$archive_executable" | tee -a "$log"
if ! zipinfo -l "$ipa" "Payload/App.app/$archive_executable" | grep -Eq '^-rwx'; then
  echo "Executable permission was not preserved in IPA" | tee -a "$log"
  exit 1
fi
echo "Verified IPA layout, plist, iOS arm64 executable, and executable mode" | tee -a "$log"
size=$(wc -c < "$ipa" | tr -d ' ')
echo "Wrote $ipa ($size bytes)" | tee -a "$log"
