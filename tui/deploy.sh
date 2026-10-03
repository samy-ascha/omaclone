#!/bin/sh
# Copy the static binary onto a running SystemRescue machine and print its disk.
# mv replaces a running binary. The process that is already on screen keeps
# the old one until omaclone is started again. Do not deploy during a copy.
#   ssh -t root@HOST omaclone
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
cd "$root"
target=x86_64-unknown-linux-musl
cargo build --release --target "$target"
bin=$root/target/$target/release/omaclone

if [ "$#" -lt 2 ] || [ $(( $# % 2 )) -ne 0 ]; then
    echo "usage: deploy.sh HOST ASKPASS [HOST ASKPASS ...]" >&2
    echo "example: deploy.sh 192.168.2.13 /tmp/askpass" >&2
    exit 2
fi

export DISPLAY="${DISPLAY:-:0}"
export SSH_ASKPASS_REQUIRE=force
# Each SystemRescue boot generates a new host key. Keep those keys out of the
# user's known_hosts, and accept the key that answers this deploy.
known=$(mktemp)
trap 'rm -f "$known"' EXIT

while [ "$#" -ge 2 ]; do
    host=$1
    ask=$2
    shift 2
    if [ ! -x "$ask" ]; then
        echo "askpass is missing or not executable: $ask" >&2
        exit 1
    fi
    export SSH_ASKPASS=$ask
    opts="-o UserKnownHostsFile=$known -o StrictHostKeyChecking=accept-new -o ConnectTimeout=8 -o PreferredAuthentications=password -o NumberOfPasswordPrompts=1"
    # shellcheck disable=SC2086
    ssh $opts "root@$host" "cat > /tmp/omaclone.new && chmod 755 /tmp/omaclone.new && mv -f /tmp/omaclone.new /usr/local/bin/omaclone && /usr/local/bin/omaclone --print" < "$bin"
    echo "--- $host ready: ssh -t root@$host omaclone ---"
done
