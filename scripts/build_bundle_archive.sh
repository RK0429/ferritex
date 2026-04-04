#!/bin/sh

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)

BUNDLE_SOURCE_DIR="$REPO_ROOT/crates/ferritex-bench/fixtures/bundle"
ARCHIVE_ROOT_DIR="FTX-ASSET-BUNDLE-001"
ARCHIVE_NAME="$ARCHIVE_ROOT_DIR.tar.gz"
DEFAULT_OUTPUT="$REPO_ROOT/tmp/$ARCHIVE_NAME"
OUTPUT_PATH=${1:-$DEFAULT_OUTPUT}
OUTPUT_DIR=$(dirname -- "$OUTPUT_PATH")
NORMALIZED_TOUCH="202601010000.00"
NORMALIZED_MTIME="2026-01-01 00:00:00"

if [ "$(uname -s)" = "Darwin" ]; then
    export COPYFILE_DISABLE=1
fi

if [ ! -f "$BUNDLE_SOURCE_DIR/manifest.json" ] || [ ! -f "$BUNDLE_SOURCE_DIR/asset-index.json" ] || [ ! -d "$BUNDLE_SOURCE_DIR/texmf" ]; then
    echo "bundle fixture is incomplete: $BUNDLE_SOURCE_DIR" >&2
    exit 1
fi

STAGING_DIR=$(mktemp -d "${TMPDIR:-/tmp}/ferritex-bundle.XXXXXX")
trap 'rm -rf "$STAGING_DIR"' EXIT HUP INT TERM

STAGED_BUNDLE_DIR="$STAGING_DIR/$ARCHIVE_ROOT_DIR"
mkdir -p "$STAGED_BUNDLE_DIR"
cp "$BUNDLE_SOURCE_DIR/manifest.json" "$STAGED_BUNDLE_DIR/manifest.json"
cp "$BUNDLE_SOURCE_DIR/asset-index.json" "$STAGED_BUNDLE_DIR/asset-index.json"
cp -R "$BUNDLE_SOURCE_DIR/texmf" "$STAGED_BUNDLE_DIR/texmf"

find "$STAGED_BUNDLE_DIR" -exec touch -t "$NORMALIZED_TOUCH" {} +
mkdir -p "$OUTPUT_DIR"

TAR_BIN=tar
ARCHIVE_TAR_PATH="$STAGING_DIR/archive.tar"
ARCHIVE_GZ_PATH="$STAGING_DIR/$ARCHIVE_NAME"
if command -v gtar >/dev/null 2>&1; then
    TAR_BIN=gtar
fi

if "$TAR_BIN" --version 2>/dev/null | grep -qi 'gnu tar'; then
    "$TAR_BIN" \
        --sort=name \
        --mtime="$NORMALIZED_MTIME" \
        --owner=0 \
        --group=0 \
        --numeric-owner \
        -cf "$ARCHIVE_TAR_PATH" \
        -C "$STAGING_DIR" \
        "$ARCHIVE_ROOT_DIR"
else
    FILE_LIST="$STAGING_DIR/archive-file-list.txt"
    (
        cd "$STAGING_DIR"
        LC_ALL=C find "$ARCHIVE_ROOT_DIR" -print | LC_ALL=C sort
    ) > "$FILE_LIST"
    "$TAR_BIN" \
        --uid 0 \
        --gid 0 \
        --uname '' \
        --gname '' \
        --no-recursion \
        -cf "$ARCHIVE_TAR_PATH" \
        -C "$STAGING_DIR" \
        -T "$FILE_LIST"
fi

gzip -n -c "$ARCHIVE_TAR_PATH" > "$ARCHIVE_GZ_PATH"
mv "$ARCHIVE_GZ_PATH" "$OUTPUT_PATH"

printf '%s\n' "$OUTPUT_PATH"
