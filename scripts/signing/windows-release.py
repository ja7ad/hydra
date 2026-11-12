#!/usr/bin/env python3
"""Prepare and repack Windows releases without editing signed packages."""
import argparse
import pathlib
import shutil
import stat
import zipfile


BINARIES = ('hydra.exe', 'hydra-gui.exe', 'hydra-host.exe', 'hydra-updater.exe', 'hydra-plugin.exe')


def extract(archive, destination):
    destination = pathlib.Path(destination)
    with zipfile.ZipFile(archive) as source:
        seen = set()
        total = 0
        for entry in source.infolist():
            name = entry.orig_filename
            parts = pathlib.PurePosixPath(name).parts
            mode = entry.external_attr >> 16
            normalized = name.rstrip('/')
            if (not parts or name != entry.filename or '\\' in name or ':' in name
                    or name.startswith('/') or '..' in parts or '.' in name.split('/')
                    or normalized != pathlib.PurePosixPath(name).as_posix()
                    or normalized.casefold() in seen or stat.S_ISLNK(mode)):
                raise ValueError(f'unsafe archive entry: {name}')
            seen.add(normalized.casefold())
            total += entry.file_size
            if total > 1024 * 1024 * 1024 or len(seen) > 20000:
                raise ValueError('archive exceeds extraction limit')
        source.extractall(destination)


def zip_directory(directory, output):
    directory = pathlib.Path(directory)
    with zipfile.ZipFile(output, 'w', zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(directory.rglob('*')):
            if path.is_file():
                archive.write(path, path.relative_to(directory).as_posix())


def installer_hook(root, mode):
    path = root / 'source/scripts/windows/hydra-installer.nsi'
    if mode == 'export':
        content = path.read_text()
        if content.count('  WriteUninstaller "$INSTDIR\\uninstall.exe"') != 1:
            raise ValueError('installer uninstaller declaration changed')
        (root / 'original-installer.nsi').write_text(content)
        output = (root / 'unsigned-app/uninstall.exe').resolve()
        path.write_text(content + f'\n!uninstfinalize \'cmd /c copy /Y "%1" "{output}"\' = 0\n')
    else:
        content = (root / 'original-installer.nsi').read_text()
        signed = (root / 'signed-app/uninstall.exe').resolve()
        path.write_text(content.replace('  WriteUninstaller "$INSTDIR\\uninstall.exe"',
                                       f'  File /oname=uninstall.exe "{signed}"'))


def stage_app(root, target):
    output = root / 'unsigned-app'
    output.mkdir(exist_ok=True)
    for binary in BINARIES:
        shutil.copy(root / 'source/target' / target / 'release' / binary, output / binary)
    shutil.copy(root / 'source/scripts/windows/portable/HydraPortable.exe', output / 'HydraPortable.exe')
    shutil.copy(root / 'source/scripts/install-native-host.ps1', output / 'install-native-host.ps1')
    shutil.copy(root / 'source/uninstall.ps1', output / 'uninstall.ps1')
    shutil.copy(root / 'source/scripts/windows/set-user-path.ps1', output / 'set-user-path.ps1')
    remote = output / 'remote-installers'
    remote.mkdir()
    for name in ('install.ps1', 'uninstall.ps1'):
        shutil.copy(root / name, remote / name)


def install_signed_app(root, target):
    for binary in BINARIES:
        shutil.copy(root / 'signed-app' / binary, root / 'source/target' / target / 'release' / binary)
    shutil.copy(root / 'signed-app/HydraPortable.exe', root / 'source/scripts/windows/portable/HydraPortable.exe')
    for name, destination in [('install-native-host.ps1', 'scripts/install-native-host.ps1'),
                              ('uninstall.ps1', 'uninstall.ps1'),
                              ('set-user-path.ps1', 'scripts/windows/set-user-path.ps1')]:
        shutil.copy(root / 'signed-app' / name, root / 'source' / destination)


def repack_archives(root, architecture, target, version="1.1.0"):
    arch = {'x86_64': 'amd64', 'aarch64': 'arm64'}[architecture]
    source = root / 'source'
    output = root / 'dist'
    output.mkdir(exist_ok=True)
    for prefix in ('hydra', 'hydra-cli', 'hydra-plugin-cli'):
        name = f'{prefix}-{version}-windows-{arch}.zip'
        stage = root / 'archives' / prefix
        extract(root / 'original-windows' / name, stage)
        replacements = []
        for binary in stage.rglob('*.exe'):
            if binary.name not in BINARIES:
                raise ValueError(f'unexpected executable: {binary}')
            shutil.copy(source / 'target' / target / 'release' / binary.name, binary)
            replacements.append(binary.name)
        expected = {'hydra': BINARIES[:4], 'hydra-cli': ('hydra.exe',), 'hydra-plugin-cli': ('hydra-plugin.exe',)}[prefix]
        if sorted(replacements) != sorted(expected):
            raise ValueError(f'unexpected executable count or names in {name}')
        for script in stage.rglob('*.ps1'):
            signed = root / 'signed-app' / script.name
            if not signed.is_file():
                raise ValueError(f'unexpected PowerShell script: {script}')
            shutil.copy(signed, script)
        zip_directory(stage, output / name)
    portable = source / 'target/dist' / f'hydra-{version}-windows-portable-{arch}.zip'
    shutil.copy(portable, output / portable.name)


def prepare_ffi(root):
    archives = list((root / 'original-ffi').glob('*.zip'))
    if len(archives) != 1:
        raise ValueError('expected exactly one Windows FFI archive')
    staging = root / 'ffi-staging'
    extract(archives[0], staging)
    libraries = sorted(staging.rglob('*.dll'))
    if not libraries:
        raise ValueError('Windows FFI archive contains no DLL')
    unsigned = root / 'unsigned-native'
    unsigned.mkdir(exist_ok=True)
    for index, library in enumerate(libraries):
        shutil.copy(library, unsigned / f'ffi-{index}-{library.name}')
    # The SignPath artifact configuration rejects a request without an EXE.
    shutil.copy(root / 'source/target/debug/hydra-plugin.exe', unsigned / 'hydra-plugin.exe')


def repack_ffi(root):
    staging = root / 'ffi-staging'
    for index, library in enumerate(sorted(staging.rglob('*.dll'))):
        shutil.copy(root / 'signed-native' / f'ffi-{index}-{library.name}', library)
    output = root / 'ffi-dist'
    output.mkdir(exist_ok=True)
    archive, = (root / 'original-ffi').glob('*.zip')
    zip_directory(staging, output / archive.name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['export-uninstaller', 'import-uninstaller', 'stage-app',
                                          'install-app', 'repack', 'prepare-ffi', 'repack-ffi'])
    parser.add_argument('--architecture', choices=['x86_64', 'aarch64'], required=True)
    parser.add_argument("--version", default="1.1.0")
    args = parser.parse_args()
    root = pathlib.Path.cwd()
    target = {'x86_64': 'x86_64-pc-windows-msvc', 'aarch64': 'aarch64-pc-windows-msvc'}[args.architecture]
    commands = {
        'prepare-ffi': lambda: prepare_ffi(root),
        'repack-ffi': lambda: repack_ffi(root),
        'export-uninstaller': lambda: installer_hook(root, 'export'),
        'import-uninstaller': lambda: installer_hook(root, 'import'),
        'stage-app': lambda: stage_app(root, target),
        'install-app': lambda: install_signed_app(root, target),
        'repack': lambda: repack_archives(root, args.architecture, target, args.version),
    }
    commands[args.command]()


if __name__ == '__main__':
    main()
