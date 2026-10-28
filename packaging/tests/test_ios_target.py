"""The iOS packager refuses an unknown CINNABAR_IOS_TARGET before its Darwin guard, and
build-sim.sh's simulator default passes that check."""

import os
from pathlib import Path
import subprocess
import sys
import unittest


ROOT = Path(__file__).resolve().parents[2]
BUILD_IPA = ROOT / "packaging/ios/build-ipa.sh"
BUILD_SIM = ROOT / "packaging/ios/build-sim.sh"

DARWIN = sys.platform == "darwin"


def run_script(script, target=None):
    """Runs the packager (the wrapper included) with an optional CINNABAR_IOS_TARGET."""
    env = os.environ.copy()
    env.pop("CINNABAR_IOS_TARGET", None)
    if target is not None:
        env["CINNABAR_IOS_TARGET"] = target
    return subprocess.run(
        [sys.executable if script.suffix == ".py" else "bash", str(script)],
        capture_output=True,
        text=True,
        env=env,
    )


class IosTargetTests(unittest.TestCase):
    def test_an_unknown_target_rejects_before_the_darwin_guard(self):
        # Both scripts respond identically on any platform: the exit and its message come
        # before the Darwin guard, so this pins the whole branch-ordering regression.
        for script in (BUILD_IPA, BUILD_SIM):
            with self.subTest(script=script.name):
                result = run_script(script, "potato")
                self.assertEqual(result.returncode, 64, result.stderr)
                self.assertIn("unknown iOS target 'potato' (device|simulator)", result.stderr)

    @unittest.skipIf(
        DARWIN,
        "a macOS machine passes the Darwin guard, and the test would run a full Xcode build",
    )
    def test_known_targets_reach_the_platform_guard(self):
        for target in ("device", "simulator"):
            with self.subTest(target=target):
                result = run_script(BUILD_SIM, target)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("needs macOS with Xcode", result.stderr)

    @unittest.skipIf(
        DARWIN,
        "a macOS machine passes the Darwin guard, and the test would run a full Xcode build",
    )
    def test_the_wrapper_default_passes_the_target_check(self):
        result = run_script(BUILD_SIM)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("needs macOS with Xcode", result.stderr)
        self.assertNotIn("unknown iOS target", result.stderr)


if __name__ == "__main__":
    unittest.main()
