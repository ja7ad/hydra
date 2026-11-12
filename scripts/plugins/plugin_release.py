#!/usr/bin/env python3
"""Plan, name, verify and publish official plugin packages for the Plugins workflow."""
import argparse
import dataclasses
import hashlib
import json
import os
import pathlib
import re
import sys
import tomllib
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
PLATFORMS = tuple(f"{system}-{architecture}" for system in ("linux", "macos", "windows")
                  for architecture in ("aarch64", "x86_64"))
CAPTIONS = {"linux": "Linux", "macos": "macOS", "windows": "Windows",
            "macos-aarch64": "Apple silicon", "macos-x86_64": "Intel", "aarch64": "ARM64", "x86_64": "x64"}
# Links name the release a package was uploaded to rather than `latest`, so a
# newer application release cannot leave the catalog pointing at nothing.
DOWNLOADS = "https://github.com/ja7ad/hydra/releases/download/{tag}/"
VERSION = r"\d+\.\d+\.\d+"
PACKAGE_FILE = re.compile(
    rf"(?P<package>[a-z0-9]+(?:-[a-z0-9]+)*)_(?P<version>{VERSION})"
    r"(?:_(?P<platform>(?:linux|macos|windows)_(?:aarch64|x86_64)))?\.hyaplugin")

# Paths outside a plugin's own folder that still decide whether its package
# builds and passes. plugins.yml repeats them in its `paths` filters, because a
# workflow cannot read this file before it has been triggered.
SHARED_TRIGGERS = (".github/workflows/plugins.yml", "plugins/build.py", "scripts/plugins/plugin_release.py",
                   "scripts/plugins/test_plugin_release.py")
PLUGIN_TRIGGERS = {
    "hydra-youtube": ("scripts/plugins/plugin-media-e2e.py", "scripts/plugins/signing-e2e.py",
                      "crates/hydra-plugin/", "crates/hydra-plugin-cli/"),
    "hydra-torrent": ("scripts/plugins/build-torrent-", "scripts/plugins/torrent-windows.Dockerfile",
                      "scripts/plugins/test_torrent_build.py"),
}


@dataclasses.dataclass(frozen=True)
class Plugin:
    folder: str
    id: str
    version: str
    package: str
    platforms: tuple
    api: int
    min_hydra: str | None

    def file_name(self, platform=None):
        if (platform is None) != (not self.platforms) or (platform and platform not in self.platforms):
            raise ValueError(f"{self.folder} has no package for platform {platform}")
        suffix = f"_{platform.replace('-', '_')}" if platform else ""
        return f"{self.package}_{self.version}{suffix}.hyaplugin"

    def file_names(self):
        return [self.file_name(platform) for platform in self.platforms] or [self.file_name()]


def load(folder, root=ROOT):
    directory = root / "plugins" / folder
    manifest = tomllib.loads((directory / "hydra-plugin.toml").read_text())
    cargo = tomllib.loads((directory / "Cargo.toml").read_text())["package"]
    version = manifest["version"]
    if not re.fullmatch(VERSION, version):
        raise ValueError(f"{folder}: version {version} is not MAJOR.MINOR.PATCH")
    if cargo["version"] != version:
        raise ValueError(f"{folder}: Cargo.toml version {cargo['version']} differs from hydra-plugin.toml {version}")
    lock = directory / "Cargo.lock"
    if lock.is_file():
        locked = [package["version"] for package in tomllib.loads(lock.read_text())["package"]
                  if package["name"] == cargo["name"]]
        if locked != [version]:
            raise ValueError(f"{folder}: Cargo.lock records {locked} for {cargo['name']}, expected {version}")
    platforms = tuple(dict.fromkeys(module["platform"] for module in manifest.get("native_modules", [])))
    unknown = set(platforms) - set(PLATFORMS)
    if unknown:
        raise ValueError(f"{folder}: unsupported native platforms {sorted(unknown)}")
    package = cargo.get("metadata", {}).get("hydra", {}).get("package-name", cargo["name"])
    return Plugin(folder, manifest["id"], version, package, platforms, manifest["api"], manifest.get("min_hydra"))


def official(root=ROOT):
    return [load(path.parent.name, root) for path in sorted((root / "plugins").glob("*/hydra-plugin.toml"))]


def released_version(catalog, plugin_id):
    return next((entry["version"] for entry in catalog["plugins"] if entry["id"] == plugin_id), None)


def numeric(version):
    return tuple(int(part) for part in version.split("."))


def plan(event, ref, changed, selected, force, catalog, root=ROOT):
    """Choose the plugins to test and, among them, the ones whose version is ahead of the catalog.

    Comparing against the published catalog instead of the previous commit keeps
    a release retryable: any later push to main or manual run publishes a bump
    whose own run failed or was superseded in the queue, and the catalog commit
    that ends a release is what stops the next run from releasing again.
    """
    tested, released = [], []
    releasable = event != "pull_request" and ref == "refs/heads/main"
    for plugin in official(root):
        published = released_version(catalog, plugin.id)
        ahead = published is None or numeric(plugin.version) > numeric(published)
        if event == "workflow_dispatch":
            wanted = selected in ("all", plugin.folder)
        else:
            prefixes = (f"plugins/{plugin.folder}/",) + SHARED_TRIGGERS + PLUGIN_TRIGGERS.get(plugin.folder, ())
            wanted = any(path.startswith(prefixes) for path in changed) or (releasable and ahead)
        if not wanted:
            continue
        if published and numeric(plugin.version) < numeric(published):
            raise ValueError(f"{plugin.folder}: version {plugin.version} is behind the released {published}")
        tested.append(plugin.folder)
        if releasable and (ahead or force):
            released.append(plugin.folder)
    return {"plugins": tested, "release": released}


def digest(path):
    return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()


def check(path, root=ROOT):
    """Return the plugin and platform a package file belongs to, or raise if its name, the manifest
    it carries and the source manifest disagree."""
    path = pathlib.Path(path)
    match = PACKAGE_FILE.fullmatch(path.name)
    if not match:
        raise ValueError(f"{path.name}: expected NAME_VERSION[_OS_ARCH].hyaplugin")
    plugin = next((plugin for plugin in official(root) if plugin.package == match["package"]), None)
    if plugin is None:
        raise ValueError(f"{path.name}: no official plugin is packaged as {match['package']}")
    platform = match["platform"] and match["platform"].replace("_", "-", 1)
    if path.name != plugin.file_name(platform):
        raise ValueError(f"{path.name}: {plugin.folder} {plugin.version} is packaged as {plugin.file_name(platform)}")
    with zipfile.ZipFile(path) as archive:
        packed = tomllib.loads(archive.read("hydra-plugin.toml").decode())
    carried = [module["platform"] for module in packed.get("native_modules", [])]
    if (packed["id"], packed["version"]) != (plugin.id, plugin.version) or carried != ([platform] if platform else []):
        raise ValueError(f"{path.name}: packaged manifest is {packed['id']} {packed['version']} {carried}")
    return plugin, platform


def caption(platform):
    system, architecture = platform.split("-", 1)
    return f"{CAPTIONS[system]} · {CAPTIONS.get(platform, CAPTIONS[architecture])}"


def patch_catalog(catalog, folder, packages, tag, root=ROOT):
    """Point a plugin's catalog entry at freshly signed packages; every platform must be present."""
    if not re.fullmatch(rf"v{VERSION}", tag):
        raise ValueError(f"{tag} is not a stable release tag")
    plugin = load(folder, root)
    found = {}
    for package in packages:
        owner, platform = check(package, root)
        if owner.folder == folder:
            found[platform] = pathlib.Path(package)
    if sorted(path.name for path in found.values()) != sorted(plugin.file_names()):
        raise ValueError(f"{folder}: expected exactly {plugin.file_names()}, got {sorted(p.name for p in found.values())}")
    entry = next((entry for entry in catalog["plugins"] if entry["id"] == plugin.id), None)
    if entry is None:
        raise ValueError(f"{plugin.id} has no catalog entry; add its description to docs/plugins.json first")
    previous = {choice.get("platform"): choice for choice in entry["download"] if isinstance(choice, dict)}
    listed = list(previous)
    download = []
    # Platforms keep their place in the catalog; new ones follow.
    for platform in sorted(plugin.platforms or (None,), key=lambda p: listed.index(p) if p in listed else len(listed)):
        choice = {"caption": previous.get(platform, {}).get("caption") or (caption(platform) if platform else "All platforms"),
                  "link": DOWNLOADS.format(tag=tag) + found[platform].name}
        if platform:
            choice |= {"platform": platform, "sha256": digest(found[platform])}
        download.append(choice)
    key = (root / "plugins/official.pub").read_text().splitlines()[1].strip()
    entry |= {"version": plugin.version, "download": download, "publisher_key": key, "api": plugin.api}
    for field, value in (("sha256", None if plugin.platforms else digest(found[None])), ("min_hydra", plugin.min_hydra)):
        if value is None:
            entry.pop(field, None)
        else:
            entry[field] = value
    return catalog


def write_catalog(path, catalog):
    pathlib.Path(path).write_text(json.dumps(catalog, indent=2, ensure_ascii=False) + "\n")


def stale_assets(assets, plugin, keep):
    """Names of a plugin's earlier release assets, in the legacy NAME[-OS-ARCH] or versioned form."""
    systems, architectures = "(?:linux|macos|windows)", "(?:aarch64|x86_64)"
    owned = re.compile(rf"{re.escape(plugin.package)}(?:-{systems}-{architectures}"
                       rf"|_{VERSION}(?:_{systems}_{architectures})?)?\.hyaplugin")
    return sorted(name for name in assets if owned.fullmatch(name) and name not in keep)


def update_checksums(text, packages, removed):
    names = {pathlib.Path(package).name for package in packages} | set(removed)
    lines = [line for line in text.splitlines() if line.split(maxsplit=1)[-1].lstrip("*") not in names]
    lines += [f"{digest(package)}  {pathlib.Path(package).name}" for package in sorted(packages)]
    return "\n".join(lines) + "\n"


def main(arguments=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    planner = commands.add_parser("plan", help="choose the plugins to test and release")
    planner.add_argument("--event", required=True)
    planner.add_argument("--ref", required=True)
    planner.add_argument("--changed", type=pathlib.Path, help="file listing changed paths, one per line")
    planner.add_argument("--plugin", default="all")
    planner.add_argument("--force-release", action="store_true")
    namer = commands.add_parser("name", help="print a plugin's package file names")
    namer.add_argument("folder")
    namer.add_argument("--platform", choices=PLATFORMS)
    checker = commands.add_parser("check", help="verify package names against the manifests")
    checker.add_argument("packages", nargs="+", type=pathlib.Path)
    patcher = commands.add_parser("catalog", help="point docs/plugins.json at signed packages")
    patcher.add_argument("folder")
    patcher.add_argument("packages", nargs="+", type=pathlib.Path)
    patcher.add_argument("--tag", required=True, help="release the packages were uploaded to")
    patcher.add_argument("--catalog", type=pathlib.Path, default=ROOT / "docs/plugins.json")
    cleaner = commands.add_parser("stale", help="list a plugin's superseded release assets (asset names on stdin)")
    cleaner.add_argument("folder")
    summer = commands.add_parser("checksums", help="replace a plugin's lines in a release SHA256SUMS.txt")
    summer.add_argument("file", type=pathlib.Path)
    summer.add_argument("packages", nargs="+", type=pathlib.Path)
    summer.add_argument("--removed", nargs="*", default=[])
    args = parser.parse_args(arguments)
    catalog_path = ROOT / "docs/plugins.json"
    if args.command == "plan":
        changed = args.changed.read_text().split("\n") if args.changed else []
        result = plan(args.event, args.ref, changed, args.plugin, args.force_release,
                      json.loads(catalog_path.read_text()))
        lines = [f"{key}={json.dumps(value)}" for key, value in result.items()]
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                output.write("\n".join(lines) + "\n")
        print("\n".join(lines))
    elif args.command == "name":
        plugin = load(args.folder)
        print("\n".join([plugin.file_name(args.platform)] if args.platform else plugin.file_names()))
    elif args.command == "check":
        for package in args.packages:
            plugin, platform = check(package)
            print(f"{package.name}: {plugin.id} {plugin.version} {platform or 'all platforms'}")
    elif args.command == "catalog":
        write_catalog(args.catalog, patch_catalog(json.loads(args.catalog.read_text()), args.folder, args.packages,
                                                   args.tag))
        plugin = load(args.folder)
        print(f"{plugin.id} {plugin.version}")
    elif args.command == "stale":
        plugin = load(args.folder)
        print("\n".join(stale_assets(sys.stdin.read().split(), plugin, plugin.file_names())))
    elif args.command == "checksums":
        args.file.write_text(update_checksums(args.file.read_text(), args.packages, args.removed))


if __name__ == "__main__":
    main()
