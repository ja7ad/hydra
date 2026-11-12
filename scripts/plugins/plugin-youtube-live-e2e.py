#!/usr/bin/env python3
"""Test the YouTube plugin against YouTube itself: listing, resolution and media transfer.

Every stage runs the real Hydra CLI with the real Wasm plugin in an isolated
profile. A stage YouTube refuses is a failure, never a pass: a run only passes
when every download produced media that ffprobe accepts. Without
--cookies-from-browser nothing from a browser profile is read.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

PLUGIN = 'hydra.youtube'


def probe(path):
    """Stream kinds and the video height of a media file, or None when it is not media."""
    result = subprocess.run(['ffprobe', '-v', 'error', '-show_streams', '-of', 'json', str(path)], capture_output=True, text=True)
    if result.returncode:
        return None
    streams = json.loads(result.stdout).get('streams', [])
    return {
        'kinds': sorted({stream['codec_type'] for stream in streams}),
        'height': next((stream.get('height') for stream in streams if stream['codec_type'] == 'video'), None),
        'codecs': sorted({stream['codec_name'] for stream in streams}),
    }


def downloads(plan):
    """The download stages a resolved plan makes possible, as (name, arguments, extension, check)."""
    kinds = {kind: [track for track in plan['tracks'] if track['kind'] == kind] for kind in ('video', 'audio', 'subtitle')}
    heights = sorted({track['height'] for track in kinds['video'] if track.get('height')})
    stages = [('best', [], 'mkv', lambda media: media and 'video' in media['kinds'] and 'audio' in media['kinds'])]
    if len(heights) > 1:
        lowest = heights[0]
        stages.append((f'quality-{lowest}', ['--quality', str(lowest)], 'mkv', lambda media: media and media['height'] == lowest))
    if kinds['audio']:
        stages.append(('audio-mp3', ['--extract-audio', '--audio-format', 'mp3'], 'mp3', lambda media: media and media['kinds'] == ['audio'] and media['codecs'] == ['mp3']))
    return stages, kinds['subtitle']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--playlist', default='https://www.youtube.com/playlist?list=PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI')
    parser.add_argument('--video', action='append', help='video URL; repeat for several (default: two short public videos)')
    parser.add_argument('--cookies-from-browser', metavar='BROWSER', help='pass youtube.com cookies from this browser through Hydra')
    parser.add_argument('--yt-dlp-dir', type=Path, help='folder of the yt-dlp build to test instead of the one on PATH')
    parser.add_argument('--extractor-args', default='', help="value for the plugin's 'YouTube extractor arguments' setting")
    parser.add_argument('--runtime', default='default', choices=['default', 'deno', 'node', 'quickjs', 'bun'])
    parser.add_argument('--rounds', type=int, default=1, help='repeat every stage this many times')
    parser.add_argument('--skip-build', action='store_true')
    args = parser.parse_args()
    videos = args.video or ['https://www.youtube.com/watch?v=jNQXAC9IVRw', 'https://www.youtube.com/watch?v=aqz-KE-bpKQ']
    root = Path(__file__).resolve().parents[2]
    cli = Path(os.environ.get('HYDRA_E2E_CLI', root / 'target/debug/hydra')).resolve()
    if not args.skip_build:
        subprocess.run(['cargo', 'build', '-p', 'hya-cli'], cwd=root, check=True)
        subprocess.run(['cargo', 'build', '--manifest-path', 'plugins/hydra-youtube/Cargo.toml', '--release', '--target', 'wasm32-wasip1'], cwd=root, check=True)
    for tool in ['ffprobe', 'ffmpeg'] + ([] if args.yt_dlp_dir else ['yt-dlp']):
        if not shutil.which(tool):
            parser.error(f'{tool} must be installed')
    work = Path(tempfile.mkdtemp(prefix='hydra-youtube-live-'))
    env = {**os.environ, 'HYDRA_CONFIG_DIR': str(work / 'profile')}
    cookies = ['--cookies-from-browser', args.cookies_from_browser] if args.cookies_from_browser else []
    results = []

    def hydra(name, arguments, timeout=600):
        started = time.monotonic()
        try:
            result = subprocess.run([str(cli), *map(str, arguments)], cwd=work, env=env, capture_output=True, text=True, timeout=timeout)
            code, stdout, stderr = result.returncode, result.stdout, result.stderr
        except subprocess.TimeoutExpired as expired:
            code, stdout, stderr = 'timeout', expired.stdout or '', expired.stderr or ''
            stdout, stderr = (text.decode(errors='replace') if isinstance(text, bytes) else text for text in (stdout, stderr))
        (work / f'{name}.stdout').write_text(stdout)
        (work / f'{name}.stderr').write_text(stderr)
        return code, stdout, stderr, round(time.monotonic() - started, 1)

    def setup(name, arguments):
        code, _, stderr, _ = hydra(name, arguments)
        if code:
            raise SystemExit(f'{name} failed: {stderr.strip()[-600:]}')

    def record(stage, code, seconds, stderr, error=None, **facts):
        entry = {'stage': stage, 'exit_code': code, 'seconds': seconds, **facts}
        if code and not error:
            error = stderr.strip().splitlines()[-1][:400] if stderr.strip() else 'failed without output'
        if error:
            entry['error'] = error
        results.append(entry)
        print(json.dumps(entry), flush=True)
        return not error

    package = work / 'package'
    package.mkdir()
    shutil.copy2(root / 'plugins/hydra-youtube/hydra-plugin.toml', package)
    shutil.copy2(root / 'plugins/hydra-youtube/target/wasm32-wasip1/release/hydra_youtube.wasm', package / 'plugin.wasm')
    setup('pack', ['plugin', 'pack', package, work / 'youtube.hyaplugin'])
    setup('install', ['plugin', 'install', work / 'youtube.hyaplugin', '--accept-permissions'])
    setup('set-cookies', ['plugin', 'set', PLUGIN, 'use_cookies', 'true' if cookies else 'false'])
    setup('set-runtime', ['plugin', 'set', PLUGIN, 'javascript_runtime', args.runtime])
    setup('set-extractor-args', ['plugin', 'set', PLUGIN, 'extractor_args', args.extractor_args])
    if args.yt_dlp_dir:
        setup('set-backend', ['plugin', 'set', PLUGIN, 'yt_dlp_path', args.yt_dlp_dir.resolve()])
        setup('grant-backend', ['plugin', 'grant', PLUGIN, 'exec:yt-dlp'])

    code, _, stderr, seconds = hydra('check', ['plugin', 'check', PLUGIN])
    record('check', code, seconds, stderr, outdated_backend='is older than' in stderr)
    for round_number in range(1, args.rounds + 1):
        tag = f'r{round_number}'
        code, stdout, stderr, seconds = hydra(f'{tag}-playlist', [*cookies, '--list-tracks', args.playlist])
        entries = len(json.loads(stdout).get('entries', [])) if code == 0 else 0
        record(f'{tag}/playlist', code, seconds, stderr, None if code or entries else 'playlist returned no entries', entries=entries)
        for index, video in enumerate(videos, 1):
            label = f'{tag}/video{index}'
            code, stdout, stderr, seconds = hydra(f'{tag}-v{index}-tracks', [*cookies, '--list-tracks', video])
            plan = json.loads(stdout) if code == 0 else {'tracks': []}
            stages, subtitles = downloads(plan)
            heights = sorted({track['height'] for track in plan['tracks'] if track['kind'] == 'video' and track.get('height')})
            listed = record(f'{label}/tracks', code, seconds, stderr, heights=heights, audio=sum(t['kind'] == 'audio' for t in plan['tracks']), subtitles=len(subtitles))
            if not listed:
                continue
            for name, flags, extension, accepts in stages:
                output = work / f'{tag}-v{index}-{name}.{extension}'
                code, _, stderr, seconds = hydra(f'{tag}-v{index}-{name}', [*cookies, '--no-input', *flags, '-O', output, video])
                media = probe(output) if code == 0 and output.is_file() else None
                error = None if code or accepts(media) else f'downloaded file is not the expected media: {media}'
                record(f'{label}/{name}', code, seconds, stderr, error, bytes=output.stat().st_size if output.is_file() else 0, media=media)
            if subtitles:
                language = subtitles[0]['language']
                output = work / f'{tag}-v{index}-subs.mkv'
                code, _, stderr, seconds = hydra(f'{tag}-v{index}-subs', [*cookies, '--no-input', '--subs', language, '-O', output, video])
                sidecars = [path for path in work.glob(f'{output.name}.track-*') if path.stat().st_size]
                error = None if code or sidecars else f'no nonempty {language} subtitle sidecar was written'
                record(f'{label}/subtitles-{language}', code, seconds, stderr, error, sidecars=[path.name for path in sidecars])
    hydra('plugin-logs', ['plugin', 'logs', PLUGIN])
    failed = [entry['stage'] for entry in results if 'error' in entry]
    report = {
        'passed': not failed,
        'failed': failed,
        'cookies': args.cookies_from_browser,
        'rounds': args.rounds,
        'playlist': args.playlist,
        'videos': videos,
        'results': results,
        'artifacts': str(work),
    }
    (work / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(f'{"PASS" if not failed else "FAIL"}: {len(results) - len(failed)}/{len(results)} live stages passed; report: {work / "report.json"}', flush=True)
    return 0 if not failed else 1


if __name__ == '__main__':
    raise SystemExit(main())
