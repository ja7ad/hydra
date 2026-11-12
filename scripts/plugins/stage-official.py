#!/usr/bin/env python3
"""Stage signed portable and platform-specific plugins for application packaging."""
import argparse
import hashlib
import io
import json
import pathlib
import shutil
import tempfile
import tomllib
import urllib.request
import zipfile

RELEASES = "https://github.com/ja7ad/hydra/releases/"
PLATFORMS = {f"{system}-{architecture}" for system in ["linux", "macos", "windows"]
             for architecture in ["x86_64", "aarch64"]}
PACKAGE_LIMIT = 16 * 1024 * 1024


def stage(root, version):
    destination = root / "plugins/bundled"
    marker = destination / "bundle-version.txt"
    if (marker.is_file() and marker.read_text().strip() == version
            and any(destination.glob("*.hyaplugin"))):
        return destination
    address = f"https://github.com/ja7ad/hydra/releases/download/v{version}/hydra-official-plugins.zip"
    with urllib.request.urlopen(address, timeout=60) as response:
        data = response.read(64 * 1024 * 1024 + 1)
    if len(data) > 64 * 1024 * 1024:
        raise ValueError("official bundle exceeds 64 MiB")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        staging = pathlib.Path(temporary) / "bundled"
        staging.mkdir()
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            names = set()
            total = 0
            for entry in archive.infolist():
                name = entry.orig_filename
                parts = pathlib.PurePosixPath(name).parts
                valid_path = (len(parts) == 1 or
                              len(parts) == 3 and parts[0] == "native" and parts[1] in PLATFORMS)
                if (not valid_path or name != entry.filename
                        or name != pathlib.PurePosixPath(name).as_posix()
                        or "\\" in name or "." in parts or ".." in parts
                        or not entry.filename.endswith(".hyaplugin")
                        or entry.filename in names or entry.file_size > PACKAGE_LIMIT):
                    raise ValueError("invalid official bundle entry")
                total += entry.file_size
                if len(names) >= 64 or total > 64 * 1024 * 1024:
                    raise ValueError("official bundle exceeds extraction limit")
                names.add(entry.filename)
                target = staging / entry.filename
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(archive.read(entry))
            if not names:
                raise ValueError("empty official bundle")
        (staging / "bundle-version.txt").write_text(version + "\n")
        if destination.exists():
            shutil.rmtree(destination)
        staging.rename(destination)
    return destination


def collect(root, version, output):
    """Gather the packages the catalog publishes into release assets and an application bundle.

    The application release does not build plugins. It ships the packages the
    Plugins workflow already released, pinned by the hashes in docs/plugins.json
    at the tagged commit.
    """
    catalog = json.loads((root / "docs/plugins.json").read_text())
    key = (root / "plugins/official.pub").read_text().splitlines()[1].strip()
    assets, bundle = output / "assets", output / "bundle"
    assets.mkdir(parents=True)
    bundle.mkdir()
    bundled = []
    for plugin in catalog["plugins"]:
        if not plugin.get("is_official"):
            continue
        if plugin.get("publisher_key") != key:
            raise ValueError(f"{plugin['id']} is not published with plugins/official.pub")
        for choice in plugin["download"]:
            link, platform = choice["link"], choice.get("platform")
            name = link.rsplit("/", 1)[-1]
            expected = choice.get("sha256") or plugin.get("sha256")
            if (not link.startswith(RELEASES) or not name.endswith(".hyaplugin") or not expected
                    or platform not in PLATFORMS | {None} or (assets / name).exists()):
                raise ValueError(f"invalid official download for {plugin['id']}: {link}")
            with urllib.request.urlopen(link, timeout=60) as response:
                data = response.read(PACKAGE_LIMIT + 1)
            if len(data) > PACKAGE_LIMIT or hashlib.sha256(data).hexdigest() != expected:
                raise ValueError(f"{name} does not match the catalog checksum")
            with zipfile.ZipFile(io.BytesIO(data)) as archive:
                manifest = tomllib.loads(archive.read("hydra-plugin.toml").decode())
            if (manifest["id"], manifest["version"]) != (plugin["id"], plugin["version"]):
                raise ValueError(f"{name} contains {manifest['id']} {manifest['version']}, "
                                 f"the catalog lists {plugin['id']} {plugin['version']}")
            (assets / name).write_bytes(data)
            relative = pathlib.PurePosixPath("native", platform, name) if platform else pathlib.PurePosixPath(name)
            (bundle / relative).parent.mkdir(parents=True, exist_ok=True)
            (bundle / relative).write_bytes(data)
            bundled.append(relative)
    if not bundled:
        raise ValueError("the catalog lists no official packages")
    with zipfile.ZipFile(assets / "hydra-official-plugins.zip", "w", zipfile.ZIP_DEFLATED) as archive:
        for relative in sorted(bundled):
            archive.write(bundle / relative, relative.as_posix())
    shutil.copy(root / "plugins/official.pub", assets / "hydra-official-plugins.pub")
    (bundle / "bundle-version.txt").write_text(version + "\n")
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--from-catalog", type=pathlib.Path, metavar="OUTPUT",
                        help="collect the released packages listed in docs/plugins.json instead of a release bundle")
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[2]
    print(collect(root, args.version, args.from_catalog) if args.from_catalog else stage(root, args.version))


if __name__ == "__main__":
    main()
