#!/usr/bin/env python3
"""Prepare and repack the v1.1.0 Windows release without editing signed packages."""
import argparse
import pathlib
import shutil
import stat
import subprocess
import tomllib
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


def prepare_plugin(root, architecture):
    platform = f'windows-{architecture}'
    package = root / 'original-unsigned' / f'unsigned-torrent-download-{platform}.hyaplugin'
    destination = root / 'plugin-staging'
    extract(package, destination)
    manifest = tomllib.loads((destination / 'hydra-plugin.toml').read_text())
    modules = manifest['native_modules']
    if len(modules) != 1 or modules[0]['platform'] != platform:
        raise ValueError('torrent package does not match the requested Windows target')
    module = modules[0]['module']
    if pathlib.PurePosixPath(module).name != module or not module.endswith('.dll'):
        raise ValueError('unexpected torrent module path')
    original = root / 'source/plugins/bundled/native' / platform / f'torrent-download-{platform}.hyaplugin'
    with zipfile.ZipFile(original) as archive:
        if archive.read(module) != (destination / module).read_bytes():
            raise ValueError('unsigned torrent DLL differs from the released plugin')
    unsigned = root / 'unsigned-native'
    unsigned.mkdir()
    shutil.copy(destination / module, unsigned / module)
    shutil.copy(root / 'source/target/debug/hydra-plugin.exe', unsigned / 'hydra-plugin.exe')


def repack_plugin(root, architecture):
    platform = f'windows-{architecture}'
    staging = root / 'plugin-staging'
    manifest = tomllib.loads((staging / 'hydra-plugin.toml').read_text())
    module = manifest['native_modules'][0]['module']
    shutil.copy(root / 'signed-native' / module, staging / module)
    tool = root / 'source/target/debug/hydra-plugin.exe'
    unsigned = root / 'unsigned-torrent.hyaplugin'
    output = root / 'source/plugins/bundled/native' / platform / f'torrent-download-{platform}.hyaplugin'
    subprocess.run([str(tool), 'pack', str(staging), '--output', str(unsigned)], check=True)
    subprocess.run([str(tool), 'sign', str(unsigned), '--output', str(output),
                    '--public-key', str(root / 'source/plugins/official.pub'),
                    '--key-env', 'HYDRA_PLUGIN_SIGNING_KEY'], check=True)
    subprocess.run([str(tool), 'validate', str(output)], check=True)


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
    remote = output / 'remote-installers'
    remote.mkdir()
    for name in ('install.ps1', 'uninstall.ps1'):
        shutil.copy(root / name, remote / name)


def install_signed_app(root, target):
    for binary in BINARIES:
        shutil.copy(root / 'signed-app' / binary, root / 'source/target' / target / 'release' / binary)
    shutil.copy(root / 'signed-app/HydraPortable.exe', root / 'source/scripts/windows/portable/HydraPortable.exe')
    for name, destination in [('install-native-host.ps1', 'scripts/install-native-host.ps1'),
                              ('uninstall.ps1', 'uninstall.ps1')]:
        shutil.copy(root / 'signed-app' / name, root / 'source' / destination)


def repack_archives(root, architecture, target):
    arch = {'x86_64': 'amd64', 'aarch64': 'arm64'}[architecture]
    source = root / 'source'
    output = root / 'dist'
    output.mkdir(exist_ok=True)
    for prefix in ('hydra', 'hydra-cli', 'hydra-plugin-cli'):
        name = f'{prefix}-1.1.0-windows-{arch}.zip'
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
    portable = source / 'target/dist' / f'hydra-1.1.0-windows-portable-{arch}.zip'
    shutil.copy(portable, output / portable.name)
    platform = f'windows-{architecture}'
    plugin = source / 'plugins/bundled/native' / platform / f'torrent-download-{platform}.hyaplugin'
    shutil.copy(plugin, output / plugin.name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['prepare-plugin', 'repack-plugin', 'export-uninstaller',
                                          'import-uninstaller', 'stage-app', 'install-app', 'repack'])
    parser.add_argument('--architecture', choices=['x86_64', 'aarch64'], required=True)
    args = parser.parse_args()
    root = pathlib.Path.cwd()
    target = {'x86_64': 'x86_64-pc-windows-msvc', 'aarch64': 'aarch64-pc-windows-msvc'}[args.architecture]
    commands = {
        'prepare-plugin': lambda: prepare_plugin(root, args.architecture),
        'repack-plugin': lambda: repack_plugin(root, args.architecture),
        'export-uninstaller': lambda: installer_hook(root, 'export'),
        'import-uninstaller': lambda: installer_hook(root, 'import'),
        'stage-app': lambda: stage_app(root, target),
        'install-app': lambda: install_signed_app(root, target),
        'repack': lambda: repack_archives(root, args.architecture, target),
    }
    commands[args.command]()


if __name__ == '__main__':
    main()
