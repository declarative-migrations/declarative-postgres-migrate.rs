#!/usr/bin/env bash
# Verify that the current tree can be packaged as a crate and installed as the
# dpm CLI. This script performs no publication, database startup, or migration.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

package_id="$(cargo pkgid)"
version="${package_id##*@}"
if [[ -z "$version" || "$version" == "$package_id" ]]; then
  echo "could not determine the package version from: $package_id" >&2
  exit 1
fi

publish_args=(--dry-run --locked)
if ! git diff --quiet --ignore-submodules -- ||
  ! git diff --cached --quiet --ignore-submodules -- ||
  [[ -n "$(git ls-files --others --exclude-standard)" ]]; then
  publish_args+=(--allow-dirty)
fi
cargo publish "${publish_args[@]}"

install_root="$(mktemp -d "${TMPDIR:-/tmp}/dpm-install.XXXXXX")"
cargo install --path . --locked --root "$install_root"

dpm_bin="$install_root/bin/dpm"
version_output="$(cd "$install_root" && "$dpm_bin" version)"
expected_version="dpm $version"
if [[ "$version_output" != "$expected_version" ]]; then
  echo "installed CLI version mismatch: expected '$expected_version', got '$version_output'" >&2
  exit 1
fi

help_output="$(cd "$install_root" && "$dpm_bin" help)"
grep -Fq "dpm — declarative postgres migrate" <<<"$help_output"
grep -Fq "COMMANDS" <<<"$help_output"
grep -Fq "apply" <<<"$help_output"
grep -Fq "verify" <<<"$help_output"

echo "release contract verified for declarative-postgres-migrate $version"
