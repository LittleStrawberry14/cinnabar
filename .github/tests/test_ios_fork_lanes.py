"""The fork's iOS lanes stay self-contained and share their recipes through the composite
actions, so an edit that silently splits the pinned toolchain, carrier compile, cache sharing,
or artifact names fails these checks."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
import yaml


GITHUB = Path(__file__).resolve().parents[1]
TOOLCHAIN_USE = "./.github/actions/ios-toolchain"
CARRIERS_USE = "./.github/actions/ios-carriers"
CACHE_RESTORE_USE = "actions/cache/restore@v5"
CACHE_SAVE_USE = "actions/cache/save@v5"
DEVICE_CACHE_PREFIX = "ios-${{ runner.os }}-"
# PyYAML resolves a bare `on` key to True, so accept both spellings.
ON = "on", True


def load(path):
    doc = yaml.safe_load(path.read_text())
    for key in ON:
        if key in doc:
            return doc, doc[key]
    raise AssertionError(f"{path} declares no triggers")


class IosForkLaneTests(unittest.TestCase):
    """Both fork lanes reuse the device lane's cache and claim disjoint artifacts."""

    def setUp(self):
        self.sim, sim_triggers = load(GITHUB / "workflows/ios-sim.yml")
        self.assets, assets_triggers = load(GITHUB / "workflows/ios-assets.yml")
        self.device, _ = load(GITHUB / "workflows/ios.yml")
        self.lanes = (
            ("ios-sim", self.sim, sim_triggers, "aarch64-apple-ios-sim"),
            ("ios-assets", self.assets, assets_triggers, "aarch64-apple-ios"),
        )
        self.device_runs_on = next(iter(self.device["jobs"].values()))["runs-on"]

    def steps_of(self, doc):
        return [step for job in doc["jobs"].values() for step in job["steps"]]

    def uses_steps(self, doc, action):
        return [step for step in self.steps_of(doc) if step.get("uses") == action]

    def test_lane_triggers_stay_fork_owned(self):
        for name, doc, triggers, _ in self.lanes:
            with self.subTest(lane=name):
                self.assertEqual(triggers["push"]["branches"], ["ios/**"])
                self.assertIn("workflow_dispatch", triggers)

    def test_each_lane_runs_where_the_device_lane_runs(self):
        for name, doc, _, _ in self.lanes:
            runs_on = {job["runs-on"] for job in doc["jobs"].values()}
            with self.subTest(lane=name):
                self.assertEqual(runs_on, {self.device_runs_on})

    def test_each_lane_installs_its_target_through_the_shared_recipe(self):
        for name, doc, _, target in self.lanes:
            with self.subTest(lane=name):
                uses = self.uses_steps(doc, TOOLCHAIN_USE)
                self.assertEqual([step["with"]["target"] for step in uses], [target])

    def test_the_shared_recipes_pin_their_sources(self):
        toolchain = yaml.safe_load((GITHUB / "actions/ios-toolchain/action.yml").read_text())
        carriers = yaml.safe_load((GITHUB / "actions/ios-carriers/action.yml").read_text())
        toolchain_recipe = toolchain["runs"]["steps"][0]["run"]
        self.assertIn("rust-toolchain.toml", toolchain_recipe)
        self.assertIn("$TARGET_TRIPLE", toolchain_recipe)
        carrier_recipe = carriers["runs"]["steps"][0]["run"]
        self.assertIn("vanilla-pack --source-manifest assets/vanilla-source.json", carrier_recipe)
        self.assertIn("prepare --accept-eula", carrier_recipe)

    def test_lanes_compile_carriers_and_inject_them_into_a_package(self):
        for name, doc, triggers, _ in self.lanes:
            with self.subTest(lane=name):
                self.assertTrue(self.uses_steps(doc, CARRIERS_USE))
                self.assertTrue(
                    any("packaging/ios/add-assets.py" in step.get("run", "")
                        for step in self.steps_of(doc))
                )

    def test_lane_caches_pin_the_vanilla_source_and_serve_from_the_device_lane(self):
        for name, doc, _, _ in self.lanes:
            with self.subTest(lane=name):
                restore = self.uses_steps(doc, CACHE_RESTORE_USE)
                save = self.uses_steps(doc, CACHE_SAVE_USE)
                self.assertEqual(len(restore), 1)
                self.assertEqual(len(save), 1)
                cache = restore[0]["with"]
                self.assertIn("'assets/vanilla-source.json'", cache["key"])
                self.assertIn(DEVICE_CACHE_PREFIX, cache["restore-keys"])
                self.assertEqual(cache["path"], "${{ env.FORK_CACHE_PATHS }}")
                self.assertEqual(save[0]["with"]["key"],
                                 "${{ steps.cache.outputs.cache-primary-key }}")

    def test_every_lane_cache_holds_the_pinned_vanilla_pack(self):
        manifest = (GITHUB.parent / "assets/vanilla-source.json")
        cache_dir = json.loads(manifest.read_text())["cache_dir"]
        self.assertTrue(cache_dir.startswith(".local/"))
        for name, doc, _, _ in self.lanes:
            paths = [
                line.strip()
                for job in doc["jobs"].values()
                for line in job.get("env", {}).get("FORK_CACHE_PATHS", "").splitlines()
                if line.strip()
            ]
            with self.subTest(lane=name):
                self.assertTrue(
                    any(cache_dir.startswith(f"{path}/") or cache_dir == path for path in paths),
                    f"{name} never caches {cache_dir}",
                )

    def test_the_carrier_upload_is_gated_to_the_fork(self):
        assets_job = self.assets["jobs"]["assets"]
        self.assertIn("github.repository != 'cinnabar-mc/cinnabar'", assets_job["if"])
        sim_jobs = {name: job["if"] for name, job in self.sim["jobs"].items() if "if" in job}
        self.assertEqual(sim_jobs, {}, "the launch gate must also run on upstream's pushes")

    def test_lanes_claim_disjoint_artifact_names(self):
        expected = {
            "ios-sim": {"ios-sim-launch"},
            "ios-assets": {"cinnabar-ios-assets"},
        }
        device_names = {
            step["with"]["name"]
            for step in self.uses_steps(self.device, "actions/upload-artifact@v7")
        }
        all_names = set(device_names)
        for name, doc, _, _ in self.lanes:
            uploads = {step["with"]["name"]
                       for step in self.uses_steps(doc, "actions/upload-artifact@v7")}
            with self.subTest(lane=name):
                self.assertEqual(uploads, expected[name])
            all_names |= uploads
        self.assertEqual(len(all_names), len({*all_names}), "artifact names must stay disjoint")

    RUNNER_DEVICE_LINES = (
        ("iOS 26.2", (
            ("iPhone 17 Pro", "B34C2F57-B98B-4C84-96E2-3170887BA2DB"),
            ("iPhone 17 Pro Max", "8B500E0C-CA36-4F12-853D-3B697FBEA084"),
            ("iPhone Air", "3E8A6C6F-3253-4050-8CF7-715CB81ADF06"),
            ("iPad Pro 13-inch (M5)", "3E32E66C-0037-43D0-BA3B-F54380A0040D"),
        )),
        ("iOS 26.4", (
            ("iPhone 17 Pro", "0183276C-C5F7-4A53-B0AF-C3D9117595F6"),
            ("iPhone 17e", "49A10376-191D-4B27-82E7-4EC71948B0BB"),
        )),
        ("tvOS 26.2", (("Apple TV", "D5618DF6-404A-4F88-9963-6D20E40FAF13"),)),
    )

    def runner_listing(self, state):
        """The macos-26 runner's `simctl list devices available` shape: every device line
        ends with a space and current Xcode spells the idle state `Shutdown`."""
        lines = ["== Devices =="]
        for section, devices in self.RUNNER_DEVICE_LINES:
            lines.append(f"-- {section} --")
            lines.extend(f"    {name} ({udid}) ({state}) " for name, udid in devices)
        return "\n".join(lines) + "\n"

    def boot_step_script(self):
        return next(
            step["run"]
            for job in self.sim["jobs"].values()
            for step in job["steps"]
            if step.get("name") == "Boot an available simulator"
        )

    def pick_device(self, script, want, state):
        """Runs the boot step against a fake xcrun serving the canned listing and returns
        the udid it wrote to GITHUB_OUTPUT."""
        with tempfile.TemporaryDirectory() as bin_dir:
            listing = Path(bin_dir, "listing.txt")
            listing.write_text(self.runner_listing(state))
            shim = Path(bin_dir, "xcrun")
            shim.write_text(
                '#!/bin/sh\n'
                '[ "$1 $2 $3 $4" = "simctl list devices available" ] && cat "$FAKE_LISTING"\n'
                'exit 0\n'
            )
            shim.chmod(0o755)
            github_output = Path(bin_dir, "github_output")
            env = dict(
                os.environ,
                PATH=f"{bin_dir}:{os.environ['PATH']}",
                FAKE_LISTING=str(listing),
                GITHUB_OUTPUT=str(github_output),
                SIM_DEVICE_INPUT=want,
            )
            run = subprocess.run(["bash", "-c", script], env=env, text=True,
                                 capture_output=True, timeout=60)
            self.assertEqual(run.returncode, 0, run.stderr)
            written = dict(
                line.split("=", 1) for line in github_output.read_text().splitlines()
            )
            return written["udid"]

    def test_the_boot_step_picks_a_device_from_the_runner_listing(self):
        script = self.boot_step_script()
        newest = "B34C2F57-B98B-4C84-96E2-3170887BA2DB"
        for want, expected in (
            ("iPhone 17 Pro", newest),
            ("iPad Pro 13-inch (M5)", "3E32E66C-0037-43D0-BA3B-F54380A0040D"),
            ("iPhone 17e", "49A10376-191D-4B27-82E7-4EC71948B0BB"),
            ("Nothing Real", newest),
        ):
            with self.subTest(device=want):
                self.assertEqual(self.pick_device(script, want, "Shutdown"), expected)

    def test_the_boot_step_reuses_a_booted_device(self):
        script = self.boot_step_script()
        picked = self.pick_device(script, "iPhone 17 Pro", "Booted")
        self.assertEqual(picked, "B34C2F57-B98B-4C84-96E2-3170887BA2DB")


if __name__ == "__main__":
    unittest.main()
