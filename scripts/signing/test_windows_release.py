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

    def app(self, target='x86_64-pc-windows-msvc'):
        for binary in release.BINARIES:
            self.write(f'source/target/{target}/release/{binary}', binary.encode())
        for file in ('scripts/windows/portable/HydraPortable.exe', 'scripts/install-native-host.ps1', 'uninstall.ps1',
                     'scripts/windows/set-user-path.ps1'):
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
        self.assertTrue((self.root / 'source/scripts/windows/set-user-path.ps1').read_bytes().startswith(b'signed '))

    def test_repacked_archives_preserve_documents_and_use_signed_executables(self):
        target = self.app()
        for prefix, binaries in [('hydra', release.BINARIES[:4]), ('hydra-cli', ['hydra.exe']), ('hydra-plugin-cli', ['hydra-plugin.exe'])]:
            entries = {f'{prefix}/{binary}': b'unsigned' for binary in binaries}
            entries[f'{prefix}/LICENSE'] = b'license'
            self.archive(f'original-windows/{prefix}-1.1.0-windows-amd64.zip', entries)
        self.write('source/target/dist/hydra-1.1.0-windows-portable-amd64.zip')
        release.repack_archives(self.root, 'x86_64', target)
        with zipfile.ZipFile(self.root / 'dist/hydra-1.1.0-windows-amd64.zip') as archive:
            self.assertEqual(archive.read('hydra/hydra.exe'), b'hydra.exe')
            self.assertEqual(archive.read('hydra/LICENSE'), b'license')
        self.assertEqual(len(list((self.root / 'dist').iterdir())), 4)

    def test_arm64_plugin_cli_archive_contains_the_signed_arm64_tool(self):
        target = self.app('aarch64-pc-windows-msvc')
        for prefix, binaries in [('hydra', release.BINARIES[:4]), ('hydra-cli', ['hydra.exe']), ('hydra-plugin-cli', ['hydra-plugin.exe'])]:
            entries = {f'{prefix}/{binary}': b'unsigned ARM64' for binary in binaries}
            self.archive(f'original-windows/{prefix}-1.1.0-windows-arm64.zip', entries)
        self.write(f'source/target/{target}/release/hydra-plugin.exe', b'signed ARM64 plugin CLI')
        self.write('source/target/dist/hydra-1.1.0-windows-portable-arm64.zip')
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
        release.repack_archives(self.root, 'x86_64', target)
        with zipfile.ZipFile(self.root / 'dist/hydra-1.1.0-windows-amd64.zip') as archive:
            self.assertEqual(archive.read('hydra/scripts/uninstall.ps1'), b'signed script')

    def test_future_release_archives_use_the_requested_version(self):
        version = '2.3.4-rc.1'
        target = self.app('aarch64-pc-windows-msvc')
        for prefix, binaries in [('hydra', release.BINARIES[:4]), ('hydra-cli', ['hydra.exe']),
                                 ('hydra-plugin-cli', ['hydra-plugin.exe'])]:
            entries = {f'{prefix}/{binary}': b'unsigned' for binary in binaries}
            self.archive(f'original-windows/{prefix}-{version}-windows-arm64.zip', entries)
        self.write(f'source/target/dist/hydra-{version}-windows-portable-arm64.zip')
        release.repack_archives(self.root, 'aarch64', target, version)
        with zipfile.ZipFile(self.root / f'dist/hydra-{version}-windows-arm64.zip') as archive:
            self.assertEqual(archive.read('hydra/hydra.exe'), b'hydra.exe')
        self.assertFalse(list((self.root / 'dist').glob('*1.1.0*')))

    def test_ffi_repack_replaces_dll_and_preserves_static_library_and_header(self):
        entries = {'libhydra/bin/hydra.dll': b'unsigned DLL',
                   'libhydra/lib/hydra.lib': b'static library', 'libhydra/include/hydra.h': b'header'}
        self.archive('original-ffi/libhydra-0.9.0-aarch64-pc-windows-msvc.zip', entries)
        self.write('source/target/debug/hydra-plugin.exe')
        release.prepare_ffi(self.root)
        self.assertEqual((self.root / 'unsigned-native/ffi-0-hydra.dll').read_bytes(), b'unsigned DLL')
        self.write('signed-native/ffi-0-hydra.dll', b'signed DLL')
        release.repack_ffi(self.root)
        with zipfile.ZipFile(self.root / 'ffi-dist/libhydra-0.9.0-aarch64-pc-windows-msvc.zip') as archive:
            self.assertEqual(archive.read('libhydra/bin/hydra.dll'), b'signed DLL')
            self.assertEqual(archive.read('libhydra/lib/hydra.lib'), b'static library')
            self.assertEqual(archive.read('libhydra/include/hydra.h'), b'header')

    def test_ffi_signing_flattens_nested_dlls_without_filename_collisions(self):
        entries = {'libhydra/z/hydra.dll': b'unsigned second DLL',
                   'libhydra/lib/hydra.dll': b'unsigned first DLL',
                   'libhydra/include/hydra.h': b'header'}
        self.archive('original-ffi/libhydra-1.0.0-x86_64-pc-windows-msvc.zip', entries)
        self.write('source/target/debug/hydra-plugin.exe', b'plugin tool')
        release.prepare_ffi(self.root)
        unsigned = self.root / 'unsigned-native'
        self.assertEqual({path.name for path in unsigned.iterdir()},
                         {'hydra-plugin.exe', 'ffi-0-hydra.dll', 'ffi-1-hydra.dll'})
        self.assertEqual((unsigned / 'hydra-plugin.exe').read_bytes(), b'plugin tool')
        self.assertEqual((unsigned / 'ffi-0-hydra.dll').read_bytes(), b'unsigned first DLL')
        self.assertEqual((unsigned / 'ffi-1-hydra.dll').read_bytes(), b'unsigned second DLL')
        self.write('signed-native/ffi-1-hydra.dll', b'signed second DLL')
        self.write('signed-native/ffi-0-hydra.dll', b'signed first DLL')
        release.repack_ffi(self.root)
        with zipfile.ZipFile(self.root / 'ffi-dist/libhydra-1.0.0-x86_64-pc-windows-msvc.zip') as archive:
            self.assertEqual(set(archive.namelist()), set(entries))
            self.assertEqual(archive.read('libhydra/lib/hydra.dll'), b'signed first DLL')
            self.assertEqual(archive.read('libhydra/z/hydra.dll'), b'signed second DLL')
            self.assertEqual(archive.read('libhydra/include/hydra.h'), b'header')

    def test_ffi_requires_one_archive_with_dll_and_complete_signed_output(self):
        with self.assertRaisesRegex(ValueError, 'exactly one'):
            release.prepare_ffi(self.root)
        self.archive('original-ffi/ffi.zip', {'bundle/hydra.lib': b'static'})
        with self.assertRaisesRegex(ValueError, 'no DLL'):
            release.prepare_ffi(self.root)
        self.archive('original-ffi/ffi.zip', {'bundle/hydra.dll': b'unsigned'})
        self.write('source/target/debug/hydra-plugin.exe')
        release.prepare_ffi(self.root)
        with self.assertRaises(FileNotFoundError):
            release.repack_ffi(self.root)
        self.assertFalse(list((self.root / 'ffi-dist').glob('*.zip')))
        self.archive('original-ffi/duplicate.zip', {'bundle/hydra.dll': b'unsigned'})
        with self.assertRaisesRegex(ValueError, 'exactly one'):
            release.prepare_ffi(self.root)

    def test_cli_dispatches_every_stage_for_arm64(self):
        for command, function in [('export-uninstaller', 'installer_hook'), ('import-uninstaller', 'installer_hook'),
                                  ('stage-app', 'stage_app'), ('install-app', 'install_signed_app'), ('repack', 'repack_archives'),
                                  ('prepare-ffi', 'prepare_ffi'), ('repack-ffi', 'repack_ffi')]:
            with self.subTest(command=command), patch('sys.argv', ['windows-release.py', command, '--architecture', 'aarch64']), patch.object(release, function) as called:
                release.main()
                called.assert_called_once()


if __name__ == '__main__':
    unittest.main()
