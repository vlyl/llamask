#!/usr/bin/env bash
set -euo pipefail

tag="${1:-}"
if [[ -z "${tag}" ]]; then
  echo "usage: $0 <git-tag>" >&2
  exit 2
fi

if [[ ! "${tag}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid release tag: ${tag}" >&2
  exit 1
fi

workspace_version="$({
  awk -F '"' '
    /^\[workspace\.package\]$/ { in_workspace_package = 1; next }
    /^\[/ { in_workspace_package = 0 }
    in_workspace_package && /^version = / { print $2; exit }
  ' Cargo.toml
})"
desktop_version="$(node -p "require('./apps/llamask-desktop/package.json').version")"
tauri_version="$(node -p "require('./apps/llamask-desktop/src-tauri/tauri.conf.json').version")"
expected_tag="v${workspace_version}"

if [[ "${tag}" != "${expected_tag}" ]]; then
  echo "tag ${tag} does not match workspace version ${workspace_version}" >&2
  exit 1
fi

if [[ "${desktop_version}" != "${workspace_version}" ]]; then
  echo "desktop package version ${desktop_version} does not match ${workspace_version}" >&2
  exit 1
fi

if [[ "${tauri_version}" != "${workspace_version}" ]]; then
  echo "Tauri version ${tauri_version} does not match ${workspace_version}" >&2
  exit 1
fi

if ! grep -Fq "## [${workspace_version}]" CHANGELOG.md; then
  echo "CHANGELOG.md has no entry for ${workspace_version}" >&2
  exit 1
fi

echo "release versions are consistent: ${workspace_version}"
