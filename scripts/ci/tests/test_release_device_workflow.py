import unittest
from pathlib import Path

import yaml


class ReleaseDeviceWorkflowTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.release_text = Path(".github/workflows/release.yml").read_text(
            encoding="utf-8"
        )
        cls.release = yaml.safe_load(cls.release_text)
        cls.device = yaml.safe_load(
            Path(".github/workflows/device-test.yml").read_text(encoding="utf-8")
        )

    @staticmethod
    def _step(job, name):
        matches = [step for step in job["steps"] if step.get("name") == name]
        if len(matches) != 1:
            raise AssertionError(f"expected exactly one step named {name!r}")
        return matches[0]

    def test_android_device_evidence_job_is_shaped_and_wired_correctly(self):
        # The job was introduced needing a `self-hosted, android-device` runner
        # (none is registered) and the sibling device-test-suite checkout's
        # install.sh/run.sh (that
        # repo ships neither), while gating `publish`. So the first tagged release
        # after it landed could not publish -- the job queued forever. It is now
        # opt-in: it runs, and gates the release, only where MIGO_DEVICE_EVIDENCE
        # is set. This test still pins its shape so that turning it on is the only
        # thing left to do; when it is on, publish must depend on it again.
        jobs = self.release["jobs"]
        evidence = jobs["release-android-device-evidence"]
        self.assertEqual(evidence["needs"], ["release-android"])
        self.assertEqual(evidence["runs-on"], ["self-hosted", "android-device"])
        self.assertEqual(
            evidence["if"], "${{ vars.MIGO_DEVICE_EVIDENCE == 'true' }}"
        )
        # While the job is guarded off, publish must not wait on a job that will
        # not run. The comment beside `publish.needs` records that it goes back
        # the moment MIGO_DEVICE_EVIDENCE is set.
        self.assertNotIn(
            "release-android-device-evidence", jobs["publish"]["needs"]
        )
        publish_src = self.release_text
        self.assertIn("MIGO_DEVICE_EVIDENCE is set", publish_src)

        download = self._step(
            evidence, "Download the hosted builder's Android release bytes"
        )
        self.assertEqual(download["with"]["name"], "release-assets-android")
        self.assertEqual(download["with"]["path"], "dist/device-input")

        run = self._step(evidence, "Install, measure, gate, and seal release evidence")[
            "run"
        ]
        self.assertIn('ARTIFACT="dist/device-input/', run)
        self.assertIn("run_android_device_evidence.sh", run)
        self.assertIn('--source-revision "$GITHUB_SHA"', run)

    def test_device_job_provisions_host_toolchain_without_rebuilding(self):
        evidence = self.release["jobs"]["release-android-device-evidence"]
        action_names = {step.get("name") for step in evidence["steps"]}
        self.assertIn("Setup Java for the external host application", action_names)
        self.assertIn("Setup Android SDK and adb", action_names)
        bodies = "\n".join(str(step.get("run", "")) for step in evidence["steps"])
        self.assertNotIn("build-aar.sh", bodies)
        self.assertNotIn("cargo build", bodies)

        upload = self._step(evidence, "Upload sealed device evidence")
        self.assertEqual(upload["with"]["if-no-files-found"], "error")

    def test_nightly_workflow_keeps_one_device_owner_and_no_skip_path(self):
        trigger = self.device.get("on", self.device.get(True))
        self.assertIn("workflow_dispatch", trigger)
        self.assertTrue(trigger.get("schedule"))
        self.assertEqual(list(self.device["jobs"]), ["build-install-and-gate"])
        job = self.device["jobs"]["build-install-and-gate"]
        self.assertEqual(job["runs-on"], ["self-hosted", "android-device"])
        bodies = "\n".join(str(step.get("run", "")) for step in job["steps"])
        self.assertIn("build-aar.sh --product-profile full release", bodies)
        self.assertIn("run_android_device_evidence.sh", bodies)
        self.assertNotIn("pre-existing", bodies.lower())
        upload = self._step(job, "Upload artifact-bound device evidence")
        self.assertEqual(upload["with"]["if-no-files-found"], "error")


if __name__ == "__main__":
    unittest.main()
