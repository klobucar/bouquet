#!/usr/bin/env bash
# Tests for check-tag-version.sh.
#
# Builds a throwaway workspace whose package versions we control, then runs
# the checker against good and bad tags and asserts on the exit status and,
# for the interesting failures, on the message. Needs only cargo and jq.
#
#   .github/scripts/test-check-tag-version.sh
#
# Set CHECK_SCRIPT to run the same cases against a different implementation;
# that is how we confirm these tests fail when the check is wrong.

set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
check="${CHECK_SCRIPT:-$here/check-tag-version.sh}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Fixture workspace: `alpha` plays the crate, `beta-py` the bindings (a
# different version, as in the real repo), `gamma` carries a prerelease.
mkdir -p "$work/alpha/src" "$work/beta-py/src" "$work/gamma/src"
for spec in "alpha 0.1.1" "beta-py 0.1.0" "gamma 0.2.0-rc.1"; do
  set -- $spec
  printf '[package]\nname = "%s"\nversion = "%s"\nedition = "2021"\n' "$1" "$2" \
    > "$work/$1/Cargo.toml"
  : > "$work/$1/src/lib.rs"
done
printf '[workspace]\nresolver = "2"\nmembers = ["alpha", "beta-py", "gamma"]\n' \
  > "$work/Cargo.toml"
manifest="$work/Cargo.toml"

failures=0
total=0

# expect <pass|fail> <message-fragment|-> <description> -- <checker args...>
expect() {
  local want="$1" fragment="$2" desc="$3"
  shift 4
  local out status
  out="$("$check" "$@" "$manifest" 2>&1)"
  status=$?
  total=$((total + 1))

  local ok=1
  if [ "$want" = pass ] && [ "$status" -ne 0 ]; then ok=0; fi
  if [ "$want" = fail ] && [ "$status" -eq 0 ]; then ok=0; fi
  if [ "$fragment" != "-" ] && ! grep -qF -- "$fragment" <<<"$out"; then ok=0; fi

  if [ "$ok" -eq 1 ]; then
    printf 'ok   - %s\n' "$desc"
  else
    printf 'FAIL - %s (want %s, exit %s)\n' "$desc" "$want" "$status"
    printf '       output: %s\n' "${out//$'\n'/ | }"
    failures=$((failures + 1))
  fi
}

# --- accepted -------------------------------------------------------------
expect pass - "crate tag matches crate version"            -- v0.1.1 v alpha
expect pass - "python tag matches bindings version"        -- py-v0.1.0 py-v beta-py
expect pass - "prerelease tag matches prerelease manifest" -- v0.2.0-rc.1 v gamma

# --- rejected: the version is wrong ----------------------------------------
expect fail "does not match" "stale tag (older than the manifest)"   -- v0.1.0 v alpha
expect fail "does not match" "tag ahead of the manifest"             -- v0.1.2 v alpha
expect fail "does not match" "python tag using the crate's version"  -- py-v0.1.1 py-v beta-py
expect fail "does not match" "crate tag using the bindings' version" -- v0.1.0 v alpha
expect fail "does not match" "manifest is a prerelease, tag is not"  -- v0.2.0 v gamma
expect fail "does not match" "tag is a prerelease, manifest is not"  -- v0.1.1-rc.1 v alpha

# --- rejected: near-misses a sloppy comparison would let through ------------
expect fail "does not match" "0.1.10 must not match 0.1.1 (prefix)" -- v0.1.10 v alpha
expect fail "does not match" "0.1 must not match 0.1.1 (truncated)" -- v0.1 v alpha
expect fail "does not match" "trailing whitespace"                  -- "v0.1.1 " v alpha
expect fail "does not match" "regex-ish tag (dots are literal)"     -- "v0x1x1" v alpha
expect fail "does not match" "glob-ish tag"                         -- "v0.1.*" v alpha

# --- rejected: the tag belongs to the other package -------------------------
expect fail "does not start with" "python tag fed to the crate check"  -- py-v0.1.1 v alpha
expect fail "does not start with" "crate tag fed to the python check"  -- v0.1.0 py-v beta-py
expect fail "does not start with" "no prefix at all"                   -- 0.1.1 v alpha
expect fail "does not start with" "uppercase prefix is a different tag"  -- V0.1.1 v alpha

# --- rejected: bad input -----------------------------------------------------
expect fail "not found" "package missing from the workspace" -- v0.1.1 v no-such-package
expect fail "-"         "empty tag"                          -- "" v alpha
expect fail "-"         "empty prefix argument"              -- v0.1.1 "" alpha

echo
echo "$((total - failures))/$total passed"
[ "$failures" -eq 0 ]
