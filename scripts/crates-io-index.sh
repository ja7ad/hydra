#!/usr/bin/env bash
# Shared crates.io sparse-index helpers for the release workflow.
#
# Sourced by .github/workflows/publish-crates.yml: the check job
# uses it to compare each library manifest against the version live on the
# registry, and the publish job uses it again to skip crates already live and
# to wait for the index to catch up after `cargo publish`.
#
# Run it directly (`scripts/crates-io-index.sh`) to print where each library
# crate stands against crates.io — the same comparison the check job makes.

# crates.io name -> manifest directory: crates/hydra-core holds the hya-core
# crate, crates/hydra-net holds hya-net, crates/hydra-stream holds hya-stream.
crate_dir() {
  echo "hydra-${1#hya-}"
}

# Version from the crate's own manifest. The library crates version
# independently of the workspace product version, so each is read from
# crates/<dir>/Cargo.toml rather than the workspace root.
crate_version() {
  grep -m1 '^version = ' "crates/$(crate_dir "$1")/Cargo.toml" | cut -d'"' -f2
}

sparse_index_url() {
  local name
  name=$(echo "$1" | tr '[:upper:]' '[:lower:]')
  local len=${#name}
  if [ "$len" -eq 1 ]; then
    echo "https://index.crates.io/1/${name}"
  elif [ "$len" -eq 2 ]; then
    echo "https://index.crates.io/2/${name}"
  elif [ "$len" -eq 3 ]; then
    echo "https://index.crates.io/3/${name:0:1}/${name}"
  else
    echo "https://index.crates.io/${name:0:2}/${name:2:2}/${name}"
  fi
}

# crates.io asks for a descriptive user agent on index requests.
crate_user_agent() {
  echo "hydra-release-ci ($1 $2; +https://github.com/ja7ad/hydra)"
}

is_indexed() {
  local name="$1" version="$2"
  curl -sf -A "$(crate_user_agent "$name" "$version")" "$(sparse_index_url "$name")" \
    | grep -q "\"vers\":\"${version}\""
}

# Latest version on the registry, ignoring yanked releases. The sparse index
# serves one JSON object per line in publish order, so the last live line is
# the current version. Empty when the crate has never been published (a 404
# from the sparse index) — checked via HTTP status rather than `curl -f`,
# whose exit 22 on 404 would otherwise abort the caller under `set -e`.
latest_indexed_version() {
  local name="$1" tmp status
  tmp=$(mktemp)
  status=$(curl -s -o "$tmp" -w '%{http_code}' -A "$(crate_user_agent "$name" latest)" "$(sparse_index_url "$name")")
  if [ "$status" = "404" ]; then
    rm -f "$tmp"
    return 0
  fi
  if [ "$status" -ge 400 ]; then
    rm -f "$tmp"
    echo "error: crates.io index request for ${name} failed with HTTP ${status}" >&2
    return 1
  fi
  jq -r 'select(.yanked | not) | .vers' "$tmp" | tail -1
  rm -f "$tmp"
}

# `a` is strictly newer than `b` under version sort. Used to refuse a manifest
# that sits behind what is already on the registry.
version_gt() {
  [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1)" = "$1" ]
}

# `cargo publish` rejects any dependency, including a workspace-internal path
# dep, that carries no version requirement. [workspace.dependencies] in the
# root Cargo.toml intentionally omits one (see commit d8b0285: local builds
# would otherwise need the pin bumped in lockstep with every hya-core/hya-net
# version change). The publish job stamps the pin in here instead, against its
# own ephemeral checkout right before publishing the dependent crates
# (hya-net, hya-stream) — nothing is committed back.
pin_workspace_dependency_version() {
  local name="$1" dir version
  dir=$(crate_dir "$name")
  version=$(crate_version "$name")
  sed -i.bak -E "s#^${name}[[:space:]]*=[[:space:]]*\{[[:space:]]*path[[:space:]]*=[[:space:]]*\"crates/${dir}\"[[:space:]]*\}#${name} = { version = \"${version}\", path = \"crates/${dir}\" }#" Cargo.toml
  rm -f Cargo.toml.bak
}

# --- status report ------------------------------------------------------------
# Only when executed, not when sourced: report each library crate's manifest
# version against the registry, and exit non-zero if one sits behind what is
# already published (the case publish-crates.yml fails on).
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  set -uo pipefail
  command -v jq >/dev/null 2>&1 || {
    echo "error: jq not found (brew install jq)" >&2
    exit 1
  }
  cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" || exit 1

  status=0
  printf '%-10s %-12s %-12s %s\n' CRATE MANIFEST REGISTRY STATUS
  for crate in hya-core hya-net hya-stream; do
    version=$(crate_version "$crate")
    latest=$(latest_indexed_version "$crate")
    if [ -z "$latest" ]; then
      verdict="publish (never published)"
    elif [ "$version" = "$latest" ]; then
      verdict="up to date"
    elif version_gt "$version" "$latest"; then
      verdict="publish (bumped)"
    else
      verdict="ERROR: behind the registry"
      status=1
    fi
    printf '%-10s %-12s %-12s %s\n' "$crate" "$version" "${latest:-none}" "$verdict"
  done
  exit "$status"
fi
