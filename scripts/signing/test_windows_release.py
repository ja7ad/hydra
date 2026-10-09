import importlib.util
import pathlib
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location('windows_release', pathlib.Path(__file__).with_name('windows-release.py'))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class SigningPackagingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)

    def write(self, name, data=b'fixture'):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def archive(self, name, entries):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(path, 'w') as archive:
            for name, contents in entries.items():
                archive.writestr(name, contents)
        return path

    def plugin(self, platform='windows-x86_64', module='torrent-windows-x86_64.dll', payload=b'original DLL'):
        manifest = f'[[native_modules]]\nplatform = "{platform}"\nmodule = "{module}"\n'
        entries = {'hydra-plugin.toml': manifest, module: payload}
        self.archive('original-unsigned/unsigned-torrent-download-windows-x86_64.hyaplugin', entries)
        self.archive('source/plugins/bundled/native/windows-x86_64/torrent-download-windows-x86_64.hyaplugin', entries)
        self.write('source/target/debug/hydra-plugin.exe')

    def app(self, target='x86_64-pc-windows-msvc'):
        for binary in release.BINARIES:
            self.write(f'source/target/{target}/release/{binary}', binary.encode())
        for file in ('scripts/windows/portable/HydraPortable.exe', 'scripts/install-native-host.ps1', 'uninstall.ps1'):
            self.write(f'source/{file}')
        self.write('install.ps1', b'current root installer')
        self.write('uninstall.ps1', b'current root uninstaller')
        return target

    def test_flat_and_nested_archives_round_trip(self):
        archive = self.archive('input.zip', {'bundle/hydra.exe': b'EXE', 'LICENSE': b'license'})
        release.extract(archive, self.root / 'stage')
        release.zip_directory(self.root / 'stage', self.root / 'output.zip')
        with zipfile.ZipFile(self.root / 'output.zip') as output:
            self.assertEqual(output.read('bundle/hydra.exe'), b'EXE')
            self.assertEqual(output.read('LICENSE'), b'license')

    def test_archive_rejects_traversal_and_windows_aliases_before_writing(self):
        for bad in ('../escape', '/absolute', 'C:/drive', 'folder\\file', './file', 'x/../file', 'file:stream'):
            with self.subTest(name=bad):
                archive = self.archive('bad.zip', {'safe': b'safe', bad: b'bad'})
                with self.assertRaises(ValueError):
                    release.extract(archive, self.root / 'stage')
                self.assertFalse((self.root / 'stage/safe').exists())

    def test_archive_rejects_case_collisions_and_symlinks(self):
        archive = self.archive('duplicate.zip', {'File': b'a', 'file': b'b'})
        with self.assertRaises(ValueError):
            release.extract(archive, self.root / 'stage')
        info = zipfile.ZipInfo('link')
        info.external_attr = (0o120777 << 16)
        with zipfile.ZipFile(self.root / 'link.zip', 'w') as archive:
            archive.writestr(info, '../outside')
        with self.assertRaises(ValueError):
            release.extract(self.root / 'link.zip', self.root / 'stage')

    def test_recovered_dll_must_equal_the_released_plugin(self):
        self.plugin()
        release.prepare_plugin(self.root, 'x86_64')
        self.assertEqual((self.root / 'unsigned-native/torrent-windows-x86_64.dll').read_bytes(), b'original DLL')
        self.assertTrue((self.root / 'unsigned-native/hydra-plugin.exe').is_file())

    def test_wrong_platform_or_module_is_rejected(self):
        for platform, module in [('linux-x86_64', 'torrent.dll'), ('windows-x86_64', 'native/torrent.dll')]:
            with self.subTest(platform=platform, module=module):
                self.plugin(platform, module)
                with self.assertRaises(ValueError):
                    release.prepare_plugin(self.root, 'x86_64')

    def test_changed_recovered_dll_is_rejected(self):
        self.plugin()
        self.archive('source/plugins/bundled/native/windows-x86_64/torrent-download-windows-x86_64.hyaplugin',
                     {'torrent-windows-x86_64.dll': b'different DLL'})
        with self.assertRaisesRegex(ValueError, 'differs'):
            release.prepare_plugin(self.root, 'x86_64')

    def test_signed_dll_is_repacked_before_publisher_signing(self):
        self.plugin()
        release.prepare_plugin(self.root, 'x86_64')
        self.write('signed-native/torrent-windows-x86_64.dll', b'signed DLL')
        with patch.object(release.subprocess, 'run') as run:
            release.repack_plugin(self.root, 'x86_64')
        self.assertEqual((self.root / 'plugin-staging/torrent-windows-x86_64.dll').read_bytes(), b'signed DLL')
        self.assertEqual([call.args[0][1] for call in run.call_args_list], ['pack', 'sign', 'validate'])
        self.assertIn('HYDRA_PLUGIN_SIGNING_KEY', run.call_args_list[1].args[0])

    def test_uninstaller_is_exported_then_signed_copy_is_embedded(self):
        original = b'Section\n  WriteUninstaller "$INSTDIR\\uninstall.exe"\nSectionEnd\n'
        path = self.write('source/scripts/windows/hydra-installer.nsi', original)
        release.installer_hook(self.root, 'export')
        self.assertIn('!uninstfinalize', path.read_text())
        release.installer_hook(self.root, 'import')
        self.assertIn('File /oname=uninstall.exe', path.read_text())
        self.assertNotIn('WriteUninstaller', path.read_text())
        self.assertEqual((self.root / 'original-installer.nsi').read_bytes(), original)

    def test_changed_uninstaller_declaration_fails_closed(self):
        self.write('source/scripts/windows/hydra-installer.nsi', b'changed installer')
        with self.assertRaises(ValueError):
            release.installer_hook(self.root, 'export')

    def test_current_remote_scripts_are_separate_from_historical_bundle(self):
        target = self.app()
        release.stage_app(self.root, target)
        self.assertEqual((self.root / 'unsigned-app/remote-installers/install.ps1').read_bytes(), b'current root installer')
        self.assertNotEqual((self.root / 'unsigned-app/uninstall.ps1').read_bytes(),
                            (self.root / 'unsigned-app/remote-installers/uninstall.ps1').read_bytes())
        for path in (self.root / 'unsigned-app').rglob('*'):
            if path.is_file():
                self.write('signed-app/' + path.relative_to(self.root / 'unsigned-app').as_posix(), b'signed ' + path.read_bytes())
        release.install_signed_app(self.root, target)
        self.assertTrue((self.root / f'source/target/{target}/release/hydra.exe').read_bytes().startswith(b'signed '))
        self.assertEqual((self.root / 'install.ps1').read_bytes(), b'current root installer')

    def test_repacked_archives_preserve_documents_and_use_signed_executables(self):
        target = self.app()
        for prefix, binaries in [('hydra', release.BINARIES[:4]), ('hydra-cli', ['hydra.exe']), ('hydra-plugin-cli', ['hydra-plugin.exe'])]:
            entries = {f'{prefix}/{binary}': b'unsigned' for binary in binaries}
            entries[f'{prefix}/LICENSE'] = b'license'
            self.archive(f'original-windows/{prefix}-1.1.0-windows-amd64.zip', entries)
        self.write('source/target/dist/hydra-1.1.0-windows-portable-amd64.zip')
        self.write('source/plugins/bundled/native/windows-x86_64/torrent-download-windows-x86_64.hyaplugin')
        release.repack_archives(self.root, 'x86_64', target)
        with zipfile.ZipFile(self.root / 'dist/hydra-1.1.0-windows-amd64.zip') as archive:
            self.assertEqual(archive.read('hydra/hydra.exe'), b'hydra.exe')
            self.assertEqual(archive.read('hydra/LICENSE'), b'license')
        self.assertEqual(len(list((self.root / 'dist').iterdir())), 5)

    def test_arm64_plugin_cli_archive_contains_the_signed_arm64_tool(self):
        target = self.app('aarch64-pc-windows-msvc')
        for prefix, binaries in [('hydra', release.BINARIES[:4]), ('hydra-cli', ['hydra.exe']), ('hydra-plugin-cli', ['hydra-plugin.exe'])]:
            entries = {f'{prefix}/{binary}': b'unsigned ARM64' for binary in binaries}
            self.archive(f'original-windows/{prefix}-1.1.0-windows-arm64.zip', entries)
        self.write(f'source/target/{target}/release/hydra-plugin.exe', b'signed ARM64 plugin CLI')
        self.write('source/target/dist/hydra-1.1.0-windows-portable-arm64.zip')
        self.write('source/plugins/bundled/native/windows-aarch64/torrent-download-windows-aarch64.hyaplugin')
        release.repack_archives(self.root, 'aarch64', target)
        with zipfile.ZipFile(self.root / 'dist/hydra-plugin-cli-1.1.0-windows-arm64.zip') as archive:
            self.assertEqual(archive.read('hydra-plugin-cli/hydra-plugin.exe'), b'signed ARM64 plugin CLI')

    def test_repack_rejects_unexpected_executable(self):
        target = self.app()
        self.archive('original-windows/hydra-1.1.0-windows-amd64.zip', {'hydra/unexpected.exe': b'EXE'})
        with self.assertRaisesRegex(ValueError, 'unexpected executable'):
            release.repack_archives(self.root, 'x86_64', target)


    def test_archive_size_limit_rejects_before_extraction(self):
        entry = zipfile.ZipInfo('large.dll')
        entry.file_size = 1024 * 1024 * 1024 + 1
        with patch.object(release.zipfile, 'ZipFile') as opened:
            archive = opened.return_value.__enter__.return_value
            archive.infolist.return_value = [entry]
            with self.assertRaisesRegex(ValueError, 'limit'):
                release.extract('large.zip', self.root / 'stage')
            archive.extractall.assert_not_called()

    def test_repack_rejects_missing_binary_and_unknown_scripts(self):
        target = self.app()
        name = 'original-windows/hydra-1.1.0-windows-amd64.zip'
        self.archive(name, {'hydra/hydra.exe': b'EXE'})
        with self.assertRaisesRegex(ValueError, 'count'):
            release.repack_archives(self.root, 'x86_64', target)
        entries = {f'hydra/{binary}': b'EXE' for binary in release.BINARIES[:4]}
        entries['hydra/scripts/unknown.ps1'] = b'script'
        self.archive(name, entries)
        with self.assertRaisesRegex(ValueError, 'PowerShell'):
            release.repack_archives(self.root, 'x86_64', target)

    def test_repack_rejects_duplicate_binaries_even_when_count_matches(self):
        target = self.app()
        entries = {f'duplicate-{index}/hydra.exe': b'EXE' for index in range(4)}
        self.archive('original-windows/hydra-1.1.0-windows-amd64.zip', entries)
        with self.assertRaisesRegex(ValueError, 'count or names'):
            release.repack_archives(self.root, 'x86_64', target)

    def test_repack_uses_verified_powershell_scripts(self):
        target = self.app()
        for prefix, binaries in [('hydra', release.BINARIES[:4]), ('hydra-cli', ['hydra.exe']), ('hydra-plugin-cli', ['hydra-plugin.exe'])]:
            entries = {f'{prefix}/{binary}': b'EXE' for binary in binaries}
            if prefix == 'hydra':
                entries['hydra/scripts/uninstall.ps1'] = b'old script'
            self.archive(f'original-windows/{prefix}-1.1.0-windows-amd64.zip', entries)
        self.write('signed-app/uninstall.ps1', b'signed script')
        self.write('source/target/dist/hydra-1.1.0-windows-portable-amd64.zip')
        self.write('source/plugins/bundled/native/windows-x86_64/torrent-download-windows-x86_64.hyaplugin')
        release.repack_archives(self.root, 'x86_64', target)
        with zipfile.ZipFile(self.root / 'dist/hydra-1.1.0-windows-amd64.zip') as archive:
            self.assertEqual(archive.read('hydra/scripts/uninstall.ps1'), b'signed script')

    def test_cli_dispatches_every_stage_for_arm64(self):
        for command, function in [('prepare-plugin', 'prepare_plugin'), ('repack-plugin', 'repack_plugin'),
                                  ('export-uninstaller', 'installer_hook'), ('import-uninstaller', 'installer_hook'),
                                  ('stage-app', 'stage_app'), ('install-app', 'install_signed_app'), ('repack', 'repack_archives')]:
            with self.subTest(command=command), patch('sys.argv', ['windows-release.py', command, '--architecture', 'aarch64']), patch.object(release, function) as called:
                release.main()
                called.assert_called_once()


if __name__ == '__main__':
    unittest.main()
