#!/usr/bin/env bash
# scripts/next-version.sh — compute the next semver version from conventional
# commits in a git range. Extracted from the release workflow
# (.github/workflows/release.yml) so the bump logic is testable outside
# Actions; see tests/next_version.rs.
#
# usage: next-version.sh <last-version> <git-range>
#   e.g.: next-version.sh 1.5.3 v1.5.3..HEAD
# stdout: the next version (e.g. "1.5.4") — or nothing if no commit in the
#         range warrants a release
# exit:   0 always (unless usage error -> exit 2)
#
# Scans every commit in <git-range> and takes the HIGHEST bump found:
#   major — subject has the `!` breaking-change marker (`feat!:`,
#           `fix(scope)!:`, ...), or the commit's full body contains a line
#           starting `BREAKING CHANGE:` or `BREAKING-CHANGE:` (Conventional
#           Commits 1.0.0 declares the hyphenated footer token synonymous
#           with the spaced one — matching only one form under-bumps a
#           major release to minor/patch).
#   minor — subject matches `^feat(\(scope\))?:`
#   patch — subject matches `^fix(\(scope\))?:` or `^perf(\(scope\))?:`
#   none  — anything else (docs/chore/test/refactor/ci/merge/...)
#
# Does NOT use `git cliff --bumped-version` — its no-bump behavior varies
# across versions; this parser is deterministic and unit-testable.

set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: next-version.sh <last-version> <git-range>" >&2
  exit 2
fi

last_version="$1"
range="$2"

# rank: 0 = no release, 1 = patch, 2 = minor, 3 = major
rank=0

while IFS= read -r sha; do
  [ -z "$sha" ] && continue

  subject=$(git log -1 --format=%s "$sha")
  body=$(git log -1 --format=%B "$sha")

  if [[ "$subject" =~ ^[a-z]+(\([^\)]*\))?!: ]] \
    || printf '%s\n' "$body" | grep -qE '^BREAKING[- ]CHANGE:'; then
    rank=3
    continue
  fi

  if [[ "$subject" =~ ^feat(\([^\)]*\))?: ]]; then
    if [ "$rank" -lt 2 ]; then
      rank=2
    fi
    continue
  fi

  if [[ "$subject" =~ ^(fix|perf)(\([^\)]*\))?: ]]; then
    if [ "$rank" -lt 1 ]; then
      rank=1
    fi
  fi
done < <(git log --format=%H "$range")

if [ "$rank" -eq 0 ]; then
  exit 0
fi

IFS='.' read -r major minor patch <<< "$last_version"

case "$rank" in
  3)
    major=$((major + 1))
    minor=0
    patch=0
    ;;
  2)
    minor=$((minor + 1))
    patch=0
    ;;
  1)
    patch=$((patch + 1))
    ;;
esac

echo "${major}.${minor}.${patch}"
