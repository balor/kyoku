#!/usr/bin/env bash
# Update Formula/kyoku.rb for a published GitHub release.
#
#   ./update-formula.sh 0.2.1
#
# Downloads the three release tarballs (aarch64 macOS, x86_64 macOS,
# x86_64 Linux) from GitHub Releases, computes their SHA-256s, and rewrites
# the version / URLs / hashes in Formula/kyoku.rb next to this script.
#
# Prereq: the tag `v<version>` must already be pushed and the release
# workflow finished (assets attached). Then commit + push the tap repo.
set -euo pipefail

VERSION="${1:?usage: update-formula.sh <version>  (e.g. 0.2.1)}"
FORMULA="$(cd "$(dirname "$0")" && pwd)/Formula/kyoku.rb"
REPO="balor/kyoku"
BASE="https://github.com/${REPO}/releases/download/v${VERSION}"

TARGETS=(
    "aarch64-apple-darwin"
    "x86_64-apple-darwin"
    "x86_64-unknown-linux-gnu"
)

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

declare -A SHA
for target in "${TARGETS[@]}"; do
    asset="kyoku-${VERSION}-${target}.tar.gz"
    echo "==> ${asset}"
    curl -fSL --retry 3 -o "${TMP}/${asset}" "${BASE}/${asset}"
    SHA[$target]="$(shasum -a 256 "${TMP}/${asset}" | cut -d' ' -f1)"
    echo "    sha256: ${SHA[$target]}"
done

# Rewrite version + URLs (single pass), then the sha256 line that follows
# each URL line keyed on the target triple.
sed -i.bak -E \
    -e "s/^  version \".*\"/  version \"${VERSION}\"/" \
    -e "s|/download/v[0-9]+\.[0-9]+\.[0-9]+/kyoku-[0-9]+\.[0-9]+\.[0-9]+-|/download/v${VERSION}/kyoku-${VERSION}-|g" \
    "$FORMULA"

awk \
    -v arm="${SHA[aarch64-apple-darwin]}" \
    -v intel="${SHA[x86_64-apple-darwin]}" \
    -v linux="${SHA[x86_64-unknown-linux-gnu]}" '
    /aarch64-apple-darwin\.tar\.gz/     { print; getline; sub(/sha256 ".*"/, "sha256 \"" arm   "\""); print; next }
    /x86_64-apple-darwin\.tar\.gz/      { print; getline; sub(/sha256 ".*"/, "sha256 \"" intel "\""); print; next }
    /x86_64-unknown-linux-gnu\.tar\.gz/ { print; getline; sub(/sha256 ".*"/, "sha256 \"" linux "\""); print; next }
    { print }
' "$FORMULA" > "$FORMULA.new" && mv "$FORMULA.new" "$FORMULA"

rm -f "$FORMULA.bak"
echo "==> Updated $FORMULA to v${VERSION}"
echo "    Verify with: brew install --build-from-source ./Formula/kyoku.rb  (or: brew style)"