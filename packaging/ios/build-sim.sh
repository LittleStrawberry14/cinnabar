#!/usr/bin/env bash
# Builds the iOS simulator package for the CI launch test: the same app executable linked with
# the same Go core, compiled for aarch64-apple-ios-sim against the simulator SDK instead of the
# device triple. build-ipa.sh explains the targets; CINNABAR_IOS_TARGET is its one selector.
#
# The device IPA from build-ipa.sh cannot run in an iOS simulator: the simulator only executes
# simulator slices, and a simulator slice would fail device installation instead, so the two are
# packaged separately. Sideload the device IPA; hand the -sim IPA to .github/workflows/ios-sim.yml.
#
# Needs the same macOS tools as build-ipa.sh plus the aarch64-apple-ios-sim Rust target.
set -euo pipefail

here="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

export CINNABAR_IOS_TARGET="${CINNABAR_IOS_TARGET:-simulator}"
exec bash "$here/build-ipa.sh"
