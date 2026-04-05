#!/bin/sh

set -eu

if [ "$#" -ne 1 ]; then
    echo "usage: $0 HASH_DIR" >&2
    exit 2
fi

HASH_DIR=$1
LINUX_FILE="$HASH_DIR/hashes-linux.txt"
MACOS_FILE="$HASH_DIR/hashes-macos.txt"
WINDOWS_FILE="$HASH_DIR/hashes-windows.txt"
FIXTURES="article book report letter"
MATCHED_COUNT=0
MISMATCHED_COUNT=0

read_hash() {
    hash_file=$1
    fixture_name=$2

    awk -v fixture="$fixture_name" '
        $1 == fixture {
            print $2
            found = 1
            exit
        }
        END {
            if (!found) {
                exit 1
            }
        }
    ' "$hash_file"
}

for hash_file in "$LINUX_FILE" "$MACOS_FILE" "$WINDOWS_FILE"; do
    if [ ! -f "$hash_file" ]; then
        echo "missing hash file: $hash_file" >&2
        exit 1
    fi
done

for fixture in $FIXTURES; do
    linux_hash=$(read_hash "$LINUX_FILE" "$fixture") || {
        echo "missing fixture entry: $fixture in $LINUX_FILE" >&2
        exit 1
    }
    macos_hash=$(read_hash "$MACOS_FILE" "$fixture") || {
        echo "missing fixture entry: $fixture in $MACOS_FILE" >&2
        exit 1
    }
    windows_hash=$(read_hash "$WINDOWS_FILE" "$fixture") || {
        echo "missing fixture entry: $fixture in $WINDOWS_FILE" >&2
        exit 1
    }

    if [ "$linux_hash" = "$macos_hash" ] && [ "$linux_hash" = "$windows_hash" ]; then
        echo "MATCH    $fixture $linux_hash"
        MATCHED_COUNT=$((MATCHED_COUNT + 1))
    else
        echo "MISMATCH $fixture"
        echo "  linux   $linux_hash"
        echo "  macos   $macos_hash"
        echo "  windows $windows_hash"
        MISMATCHED_COUNT=$((MISMATCHED_COUNT + 1))
    fi
done

echo "Summary: matches=$MATCHED_COUNT mismatches=$MISMATCHED_COUNT total=4"

if [ "$MISMATCHED_COUNT" -ne 0 ]; then
    exit 1
fi
