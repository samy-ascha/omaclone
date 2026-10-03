# Stick TUI

Two laptops boot the same stick. One is the source, one is the target. The bytes go from disk to disk over Thunderbolt. Build, bake, and boot instructions are in `README.md`.

```text
cd tui && cargo run -- --print
cd tui && cargo run
tui/deploy.sh HOST ASKPASS
```

`deploy.sh` installs the static binary on a live SystemRescue machine. It lasts until the next boot of an image that does not contain it. Restart `omaclone` to run the binary just installed. Do not deploy over a copy that is already running.

## Screens

1. **Role.** `s` source, this disk is read. `t` target, this disk is erased.
2. **Disk.** The internal SSD: model, exact byte size, serial, device. A USB label, when present, is the boot stick.
3. **Cable.** Address, link state, `clx`, and `e2e`. Green is the value a working link needs. `r` reloads Thunderbolt on this laptop. The other laptop has to do the same. Drops are packets lost in the last 15 seconds. Older losses clear. `t` runs a two-second speed test. Under 5 Gbit/s, or any recent drop, is red. A mark on that screen moves while the link is checked.
4. **Confirm.** Target only. The screen says the disk will be erased. The operator types the serial. A mismatch stays on this screen.
5. **Ready.** The serial matched, or this laptop is the source. `enter` starts the copy. Both laptops press it. Esc aborts. A finished copy shows the BLAKE3 of the bytes. Enter after that does not start again.

`w` opens Wi-Fi from any clone screen except the serial prompt. That screen scans, joins a network, disconnects with `d`, or takes a hidden name and password. The bottom `wi-fi` label is green while a network is up. It is not required for cloning. Boot does not join a network. `q` quits, except during a copy and while typing a serial, a network name, or a password. `ctrl-c` always quits.

## Identity

The internal disk is a non-removable block device with a serial. USB disks, empty readers, `zram`, and device-mapper nodes are not candidates. Two internal serials is an error. The program does not guess.

The serial is the identity. Role is chosen on that laptop after boot.

## Copy

`enter` on Ready sends a broadcast on `thunderbolt0` only. The target listens. The source connects. The target does not open its disk until the source serial and size are checked. The copy is the source disk, byte for byte. A smaller target is refused. Serial `S2Z5NY0H998813` is refused on either side. A peer that is not a link-local address is dropped. Wi-Fi is not the copy path. If the target disk is larger, the extra space is left as it is.

The read, the socket write, and the BLAKE3 hash run together. The hash is what the finished screen shows. On the two Mid 2015 machines this was written for, the Thunderbolt IP link is the limit, at about 10 Gbit/s.

## Boot image

`out/systemrescue-13.02-amd64-omaclone.iso` carries the static binary. At boot, autorun copies it to `/usr/local/bin/omaclone`, schedules the Thunderbolt reload, and starts the TUI on tty1. The autorun unit can report FAILED because taking tty1 hangups that process. The reload still runs. Writing the ISO to the sticks is a separate step. An older image comes back only if the machine boots that older image.
