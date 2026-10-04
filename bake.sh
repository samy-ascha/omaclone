#!/bin/sh
# Build the static binary and fold it into a SystemRescue ISO.
# The user installs squashfs-tools (mksquashfs) and libisoburn (xorriso).
# This script never installs packages. It warns and stops when a tool is missing.
# https://www.system-rescue.org/manual/customizing_systemrescue/
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
source_iso=${1:-}
dest=${2:-$root/out/systemrescue-13.02-amd64-omaclone.iso}

if [ -z "$source_iso" ] || [ ! -f "$source_iso" ]; then
    echo "usage: bake.sh SOURCE_ISO [DEST_ISO]" >&2
    exit 2
fi

missing=0
if ! command -v mksquashfs >/dev/null 2>&1; then
    echo "warning: mksquashfs was not found. Install squashfs-tools, then run bake again." >&2
    missing=1
fi
if ! command -v xorriso >/dev/null 2>&1; then
    echo "warning: xorriso was not found. Install libisoburn, then run bake again." >&2
    missing=1
fi
for cmd in sysrescue-customize cargo; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "warning: $cmd was not found on PATH." >&2
        missing=1
    fi
done
if [ "$missing" -ne 0 ]; then
    echo "warning: bake.sh left the packages alone. Install what is missing, then run it again." >&2
    exit 1
fi

cd "$root/tui"
cargo build --release --target x86_64-unknown-linux-musl
bin=$root/tui/target/x86_64-unknown-linux-musl/release/omaclone

parent=${BAKE_WORK:-/var/tmp}
recipe=$(mktemp -d "$parent/omaclone-recipe.XXXXXX")
work=$(mktemp -d "$parent/omaclone-iso-work.XXXXXX")
trap 'rm -rf "$recipe" "$work"' EXIT

mkdir -p "$recipe/iso_add/autorun" \
    "$recipe/iso_add/omaclone" \
    "$recipe/iso_add/sysrescue.d"
install -m 755 "$root/autorun/autorun" "$recipe/iso_add/autorun/autorun"
install -m 755 "$bin" "$recipe/iso_add/omaclone/omaclone"
install -m 755 "$root/omaclone/omaclone-console" "$recipe/iso_add/omaclone/omaclone-console"
install -m 755 "$root/omaclone/capture-console" "$recipe/iso_add/omaclone/capture-console"
install -m 644 "$root/omaclone/omaclone-tui.service" "$recipe/iso_add/omaclone/omaclone-tui.service"
install -m 644 "$root/sysrescue.d/200-omaclone.yaml" "$recipe/iso_add/sysrescue.d/200-omaclone.yaml"

mkdir -p "$(dirname "$dest")"
sysrescue-customize --auto \
    --source="$source_iso" \
    --dest="$dest" \
    --recipe-dir="$recipe" \
    --work-dir="$work" \
    --overwrite

echo "wrote $dest"
