#!/usr/bin/env python3
"""Build and validate an official .hyaplugin with the shared authoring tool."""
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parent
sys.path.insert(0, str(root.parent / "scripts/plugins"))
import plugin_release  # noqa: E402

if len(sys.argv) != 2 or Path(sys.argv[1]).name != sys.argv[1]:
    raise SystemExit("usage: python3 plugins/build.py PLUGIN_FOLDER")
plugin = root / sys.argv[1]
output = plugin / plugin_release.load(sys.argv[1]).file_name()
for stale in plugin.glob("*.hyaplugin"):
    stale.unlink()
subprocess.run(["cargo", "run", "--manifest-path", str(root.parent / "Cargo.toml"),
                "-p", "hya-plugin-cli", "--", "build", str(plugin),
                "--output", str(output)], check=True)
print(output)
