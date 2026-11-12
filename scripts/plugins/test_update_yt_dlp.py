#!/usr/bin/env python3
"""Check the yt-dlp release bump: what it compares, what it runs and what it rewrites."""
import importlib.util
from pathlib import Path
import tomllib
import unittest

spec = importlib.util.spec_from_file_location('update_yt_dlp', Path(__file__).with_name('update-yt-dlp.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ReleaseBumpTests(unittest.TestCase):
    def test_releases_compare_as_dates_not_text(self):
        self.assertLess(module.release_number('2026.9.27'), module.release_number('2026.10.26'))
        self.assertLess(module.release_number('2026.10.26'), module.release_number('2026.10.26.232945'))
        self.assertEqual(module.release_number('2026.08.19'), module.release_number('2026.8.19'))

    def test_checksum_is_taken_for_the_exact_asset_only(self):
        sums = 'aaa  yt-dlp\nbbb *yt-dlp_macos\nccc  yt-dlp_macos.zip\n'
        self.assertEqual(module.checksum_for(sums, 'yt-dlp_macos'), 'bbb')
        self.assertEqual(module.checksum_for(sums, 'yt-dlp'), 'aaa')
        self.assertIsNone(module.checksum_for(sums, 'yt-dlp_linux'))

    def test_only_the_patch_number_moves(self):
        self.assertEqual(module.bump_patch('0.2.0'), '0.2.1')
        self.assertEqual(module.bump_patch('1.4.9'), '1.4.10')

    def test_templates_expand_to_the_command_lines_the_plugin_runs(self):
        template = ['--ignore-config', '--no-js-runtimes', '--js-runtimes', '*', '--extractor-args', '*', '--cookies', '{cookie_file}', '--', '*']
        self.assertEqual(
            module.command_line(template, 'https://youtu.be/x', Path('/tmp/jar.txt')),
            ['--ignore-config', '--no-js-runtimes', '--js-runtimes', 'node', '--extractor-args', 'youtube:lang=en', '--cookies', '/tmp/jar.txt', '--', 'https://youtu.be/x'],
        )

    def test_a_refused_option_is_told_apart_from_a_refused_video(self):
        self.assertTrue(module.flags_rejected(2, 'Usage: yt-dlp [OPTIONS] URL\n\nyt-dlp: error: no such option: --js-runtimes'))
        self.assertFalse(module.flags_rejected(1, 'ERROR: [youtube] x: Sign in to confirm you’re not a bot.'))
        self.assertFalse(module.flags_rejected(0, 'WARNING: something'))

    def test_every_declared_extraction_template_can_be_expanded(self):
        manifest = tomllib.loads((module.PLUGIN / 'hydra-plugin.toml').read_text())
        for entry in manifest['permissions']['exec']:
            line = module.command_line(entry['args'], 'https://youtu.be/x', Path('jar'))
            self.assertNotIn('*', line)
            self.assertNotIn('{cookie_file}', line)

    def test_a_bump_rewrites_the_pin_and_the_patch_version_everywhere(self):
        release = module.pinned_release((module.PLUGIN / 'src/backend.rs').read_text())
        version = tomllib.loads((module.PLUGIN / 'hydra-plugin.toml').read_text())['version']
        files = module.rewritten(release, '2099.01.02', version, module.bump_patch(version))
        self.assertEqual(module.pinned_release(files['src/backend.rs']), '2099.01.02')
        self.assertIn('official 2099.01.02 release', files['README.md'])
        bumped = module.bump_patch(version)
        self.assertEqual(tomllib.loads(files['hydra-plugin.toml'])['version'], bumped)
        self.assertEqual(tomllib.loads(files['Cargo.toml'])['package']['version'], bumped)
        locked = next(p for p in tomllib.loads(files['Cargo.lock'])['package'] if p['name'] == 'hydra-youtube')
        self.assertEqual(locked['version'], bumped)
        self.assertEqual(module.pinned_release((module.PLUGIN / 'src/backend.rs').read_text()), release, 'rewritten() must not write')

    def test_a_bump_refuses_to_guess_when_the_files_do_not_match(self):
        with self.assertRaises(SystemExit):
            module.rewritten('1999.01.01', '2099.01.02', '0.0.0', '0.0.1')


if __name__ == '__main__':
    unittest.main()
