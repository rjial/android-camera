#!/bin/sh
# Project version (semver) lives in VERSION; Gradle and CMake read it directly, Cargo needs a copy.
#   scripts/version.sh                 print the version
#   scripts/version.sh check [vX.Y.Z]  fail if pc/Cargo.toml (or the given tag) disagrees with VERSION
#   scripts/version.sh bump X.Y.Z      set the version everywhere and turn [Unreleased] into a CHANGELOG section
set -eu
cd "$(dirname "$0")/.."
version=$(tr -d '[:space:]' < VERSION)
cargo_version=$(sed -n 's/^version = "\(.*\)"/\1/p' pc/Cargo.toml | head -n 1)
semver='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'

case "${1:-}" in
"")
    echo "$version"
    ;;
check)
    echo "$version" | grep -Eq "$semver" || { echo "VERSION '$version' is not semver" >&2; exit 1; }
    [ "$cargo_version" = "$version" ] || { echo "pc/Cargo.toml has $cargo_version, VERSION has $version" >&2; exit 1; }
    if [ -n "${2:-}" ] && [ "$2" != "v$version" ]; then
        echo "tag $2 does not match VERSION $version (expected v$version)" >&2
        exit 1
    fi
    grep -q "^## \[$version\]" CHANGELOG.md || { echo "CHANGELOG.md has no section for $version" >&2; exit 1; }
    echo "version $version OK"
    ;;
bump)
    new=${2:?usage: scripts/version.sh bump X.Y.Z}
    echo "$new" | grep -Eq "$semver" || { echo "'$new' is not semver" >&2; exit 1; }
    echo "$new" > VERSION
    sed -i "0,/^version = \".*\"/s//version = \"$new\"/" pc/Cargo.toml
    # Keep Cargo.lock in step without touching dependency versions.
    sed -i "/^name = \"acam\"$/{n;s/^version = \".*\"/version = \"$new\"/}" pc/Cargo.lock
    # Turn the [Unreleased] section into the new version and open a fresh one above it.
    if ! grep -q "^## \[$new\]" CHANGELOG.md; then
        if grep -q '^## \[Unreleased\]' CHANGELOG.md; then
            sed -i "s/^## \[Unreleased\]$/## [Unreleased]\n\n## [$new] - $(date +%Y-%m-%d)/" CHANGELOG.md
        else
            sed -i "0,/^## \[/s//## [Unreleased]\n\n## [$new] - $(date +%Y-%m-%d)\n\n### Changed\n\n- \n\n## [/" CHANGELOG.md
        fi
        sed -i "s|^\[Unreleased\]: .*|[Unreleased]: https://github.com/rjial/android-camera/compare/v$new...HEAD\n[$new]: https://github.com/rjial/android-camera/releases/tag/v$new|" CHANGELOG.md
    fi
    echo "version set to $new; fill in CHANGELOG.md, commit, then: git tag v$new && git push origin v$new"
    ;;
*)
    echo "usage: $0 [check [vX.Y.Z] | bump X.Y.Z]" >&2
    exit 2
    ;;
esac
