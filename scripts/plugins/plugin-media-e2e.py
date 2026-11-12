#!/usr/bin/env python3
"""Exercise the official Wasm YouTube plugin with a local extractor and media origin."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile
import threading
import time

parser = argparse.ArgumentParser()
parser.add_argument('--serve-gui', action='store_true')
args = parser.parse_args()
root = Path(__file__).resolve().parents[2]
if not os.environ.get('HYDRA_E2E_SKIP_BUILD'):
    subprocess.run(['cargo', 'build', '-p', 'hya-cli', '-p', 'hya-gui'], cwd=root, check=True)
    subprocess.run(['cargo', 'build', '--manifest-path', 'plugins/hydra-youtube/Cargo.toml', '--release', '--target', 'wasm32-wasip1'], cwd=root, check=True)
if os.name == 'nt':
    raise SystemExit('The extractor fixture uses a POSIX executable; real Windows backend tests run separately.')
work = Path(tempfile.mkdtemp(prefix='hydra-media-e2e-'))
media = work / 'media'
media.mkdir()
for name, size in [('low', '64x64'), ('high', '128x128')]:
    subprocess.run(['ffmpeg', '-nostdin', '-v', 'error', '-f', 'lavfi', '-i', f'color=c=blue:s={size}:r=12', '-t', '0.5', '-an', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', str(media / f'{name}.mp4')], check=True)
subprocess.run(['ffmpeg', '-nostdin', '-v', 'error', '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=44100', '-t', '0.5', '-c:a', 'aac', str(media / 'audio.m4a')], check=True)
(media / 'captions.vtt').write_text('WEBVTT\n\n00:00.000 --> 00:00.400\nHydra plugin test\n')
class Origin(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(media), **kwargs)
    def log_message(self, *_):
        pass
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Origin)
threading.Thread(target=server.serve_forever, daemon=True).start()
base = f'http://127.0.0.1:{server.server_port}'
backend = work / 'backend'
backend.mkdir()
program = backend / 'yt-dlp'
program.write_text(f'#!{sys.executable}\n' + '''import json,sys
from urllib.parse import urlparse, parse_qs
if '--version' in sys.argv:
    print('fixture-1.0')
    raise SystemExit(0)
url=sys.argv[-1]
if '--flat-playlist' in sys.argv:
    entries=[{'id':'one','title':'First video'},{'id':'gone','title':'[Deleted video]'},{'id':'two','title':'Second video'}]
    if 'PLbroken' in url:
        entries.insert(1,{'id':'broken','title':'Unplayable video'})
    print(json.dumps({'id':'PLfixture','title':'Fixture playlist','entries':entries}))
else:
    video=parse_qs(urlparse(url).query).get('v',['one'])[0]
    if video=='broken':
        sys.stderr.write('ERROR: [youtube] broken: Video unavailable. This video has been removed\\n')
        raise SystemExit(1)
    print(json.dumps({'id':video,'title':'Video '+video,'formats':[
        {'format_id':'low','url':BASE+'/low.mp4','vcodec':'avc1','acodec':'none','height':480,'ext':'mp4','protocol':'http'},
        {'format_id':'high','url':BASE+'/high.mp4','vcodec':'avc1','acodec':'none','height':720,'ext':'mp4','protocol':'http'},
        {'format_id':'audio','url':BASE+'/audio.m4a','vcodec':'none','acodec':'mp4a','abr':128,'ext':'m4a','protocol':'http'}
    ],'subtitles':{'en':[{'url':BASE+'/captions.vtt','ext':'vtt'}]}}))
'''.replace('BASE', repr(base)))
program.chmod(0o755)
package = work / 'package'
package.mkdir()
manifest = (root / 'plugins/hydra-youtube/hydra-plugin.toml').read_text()
manifest = manifest.replace('sources = [', 'sources = ["127.0.0.1", ')
manifest = manifest.replace('key = "yt_dlp_path"', 'default = '+json.dumps(str(backend))+'\nkey = "yt_dlp_path"')
(package / 'hydra-plugin.toml').write_text(manifest)
shutil.copy2(root / 'plugins/hydra-youtube/target/wasm32-wasip1/release/hydra_youtube.wasm', package / 'plugin.wasm')
cli = Path(os.environ.get('HYDRA_E2E_CLI', root / 'target/debug/hydra'))
env = {**os.environ, 'PATH': str(backend) + os.pathsep + os.environ.get('PATH', ''), 'HYDRA_CONFIG_DIR': str(work / 'profile')}
def run(*arguments, success=True):
    result = subprocess.run([str(cli), *map(str, arguments)], cwd=work, env=env, text=True, capture_output=True, timeout=90)
    assert result.returncode == 0 if success else result.returncode != 0, (arguments, result.stdout, result.stderr)
    return result
try:
    archive = work / 'youtube-fixture.hyaplugin'
    run('plugin', 'pack', package, archive)
    installed = run('plugin', 'install', archive, '--accept-permissions')
    assert 'Getting started' in installed.stderr
    info = json.loads(run('plugin', 'info', 'hydra.youtube', '--json').stdout)
    assert str(work / 'profile/plugins/hydra.youtube') in info['directory']
    url = 'https://www.youtube.com/watch?v=one'
    plan = json.loads(run('--list-tracks', url).stdout)
    assert len(plan['tracks']) == 4
    original = work / 'original.mp4'
    run('--no-input', '--track', 'low', '--audio', 'none', '-O', original, url)
    assert hashlib.sha256(original.read_bytes()).digest() == hashlib.sha256((media / 'low.mp4').read_bytes()).digest()
    extracted = work / 'audio.mp3'
    run('--no-input', '--extract-audio', '--audio-format', 'mp3', '-O', extracted, url)
    probe = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-show_streams', '-of', 'json', str(extracted)], text=True))
    assert len(probe['streams']) == 1 and probe['streams'][0]['codec_name'] == 'mp3'
    playlist = 'https://www.youtube.com/playlist?list=PLfixture'
    listing = json.loads(run('--list-tracks', playlist).stdout)
    assert [entry['id'] for entry in listing['entries']] == ['one', 'two']
    run('--no-input', '--extract-audio', '--audio-format', 'mp3', '--output-dir', work / 'playlist-output', playlist)
    assert len(list((work / 'playlist-output').rglob('*.mp3'))) == 2
    run('--no-input', '-O', work / 'invalid.bin', playlist, success=False)
    partial = run('--no-input', '--extract-audio', '--audio-format', 'mp3', '--output-dir', work / 'partial-output', 'https://www.youtube.com/playlist?list=PLbroken', success=False)
    assert '1 of 3 playlist items failed' in partial.stderr, partial.stderr
    assert 'reports this video as unavailable' in partial.stderr, partial.stderr
    assert len(list((work / 'partial-output').rglob('*.mp3'))) == 2
    assert 'Resolved' in run('plugin', 'logs', 'hydra.youtube').stdout
    import pty
    pid, terminal = pty.fork()
    if pid == 0:
        os.chdir(work)
        os.execve(cli, [str(cli), '-O', str(work / 'interactive.mp4'), url], env)
    transcript = bytearray()
    def wait_for(text, timeout=45):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if text.encode() in transcript:
                return
            if select.select([terminal], [], [], .1)[0]:
                transcript.extend(os.read(terminal, 65536))
        raise AssertionError((text, transcript[-3000:]))
    try:
        wait_for('Download mode:')
        os.write(terminal, b'1\n')
        wait_for('Choose video quality:')
        os.write(terminal, b'99\n3\n')
        wait_for('Choose audio:')
        os.write(terminal, b'1\n')
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            finished, status = os.waitpid(pid, os.WNOHANG)
            if finished:
                assert os.waitstatus_to_exitcode(status) == 0, transcript[-3000:]
                break
            if select.select([terminal], [], [], .1)[0]:
                try:
                    transcript.extend(os.read(terminal, 65536))
                except OSError:
                    pass
        else:
            raise AssertionError('interactive download did not finish')
        assert b'Enter a number from 1 to 3' in transcript
        probe = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-show_streams', '-of', 'json', str(work / 'interactive.mp4')], text=True))
        assert next(stream for stream in probe['streams'] if stream['codec_type'] == 'video')['width'] == 64
        assert any(stream['codec_type'] == 'audio' for stream in probe['streams'])
    finally:
        os.close(terminal)
    print('PASS: official Wasm plugin, shared storage, welcome, logs, original video, MP3 extraction, playlists that survive a failed item, and interactive quality selection', flush=True)
    if args.serve_gui:
        gui = work / 'gui-profile'
        gui.mkdir()
        (gui / 'config.toml').write_text('[settings]\nlaunch_on_startup=false\ncheck_updates_on_startup=false\nstart_in_tray=false\nclose_to_tray=false\nmonitor_clipboard=false\ngpu_render=false\ntheme_mode="Light"\n')
        with (gui / 'config.toml').open('a') as config:
            config.write('\n[[categories]]\nname="General"\nexts=[]\nbuiltin=true\ndir=' + json.dumps(str(work / 'gui-output')) + '\n')
        print(json.dumps({'work':str(work), 'package':str(archive), 'profile':str(gui), 'video':url, 'playlist':playlist}), flush=True)
        threading.Event().wait()
finally:
    server.shutdown()
    if not args.serve_gui:
        shutil.rmtree(work)
