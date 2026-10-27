"""An empty cross-architecture tool report must never count as safe TLS."""
import importlib.util
from pathlib import Path
import unittest

REPO = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("v8_linux_tls", REPO / "scripts/lib/v8_linux_tls.py")
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


class LinuxTlsTest(unittest.TestCase):
    def test_observed_dynamic_arm64_and_x64_are_accepted(self):
        helper.validate({"R_AARCH64_TLSDESC_CALL": 335}, "aarch64")
        helper.validate({"R_X86_64_DTPOFF32": 10}, "x86_64")

    def test_each_local_exec_dialect_is_rejected(self):
        for arch, key in (("aarch64", "R_AARCH64_TLSLE_ADD_TPREL_HI12"),
                          ("x86_64", "R_X86_64_TPOFF32")):
            with self.subTest(arch=arch), self.assertRaisesRegex(ValueError, "local-exec"):
                helper.validate({key: 1}, arch)

    def test_empty_and_wrong_arch_reports_are_rejected(self):
        for counts in ({}, {"R_X86_64_DTPOFF32": 1}):
            with self.assertRaisesRegex(ValueError, "no aarch64 relocations"):
                helper.validate(counts, "aarch64")
