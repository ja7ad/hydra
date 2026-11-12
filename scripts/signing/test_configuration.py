import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name('configure-signing.sh')
TEST_THUMBPRINT = '4924FFE138E36949CA3EDDF03252903CDB65B2AE'


class ConfigurationTests(unittest.TestCase):
    def configure(self, **overrides):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / 'output'
            env = {
                'PATH': os.environ['PATH'],
                'SIGNPATH_API_TOKEN': 'fixture-token',
                'SIGNING_POLICY': 'test-signing',
                'RELEASE_ENABLED': '',
                'RELEASE_THUMBPRINT': '',
                'GITHUB_OUTPUT': str(output),
            }
            env.update(overrides)
            result = subprocess.run(['bash', str(SCRIPT)], env=env, text=True, capture_output=True)
            values = dict(line.split('=', 1) for line in output.read_text().splitlines()) if output.exists() else {}
            return result, values

    def test_onboarding_uses_pinned_test_certificate_without_release_configuration(self):
        result, values = self.configure()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(values, {'signing_policy': 'test-signing', 'certificate_thumbprint': TEST_THUMBPRINT})

    def test_missing_api_token_is_reported_without_disclosing_values(self):
        result, values = self.configure(SIGNPATH_API_TOKEN='')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('SIGNPATH_API_TOKEN', result.stdout)
        self.assertNotIn('fixture-token', result.stdout + result.stderr)
        self.assertFalse(values)

    def test_production_requires_explicit_activation(self):
        result, values = self.configure(SIGNING_POLICY='release-signing', RELEASE_THUMBPRINT='A' * 40)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('SIGNPATH_RELEASE_SIGNING_ENABLED', result.stdout)
        self.assertFalse(values)

    def test_production_rejects_test_certificate_and_invalid_thumbprints(self):
        for thumbprint in (TEST_THUMBPRINT, '', 'not-a-thumbprint', 'A' * 39):
            with self.subTest(thumbprint=thumbprint):
                result, values = self.configure(SIGNING_POLICY='release-signing', RELEASE_ENABLED='true', RELEASE_THUMBPRINT=thumbprint)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(values)

    def test_production_uses_only_explicit_production_certificate(self):
        result, values = self.configure(SIGNING_POLICY='release-signing', RELEASE_ENABLED='true', RELEASE_THUMBPRINT='B' * 40)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(values, {'signing_policy': 'release-signing', 'certificate_thumbprint': 'B' * 40})

    def test_unknown_policy_is_refused_instead_of_falling_back(self):
        result, values = self.configure(SIGNING_POLICY='unknown')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Unsupported signing policy', result.stdout)
        self.assertFalse(values)


class ModeSelectionTests(unittest.TestCase):
    def select(self, value=None):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / 'output'
            env = {'PATH': os.environ['PATH'], 'GITHUB_OUTPUT': str(output)}
            if value is not None:
                env['SIGNPATH_TEST_SIGNING'] = value
            result = subprocess.run(['bash', str(SCRIPT.with_name('select-signing-mode.sh'))],
                                    env=env, text=True, capture_output=True)
            return result, output.read_text() if output.exists() else ''

    def test_unconfigured_releases_use_the_test_certificate(self):
        for value in (None, '', 'true'):
            with self.subTest(value=value):
                result, output = self.select(value)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(output, 'test_signing=true\n')

    def test_disabling_test_mode_selects_production(self):
        result, output = self.select('false')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(output, 'test_signing=false\n')

    def test_malformed_flags_fail_instead_of_selecting_a_certificate(self):
        for value in ('False', '0', 'yes', ' false ', 'false\ntest_signing=true'):
            with self.subTest(value=value):
                result, output = self.select(value)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('must be true or false', result.stdout)
                self.assertFalse(output)


if __name__ == '__main__':
    unittest.main()
