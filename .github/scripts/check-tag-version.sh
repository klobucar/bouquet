#!/usr/bin/env bash
# Fail unless a release tag names exactly the version in a package's manifest.
#
#   check-tag-version.sh <tag> <tag-prefix> <package> [manifest-path]
#
#   check-tag-version.sh v0.1.1    v    bouquet      # crate releases
#   check-tag-version.sh py-v0.1.0 py-v bouquet-py   # Python wheel releases
#
# Registry versions are immutable, so a tag that disagrees with the manifest
# publishes the wrong version under the right name with no way to take it
# back. This runs before anything is uploaded. It is its own script, rather
# than inline workflow shell, so test-check-tag-version.sh can exercise it.
#
# The comparison is exact string equality after removing the prefix: no
# globbing, no regex, no semver parsing. `0.1.1` != `0.1.10`, `0.1.1` !=
# `0.1.1-rc.1`, and a tag with the wrong prefix is rejected outright instead
# of being compared as-is.

set -euo pipefail

tag_full="${1:?usage: $0 <tag> <tag-prefix> <package> [manifest-path]}"
prefix="${2:?missing tag prefix}"
package="${3:?missing package name}"
manifest_path="${4:-}"

case "$tag_full" in
  "$prefix"*) ;;
  *)
    echo "::error::tag '$tag_full' does not start with '$prefix'"
    exit 1
    ;;
esac
tag="${tag_full#"$prefix"}"

args=(--no-deps --format-version 1)
if [ -n "$manifest_path" ]; then
  args+=(--manifest-path "$manifest_path")
fi

# `first(...) // empty` so a missing package yields an empty string we can
# report clearly, rather than `null` or a jq error.
version="$(cargo metadata "${args[@]}" \
  | jq -r --arg p "$package" 'first(.packages[] | select(.name == $p) | .version) // empty')"

if [ -z "$version" ]; then
  echo "::error::package '$package' not found in the workspace"
  exit 1
fi

echo "Cargo.toml: $version"
echo "git tag:    $tag"
if [ "$version" != "$tag" ]; then
  echo "::error::tag $tag_full does not match $package version $version"
  exit 1
fi
