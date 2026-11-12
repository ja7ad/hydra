#!/usr/bin/env python3
"""Move the YouTube plugin to the newest yt-dlp release, but only if it works.

Looks up the latest yt-dlp release, downloads this platform's official build
into a temporary folder and verifies it against the release's SHA2-256SUMS.
The new build then has to (1) accept every command line the plugin's manifest
declares and (2) pass the live plugin E2E. Only then are the pinned release in
the plugin source and README, and the plugin's patch version in
hydra-plugin.toml, Cargo.toml and Cargo.lock, rewritten. Nothing is committed.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
PLUGIN = ROOT / 'plugins/hydra-youtube'
RELEASES = 'https://github.com/yt-dlp/yt-dlp/releases'
LATEST_API = 'https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest'
PINNED = re.compile(r'(pub const RELEASE: &str = ")([0-9.]+)(";)')
ASSETS = {
    ('Darwin', 'arm64'): 'yt-dlp_macos',
    ('Darwin', 'x86_64'): 'yt-dlp_macos',
    ('Linux', 'x86_64'): 'yt-dlp_linux',
    ('Linux', 'aarch64'): 'yt-dlp_linux_aarch64',
    ('Windows', 'AMD64'): 'yt-dlp.exe',
    ('Windows', 'ARM64'): 'yt-dlp_arm64.exe',
}
# What each `*` in a manifest template stands for, keyed by the argument before it.
SAMPLE_VALUES = {'--js-runtimes': 'node', '--extractor-args': 'youtube:lang=en'}


def pinned_release(source):
    return PINNED.search(source).group(2)


def release_number(tag):
    return [int(part) for part in tag.split('.')]


def checksum_for(sums, asset):
    for line in sums.splitlines():
        digest, _, name = line.partition(' ')
        if name.strip().lstrip('*') == asset:
            return digest
    return None


def bump_patch(version):
    major, minor, patch = version.split('.')
    return f'{major}.{minor}.{int(patch) + 1}'


def command_line(template, url, cookie_file):
    """A manifest exec template with every placeholder filled in."""
    line = []
    for argument in template:
        if argument == '{cookie_file}':
            line.append(str(cookie_file))
        elif argument == '*':
            line.append(SAMPLE_VALUES.get(line[-1], url))
        else:
            line.append(argument)
    return line


def flags_rejected(returncode, stderr):
    """Whether yt-dlp refused the command line itself, as opposed to failing to extract."""
    return returncode == 2 or 'no such option' in stderr or 'Usage: ' in stderr


def rewritten(old_release, new_release, old_version, new_version):
    """The plugin files a release bump touches, as {path: new text}."""
    def replace(path, old, new, count=1):
        text = (PLUGIN / path).read_text()
        if text.count(old) < count:
            raise SystemExit(f'{path}: expected `{old}`')
        return text.replace(old, new)

    return {
        'src/backend.rs': replace('src/backend.rs', f'RELEASE: &str = "{old_release}"', f'RELEASE: &str = "{new_release}"'),
        'README.md': replace('README.md', f'official {old_release} release', f'official {new_release} release'),
        'hydra-plugin.toml': replace('hydra-plugin.toml', f'version = "{old_version}"', f'version = "{new_version}"'),
        'Cargo.toml': replace('Cargo.toml', f'name = "hydra-youtube"\nversion = "{old_version}"', f'name = "hydra-youtube"\nversion = "{new_version}"'),
        'Cargo.lock': replace('Cargo.lock', f'name = "hydra-youtube"\nversion = "{old_version}"', f'name = "hydra-youtube"\nversion = "{new_version}"'),
    }


def fetch(url):
    request = urllib.request.Request(url, headers={'User-Agent': 'hydra-plugin-maintenance'})
    with urllib.request.urlopen(request, timeout=120) as response:
        return response.read()


def download_release(tag, folder):
    asset = ASSETS.get((platform.system(), platform.machine()))
    if not asset:
        raise SystemExit(f'no official yt-dlp build for {platform.system()} {platform.machine()}')
    wanted = checksum_for(fetch(f'{RELEASES}/download/{tag}/SHA2-256SUMS').decode(), asset)
    if not wanted:
        raise SystemExit(f'yt-dlp {tag} publishes no checksum for {asset}')
    data = fetch(f'{RELEASES}/download/{tag}/{asset}')
    if hashlib.sha256(data).hexdigest() != wanted:
        raise SystemExit(f'{asset} does not match the published SHA256; nothing was changed')
    program = folder / ('yt-dlp.exe' if asset.endswith('.exe') else 'yt-dlp')
    program.write_bytes(data)
    program.chmod(0o755)
    return program


def check_flags(program, templates, video, playlist, work):
    """Runs every declared command line; returns the ones yt-dlp's option parser refused."""
    cookie_file = work / 'cookies.txt'
    cookie_file.write_text('# Netscape HTTP Cookie File\n')

    def attempt(numbered):
        index, template = numbered
        jar = work / f'cookies-{index}.txt'
        jar.write_text(cookie_file.read_text())
        line = command_line(template, playlist if '--flat-playlist' in template else video, jar)
        result = subprocess.run([str(program), *line], capture_output=True, text=True, timeout=300)
        rejected = flags_rejected(result.returncode, result.stderr)
        verdict = 'REJECTED' if rejected else 'accepted' if result.returncode == 0 else 'accepted (extraction refused by YouTube)'
        print(f'  {verdict}: {" ".join(template)}', flush=True)
        return template if rejected else None

    with ThreadPoolExecutor(max_workers=4) as pool:
        return [template for template in pool.map(attempt, enumerate(templates)) if template]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--release', help='test this yt-dlp release instead of the latest one')
    parser.add_argument('--force', action='store_true', help='run the checks even when the plugin already pins that release')
    parser.add_argument('--dry-run', action='store_true', help='run every check but leave the plugin files alone')
    parser.add_argument('--cookies-from-browser', metavar='BROWSER', help='passed to the live E2E for networks YouTube refuses anonymously')
    parser.add_argument('--rounds', type=int, default=2, help='live E2E rounds (default 2)')
    parser.add_argument('--video', default='https://www.youtube.com/watch?v=jNQXAC9IVRw')
    parser.add_argument('--playlist', default='https://www.youtube.com/playlist?list=PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI')
    args = parser.parse_args()

    current = pinned_release((PLUGIN / 'src/backend.rs').read_text())
    latest = args.release or json.loads(fetch(LATEST_API))['tag_name']
    print(f'pinned yt-dlp {current}; candidate {latest}', flush=True)
    if release_number(latest) <= release_number(current) and not args.force:
        print('The plugin already pins the newest release; nothing to do.')
        return 0

    work = Path(tempfile.mkdtemp(prefix='hydra-yt-dlp-update-'))
    backend = work / 'backend'
    backend.mkdir()
    program = download_release(latest, backend)
    reported = subprocess.run([str(program), '--ignore-config', '--no-plugin-dirs', '--version'], capture_output=True, text=True, timeout=120).stdout.strip()
    if reported != latest:
        raise SystemExit(f'the downloaded build reports version `{reported}`, not {latest}')
    print(f'downloaded and verified {program}', flush=True)

    manifest = tomllib.loads((PLUGIN / 'hydra-plugin.toml').read_text())
    templates = [entry['args'] for entry in manifest['permissions']['exec'] if '--version' not in entry['args']]
    print(f'checking {len(templates)} declared command lines against yt-dlp {latest}', flush=True)
    rejected = check_flags(program, templates, args.video, args.playlist, work)
    if rejected:
        print(f'FAIL: yt-dlp {latest} refuses {len(rejected)} command lines the plugin uses; the plugin needs a code change first.')
        return 1

    live = [sys.executable, str(ROOT / 'scripts/plugins/plugin-youtube-live-e2e.py'), '--yt-dlp-dir', str(backend), '--rounds', str(args.rounds)]
    if args.cookies_from_browser:
        live += ['--cookies-from-browser', args.cookies_from_browser]
    print('running the live plugin E2E with the new build', flush=True)
    if subprocess.run(live, cwd=ROOT).returncode:
        print(f'FAIL: the live E2E did not pass with yt-dlp {latest}; the plugin files were left alone.')
        return 1

    old_version = manifest['version']
    new_version = bump_patch(old_version)
    if latest == current:
        print(f'PASS: yt-dlp {latest} is already pinned; no files changed.')
        return 0
    files = rewritten(current, latest, old_version, new_version)
    if args.dry_run:
        print(f'PASS (dry run): would pin yt-dlp {latest} and release plugin {new_version}: {", ".join(files)}')
        return 0
    for path, text in files.items():
        (PLUGIN / path).write_text(text)
    print(f'PASS: pinned yt-dlp {latest}; plugin {old_version} -> {new_version}. Review and commit: {", ".join(files)}')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
