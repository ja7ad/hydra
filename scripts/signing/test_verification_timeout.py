import importlib.util
from pathlib import Path
import sys
import unittest

spec = importlib.util.spec_from_file_location("verification", Path(__file__).with_name("run-verification.py"))
verification = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verification)


class VerificationTimeoutTests(unittest.TestCase):
    def test_success(self):
        self.assertEqual(verification.run([sys.executable, "-c", "pass"]), 0)

    def test_failure_is_preserved(self):
        self.assertEqual(verification.run([sys.executable, "-c", "raise SystemExit(7)"]), 7)

    def test_stuck_process_is_killed_and_rejected(self):
        self.assertEqual(verification.run([sys.executable, "-c", "import time; time.sleep(60)"], timeout=0.1), 1)

    def test_entry_point_passes_signing_inputs_without_shell_expansion(self):
        import os
        import runpy
        from unittest.mock import patch

        inputs = {
            "UNSIGNED_DIRECTORY": "unsigned files",
            "SIGNED_DIRECTORY": "signed files",
            "CERTIFICATE_THUMBPRINT": "A" * 40,
            "SIGNING_POLICY": "test-signing",
        }
        with patch.dict(os.environ, inputs), patch("subprocess.run") as child:
            child.return_value.returncode = 7
            with self.assertRaises(SystemExit) as result:
                runpy.run_path(str(Path(__file__).with_name("run-verification.py")), run_name="__main__")
            self.assertEqual(result.exception.code, 7)
            args = child.call_args.args[0]
            self.assertEqual(args[:4], ["pwsh", "-NoProfile", "-NonInteractive", "-File"])
            self.assertTrue(args[4].endswith("verify-authenticode.ps1"))
            self.assertEqual(args[5:], [
                "-UnsignedDirectory", inputs["UNSIGNED_DIRECTORY"],
                "-SignedDirectory", inputs["SIGNED_DIRECTORY"],
                "-CertificateThumbprint", inputs["CERTIFICATE_THUMBPRINT"],
                "-SigningPolicy", inputs["SIGNING_POLICY"],
            ])
            self.assertEqual(child.call_args.kwargs, {"timeout": 300, "check": False})
