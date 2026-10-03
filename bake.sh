#!/bin/sh
# Build the static binary and fold it into a SystemRescue ISO.
# Requires sysrescue-customize, mksquashfs, and xorriso on PATH.
# https://www.system-rescue.org/manual/customizing_systemrescue/
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
source_iso=${1:-}
dest=${2:-$root/out/systemrescue-13.02-amd64-omaclone.iso}

if [ -z "$source_iso" ] || [ ! -f "$source_iso" ]; then
    echo "usage: bake.sh SOURCE_ISO [DEST_ISO]" >&2
    exit 2
fi

missing=
for cmd in sysrescue-customize mksquashfs xorriso cargo; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        missing="$missing $cmd"
    fi
done
if [ -n "$missing" ]; then
    echo "missing on PATH:$missing" >&2
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
