"""Offline scenarios for planning, naming and publishing official plugin packages."""
import copy
import hashlib
import json
import pathlib
import tempfile
import unittest
import zipfile

import plugin_release as release

REPOSITORY = pathlib.Path(__file__).resolve().parents[2]
KEY = "RWSfixturepublisherkey"
LATEST = "https://github.com/ja7ad/hydra/releases/latest/download/"
TAGGED = "https://github.com/ja7ad/hydra/releases/download/v1.2.0/"


class PluginReleaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        (self.root / "plugins").mkdir()
        (self.root / "plugins/official.pub").write_text(f"untrusted comment: fixture\n{KEY}\n")
        self.plugin("hydra-youtube", "hydra.youtube", "youtube-download", "0.1.3")
        self.plugin("hydra-torrent", "hydra.torrent", "torrent-download", "0.1.0",
                    platforms=release.PLATFORMS, min_hydra="1.0.7")
        self.catalog = {"schema": 1, "name": "Fixture", "plugins": [
            {"id": "hydra.youtube", "name": "YouTube", "version": "0.1.3", "is_official": True,
             "download": [{"caption": "All platforms", "link": LATEST + "youtube-download.hyaplugin"}],
             "image": "plugins/img/hydra.youtube/icon.svg", "sha256": "0" * 64, "publisher_key": "old", "api": 1},
            {"id": "hydra.torrent", "name": "Torrent", "version": "0.1.0", "is_official": True,
             "download": [{"caption": "Linux · 64-bit Intel", "platform": "linux-x86_64", "sha256": "1" * 64,
                           "link": LATEST + "torrent-download-linux-x86_64.hyaplugin"}],
             "publisher_key": "old", "api": 1, "min_hydra": "1.0.0"}]}

    def plugin(self, folder, identity, package, version, platforms=(), min_hydra=None, cargo=None, lock=None):
        directory = self.root / "plugins" / folder
        directory.mkdir(exist_ok=True)
        (directory / "hydra-plugin.toml").write_text(self.manifest(identity, version, platforms, min_hydra))
        (directory / "Cargo.toml").write_text(
            f'[package]\nname = "{folder}"\nversion = "{cargo or version}"\n'
            f'[package.metadata.hydra]\npackage-name = "{package}"\n')
        (directory / "Cargo.lock").write_text(
            f'[[package]]\nname = "base64"\nversion = "0.22.1"\n\n[[package]]\nname = "{folder}"\nversion = "{lock or version}"\n')

    @staticmethod
    def manifest(identity, version, platforms=(), min_hydra=None):
        text = f'id = "{identity}"\nversion = "{version}"\napi = 1\n'
        if min_hydra:
            text += f'min_hydra = "{min_hydra}"\n'
        return text + "".join(f'\n[[native_modules]]\nid = "engine"\nplatform = "{platform}"\nmodule = "m"\n'
                              for platform in platforms)

    def package(self, name, identity, version, platforms=(), body=b"wasm"):
        path = self.root / "signed" / name
        path.parent.mkdir(exist_ok=True)
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr("hydra-plugin.toml", self.manifest(identity, version, platforms))
            archive.writestr("plugin.wasm", body)
        return path

    def torrent_packages(self, version="0.1.0"):
        return [self.package(f"torrent-download_{version}_{platform.replace('-', '_')}.hyaplugin",
                             "hydra.torrent", version, [platform], platform.encode())
                for platform in release.PLATFORMS]

    def plan(self, event="push", ref="refs/heads/main", changed=(), selected="all", force=False):
        return release.plan(event, ref, list(changed), selected, force, self.catalog, self.root)

    def test_portable_packages_carry_the_version_and_native_ones_add_os_and_architecture(self):
        self.assertEqual(release.load("hydra-youtube", self.root).file_names(), ["youtube-download_0.1.3.hyaplugin"])
        torrent = release.load("hydra-torrent", self.root)
        self.assertEqual(torrent.file_name("windows-x86_64"), "torrent-download_0.1.0_windows_x86_64.hyaplugin")
        self.assertEqual(len(set(torrent.file_names())), 6)
        with self.assertRaisesRegex(ValueError, "no package for platform"):
            torrent.file_name()
        with self.assertRaisesRegex(ValueError, "no package for platform"):
            release.load("hydra-youtube", self.root).file_name("linux-x86_64")

    def test_the_real_plugins_load_and_their_names_parse_back(self):
        for plugin in release.official(REPOSITORY):
            for name in plugin.file_names():
                match = release.PACKAGE_FILE.fullmatch(name)
                self.assertEqual((match["package"], match["version"]), (plugin.package, plugin.version))

    def test_a_bump_that_misses_cargo_toml_or_the_lockfile_is_rejected(self):
        self.plugin("hydra-youtube", "hydra.youtube", "youtube-download", "0.2.0", cargo="0.1.3")
        with self.assertRaisesRegex(ValueError, "Cargo.toml version 0.1.3"):
            release.load("hydra-youtube", self.root)
        self.plugin("hydra-youtube", "hydra.youtube", "youtube-download", "0.2.0", lock="0.1.3")
        with self.assertRaisesRegex(ValueError, "Cargo.lock records"):
            release.load("hydra-youtube", self.root)
        self.plugin("hydra-youtube", "hydra.youtube", "youtube-download", "0.2.0-rc")
        with self.assertRaisesRegex(ValueError, "MAJOR.MINOR.PATCH"):
            release.load("hydra-youtube", self.root)

    def test_a_change_without_a_bump_is_tested_and_not_released(self):
        self.assertEqual(self.plan(changed=["plugins/hydra-youtube/src/lib.rs"]),
                         {"plugins": ["hydra-youtube"], "release": []})

    def test_a_bump_on_main_releases_only_the_bumped_plugin(self):
        self.plugin("hydra-youtube", "hydra.youtube", "youtube-download", "0.1.4")
        changed = ["plugins/hydra-youtube/hydra-plugin.toml", "plugins/hydra-youtube/Cargo.toml",
                   "plugins/hydra-youtube/Cargo.lock", "plugins/hydra-torrent/README.md"]
        self.assertEqual(self.plan(changed=changed),
                         {"plugins": ["hydra-torrent", "hydra-youtube"], "release": ["hydra-youtube"]})

    def test_a_pull_request_or_another_branch_never_releases_a_bump(self):
        self.plugin("hydra-youtube", "hydra.youtube", "youtube-download", "0.2.0")
        changed = ["plugins/hydra-youtube/hydra-plugin.toml"]
        self.assertEqual(self.plan("pull_request", "refs/pull/7/merge", changed)["release"], [])
        self.assertEqual(self.plan("workflow_dispatch", "refs/heads/feat/x", selected="hydra-youtube")["release"], [])

    def test_the_catalog_commit_and_unrelated_files_start_nothing(self):
        self.assertEqual(self.plan(changed=["docs/plugins.json", "crates/hydra-core/src/lib.rs", "plugins/README.md", ""]),
                         {"plugins": [], "release": []})

    def test_a_bump_whose_run_was_superseded_is_released_by_the_next_push_to_main(self):
        self.plugin("hydra-youtube", "hydra.youtube", "youtube-download", "0.2.0")
        changed = ["plugins/hydra-torrent/src/lib.rs"]
        self.assertEqual(self.plan(changed=changed),
                         {"plugins": ["hydra-torrent", "hydra-youtube"], "release": ["hydra-youtube"]})
        self.assertEqual(self.plan("pull_request", "refs/pull/7/merge", changed),
                         {"plugins": ["hydra-torrent"], "release": []})

    def test_shared_build_inputs_test_every_plugin_and_plugin_scripts_only_their_own(self):
        self.assertEqual(self.plan(changed=["plugins/build.py"])["plugins"], ["hydra-torrent", "hydra-youtube"])
        self.assertEqual(self.plan(changed=["scripts/plugins/build-torrent-linux.sh"])["plugins"], ["hydra-torrent"])
        self.assertEqual(self.plan(changed=["crates/hydra-plugin-cli/src/sign.rs"])["plugins"], ["hydra-youtube"])

    def test_a_manual_run_picks_one_plugin_and_can_republish_the_current_version(self):
        self.assertEqual(self.plan("workflow_dispatch", selected="hydra-torrent"),
                         {"plugins": ["hydra-torrent"], "release": []})
        self.assertEqual(self.plan("workflow_dispatch", selected="hydra-torrent", force=True),
                         {"plugins": ["hydra-torrent"], "release": ["hydra-torrent"]})
        self.assertEqual(self.plan("workflow_dispatch")["plugins"], ["hydra-torrent", "hydra-youtube"])

    def test_a_release_that_failed_is_retried_by_the_next_run(self):
        self.plugin("hydra-youtube", "hydra.youtube", "youtube-download", "0.1.4")
        self.assertEqual(self.plan(changed=["plugins/hydra-youtube/README.md"])["release"], ["hydra-youtube"])
        self.catalog["plugins"][0]["version"] = "0.1.4"
        self.assertEqual(self.plan(changed=["plugins/hydra-youtube/README.md"])["release"], [])

    def test_a_version_behind_the_catalog_fails_the_plan(self):
        self.catalog["plugins"][0]["version"] = "0.1.10"
        with self.assertRaisesRegex(ValueError, "behind the released 0.1.10"):
            self.plan(changed=["plugins/hydra-youtube/src/lib.rs"])

    def test_package_names_must_match_the_source_and_packaged_manifests(self):
        good = self.package("youtube-download_0.1.3.hyaplugin", "hydra.youtube", "0.1.3")
        plugin, platform = release.check(good, self.root)
        self.assertEqual((plugin.folder, platform), ("hydra-youtube", None))
        native = self.package("torrent-download_0.1.0_macos_aarch64.hyaplugin", "hydra.torrent", "0.1.0", ["macos-aarch64"])
        self.assertEqual(release.check(native, self.root)[1], "macos-aarch64")
        for name, identity, version, platforms, message in [
                ("youtube-download.hyaplugin", "hydra.youtube", "0.1.3", [], "expected NAME_VERSION"),
                ("youtube-download_0.1.2.hyaplugin", "hydra.youtube", "0.1.2", [], "is packaged as youtube-download_0.1.3"),
                ("youtube-download_0.1.3.hyaplugin", "hydra.youtube", "0.1.2", [], "packaged manifest is hydra.youtube 0.1.2"),
                ("youtube-download_0.1.3_linux_x86_64.hyaplugin", "hydra.youtube", "0.1.3", [], "no package for platform"),
                ("torrent-download_0.1.0.hyaplugin", "hydra.torrent", "0.1.0", [], "no package for platform"),
                ("torrent-download_0.1.0_linux_x86_64.hyaplugin", "hydra.torrent", "0.1.0", ["linux-aarch64"], "packaged manifest"),
                ("other-plugin_0.1.0.hyaplugin", "hydra.other", "0.1.0", [], "no official plugin")]:
            with self.subTest(name=name, version=version), self.assertRaisesRegex(ValueError, message):
                release.check(self.package(name, identity, version, platforms), self.root)

    def test_a_portable_release_updates_link_version_hash_and_key_and_nothing_else(self):
        self.plugin("hydra-youtube", "hydra.youtube", "youtube-download", "0.2.0")
        package = self.package("youtube-download_0.2.0.hyaplugin", "hydra.youtube", "0.2.0")
        before = copy.deepcopy(self.catalog)
        entry = release.patch_catalog(self.catalog, "hydra-youtube", [package], "v1.2.0", self.root)["plugins"][0]
        self.assertEqual(entry["version"], "0.2.0")
        self.assertEqual(entry["download"], [{"caption": "All platforms",
                                              "link": TAGGED + "youtube-download_0.2.0.hyaplugin"}])
        self.assertEqual(entry["sha256"], hashlib.sha256(package.read_bytes()).hexdigest())
        self.assertEqual(entry["publisher_key"], KEY)
        self.assertEqual(list(entry), list(before["plugins"][0]))
        self.assertEqual((entry["name"], entry["image"]), ("YouTube", "plugins/img/hydra.youtube/icon.svg"))
        self.assertEqual(self.catalog["plugins"][1], before["plugins"][1])

    def test_a_native_release_lists_every_platform_with_its_own_hash(self):
        self.plugin("hydra-torrent", "hydra.torrent", "torrent-download", "0.1.1",
                    platforms=release.PLATFORMS, min_hydra="1.2.0")
        packages = self.torrent_packages("0.1.1")
        entry = release.patch_catalog(self.catalog, "hydra-torrent", packages, "v1.2.0", self.root)["plugins"][1]
        self.assertEqual([choice["platform"] for choice in entry["download"]],
                         ["linux-x86_64"] + [platform for platform in release.PLATFORMS if platform != "linux-x86_64"])
        for choice in entry["download"]:
            package = next(path for path in packages if path.name.endswith(choice["platform"].replace("-", "_") + ".hyaplugin"))
            self.assertEqual(choice["link"], TAGGED + package.name)
            self.assertEqual(choice["sha256"], hashlib.sha256(package.read_bytes()).hexdigest())
        captions = {choice["platform"]: choice["caption"] for choice in entry["download"]}
        self.assertEqual(captions["linux-x86_64"], "Linux · 64-bit Intel")
        self.assertEqual((captions["macos-aarch64"], captions["windows-aarch64"], captions["macos-x86_64"]),
                         ("macOS · Apple silicon", "Windows · ARM64", "macOS · Intel"))
        self.assertNotIn("sha256", entry)
        self.assertEqual((entry["version"], entry["min_hydra"]), ("0.1.1", "1.2.0"))

    def test_an_incomplete_or_unlisted_release_leaves_the_catalog_untouched(self):
        before = copy.deepcopy(self.catalog)
        with self.assertRaisesRegex(ValueError, "expected exactly"):
            release.patch_catalog(self.catalog, "hydra-torrent", self.torrent_packages()[:5], "v1.2.0", self.root)
        for tag in ["v1.3.0-rc", "latest", "../v1.2.0"]:
            with self.assertRaisesRegex(ValueError, "not a stable release tag"):
                release.patch_catalog(self.catalog, "hydra-torrent", self.torrent_packages(), tag, self.root)
        self.catalog["plugins"].pop(0)
        package = self.package("youtube-download_0.1.3.hyaplugin", "hydra.youtube", "0.1.3")
        with self.assertRaisesRegex(ValueError, "no catalog entry"):
            release.patch_catalog(self.catalog, "hydra-youtube", [package], "v1.2.0", self.root)
        self.assertEqual(self.catalog["plugins"], before["plugins"][1:])

    def test_rewriting_the_published_catalog_keeps_its_formatting(self):
        source = (REPOSITORY / "docs/plugins.json").read_text()
        target = self.root / "plugins.json"
        release.write_catalog(target, json.loads(source))
        self.assertEqual(target.read_text(), source)

    def test_only_the_released_plugins_earlier_assets_are_superseded(self):
        youtube = release.load("hydra-youtube", self.root)
        assets = ["youtube-download.hyaplugin", "youtube-download_0.1.2.hyaplugin", "youtube-download_0.1.3.hyaplugin",
                  "youtube-download-extra_1.0.0.hyaplugin", "torrent-download-linux-x86_64.hyaplugin",
                  "hydra-official-plugins.zip", "SHA256SUMS.txt"]
        self.assertEqual(release.stale_assets(assets, youtube, youtube.file_names()),
                         ["youtube-download.hyaplugin", "youtube-download_0.1.2.hyaplugin"])
        torrent = release.load("hydra-torrent", self.root)
        assets += ["torrent-download_0.0.9_linux_x86_64.hyaplugin", "torrent-download_0.1.0_linux_x86_64.hyaplugin"]
        self.assertEqual(release.stale_assets(assets, torrent, torrent.file_names()),
                         ["torrent-download-linux-x86_64.hyaplugin", "torrent-download_0.0.9_linux_x86_64.hyaplugin"])

    def test_release_checksums_swap_the_plugin_lines_and_keep_the_rest(self):
        package = self.package("youtube-download_0.1.3.hyaplugin", "hydra.youtube", "0.1.3")
        text = f"{'a' * 64}  hydra-1.2.0-linux-amd64.tar.gz\n{'b' * 64}  youtube-download.hyaplugin\n" \
               f"{'c' * 64}  youtube-download_0.1.3.hyaplugin\n"
        updated = release.update_checksums(text, [package], ["youtube-download.hyaplugin"])
        self.assertEqual(updated, f"{'a' * 64}  hydra-1.2.0-linux-amd64.tar.gz\n"
                                  f"{hashlib.sha256(package.read_bytes()).hexdigest()}  youtube-download_0.1.3.hyaplugin\n")

    def test_every_trigger_the_plan_knows_is_a_path_filter_of_the_workflow(self):
        workflow = (REPOSITORY / ".github/workflows/plugins.yml").read_text()
        triggers = [f"plugins/{plugin.folder}/**" for plugin in release.official(REPOSITORY)]
        triggers += release.SHARED_TRIGGERS + sum(release.PLUGIN_TRIGGERS.values(), ())
        for trigger in triggers:
            for event in ("push", "pull_request"):
                section = workflow.split(f"\n  {event}:", 1)[1].split("\n  workflow_dispatch:" if event == "pull_request" else "\n  pull_request:", 1)[0]
                self.assertIn(f"- '{trigger}", section, f"{trigger} missing from on.{event}.paths")


if __name__ == "__main__":
    unittest.main()
