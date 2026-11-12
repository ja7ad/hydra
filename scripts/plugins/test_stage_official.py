"""Offline scenarios for official release bundle staging."""
import hashlib
import importlib.util
import io
import json
import pathlib
import tempfile
import unittest
import warnings
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location("stage_official", pathlib.Path(__file__).with_name("stage-official.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def bundle(entries):
    stream = io.BytesIO()
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", UserWarning)
        with zipfile.ZipFile(stream, "w", zipfile.ZIP_DEFLATED) as archive:
            for name, data in entries:
                entry = zipfile.ZipInfo()
                entry.filename = name
                entry.orig_filename = name
                entry.compress_type = zipfile.ZIP_DEFLATED
                archive.writestr(entry, data)
    return stream.getvalue()


def package(identity, version):
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w") as archive:
        archive.writestr("hydra-plugin.toml", f'id = "{identity}"\nversion = "{version}"\n')
    return stream.getvalue()


class CollectFromCatalogTests(unittest.TestCase):
    KEY = "RWSfixturepublisherkey"
    LATEST = "https://github.com/ja7ad/hydra/releases/latest/download/"

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        (self.root / "docs").mkdir()
        (self.root / "plugins").mkdir()
        (self.root / "plugins/official.pub").write_text(f"untrusted comment: fixture\n{self.KEY}\n")
        self.released = {"youtube-download_0.2.0.hyaplugin": package("hydra.youtube", "0.2.0")}
        torrent = []
        for platform in ["linux-x86_64", "windows-aarch64"]:
            name = f"torrent-download_0.1.0_{platform.replace('-', '_')}.hyaplugin"
            self.released[name] = package("hydra.torrent", "0.1.0") + platform.encode()
            torrent.append({"caption": platform, "link": self.LATEST + name, "platform": platform,
                            "sha256": hashlib.sha256(self.released[name]).hexdigest()})
        self.catalog = {"schema": 1, "name": "Fixture", "plugins": [
            {"id": "hydra.youtube", "version": "0.2.0", "is_official": True, "publisher_key": self.KEY,
             "sha256": hashlib.sha256(self.released["youtube-download_0.2.0.hyaplugin"]).hexdigest(),
             "download": [{"caption": "All platforms", "link": self.LATEST + "youtube-download_0.2.0.hyaplugin"}]},
            {"id": "hydra.torrent", "version": "0.1.0", "is_official": True, "publisher_key": self.KEY,
             "download": torrent},
            {"id": "community.sample", "version": "1.0.0", "is_official": False,
             "download": [{"caption": "All platforms", "link": "https://example.com/sample.hyaplugin"}]}]}

    def collect(self):
        (self.root / "docs/plugins.json").write_text(json.dumps(self.catalog))
        fetch = lambda link, timeout: io.BytesIO(self.released[link.removeprefix(self.LATEST)])
        with patch.object(module.urllib.request, "urlopen", side_effect=fetch) as opened:
            output = module.collect(self.root, "1.3.0", self.root / "official")
        self.assertEqual(opened.call_count, 3)
        return output

    def test_released_packages_become_release_assets_and_an_installable_bundle(self):
        output = self.collect()
        assets = {path.name for path in (output / "assets").iterdir()}
        self.assertEqual(assets, set(self.released) | {"hydra-official-plugins.zip", "hydra-official-plugins.pub"})
        bundle = output / "bundle"
        self.assertEqual({path.relative_to(bundle).as_posix() for path in bundle.rglob("*") if path.is_file()},
                         {"youtube-download_0.2.0.hyaplugin", "bundle-version.txt",
                          "native/linux-x86_64/torrent-download_0.1.0_linux_x86_64.hyaplugin",
                          "native/windows-aarch64/torrent-download_0.1.0_windows_aarch64.hyaplugin"})
        self.assertEqual((bundle / "bundle-version.txt").read_text().strip(), "1.3.0")
        for name, data in self.released.items():
            self.assertEqual((output / "assets" / name).read_bytes(), data)

    def test_the_collected_archive_is_the_bundle_local_packaging_stages(self):
        archive = (self.collect() / "assets/hydra-official-plugins.zip").read_bytes()
        with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(archive)):
            staged = module.stage(self.root, "1.3.0")
        native = staged / "native/linux-x86_64/torrent-download_0.1.0_linux_x86_64.hyaplugin"
        self.assertEqual(native.read_bytes(), self.released[native.name])
        self.assertTrue((staged / "youtube-download_0.2.0.hyaplugin").is_file())

    def test_a_package_that_differs_from_the_catalog_blocks_the_release(self):
        scenarios = {
            "checksum": lambda: self.released.update({"youtube-download_0.2.0.hyaplugin": package("hydra.youtube", "0.2.1")}),
            "catalog lists hydra.youtube 0.3.0": lambda: self.catalog["plugins"][0].update(version="0.3.0"),
            "not published with plugins/official.pub": lambda: self.catalog["plugins"][1].update(publisher_key="other"),
            "invalid official download": lambda: self.catalog["plugins"][0]["download"][0].update(
                link="https://example.com/youtube-download_0.2.0.hyaplugin"),
        }
        for message, corrupt in scenarios.items():
            with self.subTest(message=message):
                self.setUp()
                corrupt()
                (self.root / "docs/plugins.json").write_text(json.dumps(self.catalog))
                fetch = lambda link, timeout: io.BytesIO(self.released[link.rsplit("/", 1)[-1]])
                with patch.object(module.urllib.request, "urlopen", side_effect=fetch):
                    with self.assertRaisesRegex(ValueError, message):
                        module.collect(self.root, "1.3.0", self.root / "official")

    def test_a_catalog_without_official_packages_is_refused(self):
        self.catalog["plugins"] = self.catalog["plugins"][2:]
        (self.root / "docs/plugins.json").write_text(json.dumps(self.catalog))
        with self.assertRaisesRegex(ValueError, "no official packages"):
            module.collect(self.root, "1.3.0", self.root / "official")


class StageOfficialTests(unittest.TestCase):
    def test_windows_normalization_does_not_hide_backslashes(self):
        name = "native\\linux-x86_64\\torrent.hyaplugin"
        data = bundle([(name, b"bad")])
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            destination = root / "plugins/bundled"
            destination.mkdir(parents=True)
            (destination / "bundle-version.txt").write_text("1.0.0")
            (destination / "youtube.hyaplugin").write_bytes(b"retained")
            with patch.object(zipfile.os, "sep", "\\"):
                with zipfile.ZipFile(io.BytesIO(data)) as archive:
                    entry = archive.infolist()[0]
                    self.assertEqual(entry.orig_filename, name)
                    self.assertEqual(entry.filename, name.replace("\\", "/"))
                with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(data)):
                    with self.assertRaisesRegex(ValueError, "invalid official bundle entry"):
                        module.stage(root, "1.1.0")
            self.assertEqual((destination / "youtube.hyaplugin").read_bytes(), b"retained")
            self.assertEqual((destination / "bundle-version.txt").read_text(), "1.0.0")

    def test_stages_portable_and_all_native_architectures(self):
        entries = [("youtube.hyaplugin", b"portable")]
        entries += [(f"native/{system}-{architecture}/torrent.hyaplugin", b"native")
                    for system in ["macos", "linux", "windows"]
                    for architecture in ["x86_64", "aarch64"]]
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(bundle(entries))):
                destination = module.stage(root, "1.0.0")
            for name, expected in entries:
                self.assertEqual((destination / name).read_bytes(), expected)

    def test_installs_release_and_refreshes_stale_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(bundle([("youtube.hyaplugin", b"v1")]))) as fetch:
                destination = module.stage(root, "1.0.0")
                self.assertEqual((destination / "youtube.hyaplugin").read_bytes(), b"v1")
                fetch.assert_called_once_with("https://github.com/ja7ad/hydra/releases/download/v1.0.0/hydra-official-plugins.zip", timeout=60)
            with patch.object(module.urllib.request, "urlopen") as fetch:
                self.assertEqual(module.stage(root, "1.0.0"), destination)
                fetch.assert_not_called()
            with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(bundle([("youtube.hyaplugin", b"v2")]))):
                module.stage(root, "1.1.0")
            self.assertEqual((destination / "youtube.hyaplugin").read_bytes(), b"v2")
            self.assertEqual((destination / "bundle-version.txt").read_text().strip(), "1.1.0")

    def test_bad_bundles_leave_previous_release_intact(self):
        for entries in [[], [("../escape.hyaplugin", b"bad")], [("key.pub", b"bad")],
                        [("a.hyaplugin", b"first"), ("a.hyaplugin", b"duplicate")],
                        [("native/linux-x86_64/../../escape.hyaplugin", b"bad")],
                        [("native/freebsd-x86_64/torrent.hyaplugin", b"bad")],
                        [("native//linux-x86_64/torrent.hyaplugin", b"bad")],
                        [("native\\linux-x86_64\\torrent.hyaplugin", b"bad")]]:
            with self.subTest(entries=entries), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                destination = root / "plugins/bundled"
                destination.mkdir(parents=True)
                (destination / "bundle-version.txt").write_text("1.0.0")
                (destination / "youtube.hyaplugin").write_bytes(b"retained")
                with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(bundle(entries))):
                    with self.assertRaises(ValueError):
                        module.stage(root, "1.1.0")
                self.assertEqual((destination / "youtube.hyaplugin").read_bytes(), b"retained")

    def test_extraction_limit_rejects_too_many_packages(self):
        data = bundle([(f"plugin-{index}.hyaplugin", b"package") for index in range(65)])
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(data)):
                with self.assertRaisesRegex(ValueError, "extraction limit"):
                    module.stage(root, "1.0.0")
            self.assertFalse((root / "plugins/bundled").exists())

    def test_rejects_oversized_package_and_compressed_bundle(self):
        for entries in [[("large.hyaplugin", b"x" * (16 * 1024 * 1024 + 1))],
                        [(f"plugin-{index}.hyaplugin", b"x" * (16 * 1024 * 1024)) for index in range(5)]]:
            with tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(bundle(entries))):
                    with self.assertRaises(ValueError):
                        module.stage(root, "1.0.0")
                self.assertFalse((root / "plugins/bundled").exists())

    def test_rejects_oversized_download(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(b"x" * (64 * 1024 * 1024 + 1))):
                with self.assertRaisesRegex(ValueError, "exceeds 64 MiB"):
                    module.stage(root, "1.0.0")
            self.assertFalse((root / "plugins/bundled").exists())

    def test_network_failure_does_not_create_partial_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with patch.object(module.urllib.request, "urlopen", side_effect=OSError("offline")):
                with self.assertRaises(OSError):
                    module.stage(root, "1.0.0")
            self.assertFalse((root / "plugins/bundled").exists())


if __name__ == "__main__":
    unittest.main()
