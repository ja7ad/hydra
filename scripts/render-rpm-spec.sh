#!/usr/bin/env bash
#
# Render packaging/rpm/hydra.spec with a concrete version baked in.
#
# The checked-in spec carries the @HYDRA_VERSION@ placeholder rather than a
# number, because the number cannot live on the rpmbuild command line: Copr
# imports the spec and the tarball into dist-git and then re-runs
# `rpmbuild -bs` in mock *without* the defines we passed, so a spec that only
# learned its version from `--define _version` falls back to whatever default
# it shipped with and then looks for a tarball that was never built
# ("error: Bad file: hydra-<old>.tar.gz: No such file or directory").
#
# Rendering here means Cargo.toml is the single source of truth and there is no
# second copy of the version to keep in sync.
#
# Usage:
#   scripts/render-rpm-spec.sh              # version from [workspace.package]
#   scripts/render-rpm-spec.sh 0.3.14       # explicit version (release tag)
#   scripts/render-rpm-spec.sh 0.3.14 out.spec
#
# Writes to stdout when no output path is given.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
spec_in="$repo_root/packaging/rpm/hydra.spec"
placeholder='@HYDRA_VERSION@'

version=${1:-}
out=${2:-}

if [ -z "$version" ]; then
  version=$(grep -m1 '^version = ' "$repo_root/Cargo.toml" | cut -d'"' -f2)
fi

if [ -z "$version" ]; then
  echo "render-rpm-spec: could not determine a version" >&2
  exit 1
fi

# RPM refuses `-` in Version:, and a release tag may carry a pre-release suffix
# the manifest does not (v0.3.0-rc.2). Fold it into the version the RPM way.
case "$version" in
  *-*)
    echo "render-rpm-spec: pre-release version '$version' cannot be packaged as an RPM Version:" >&2
    echo "render-rpm-spec: pass the base version instead" >&2
    exit 1
    ;;
esac

if ! grep -q "$placeholder" "$spec_in"; then
  echo "render-rpm-spec: $placeholder not found in $spec_in" >&2
  exit 1
fi

rendered=$(sed "s/$placeholder/$version/g" "$spec_in")

# A leftover placeholder would only surface much later, as a Copr build looking
# for hydra-@HYDRA_VERSION@.tar.gz, so fail here instead.
if printf '%s\n' "$rendered" | grep -q "$placeholder"; then
  echo "render-rpm-spec: $placeholder survived substitution" >&2
  exit 1
fi

if [ -n "$out" ]; then
  printf '%s\n' "$rendered" > "$out"
  echo "render-rpm-spec: wrote $out (version $version)" >&2
else
  printf '%s\n' "$rendered"
fi
