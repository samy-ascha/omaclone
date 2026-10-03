# omaclone

Clone the internal disk of one laptop onto the other. Each machine boots the same SystemRescue stick, picks source or target, and the bytes move over a Thunderbolt cable. There is no third machine in the copy.

This was proven on two MacBookPro11,4 machines (15-inch Retina MacBook Pro, Mid 2015): Apple SSD SM0256G, Broadcom BCM43602 Wi-Fi, Intel DSL5520 Thunderbolt 2. A full-disk copy from one to the other booted the cloned OS. On that hardware the copy holds about 10 Gbit/s, so a 234 GiB disk takes about three and a half minutes.

The program is a small Rust TUI. It refuses to write until the target operator types that disk's serial, refuses a target smaller than the source, and checks the copy with BLAKE3.

## Use the sticks

Write `out/systemrescue-13.02-amd64-omaclone.iso` to two USB sticks. A `dd` of the ISO is enough. The stick can stay read-only: SystemRescue runs `autorun/autorun` from the boot medium either way.

Boot both laptops with the Thunderbolt cable between them.

1. **Role.** `s` reads this disk. `t` erases this disk.
2. **Disk.** Model, exact size, serial, and device name. A USB stick is shown as the boot medium, not as a clone disk.
3. **Cable.** Thunderbolt address and link. Green means the value is the one a working link needs. `r` reloads the Thunderbolt driver on this laptop. Both laptops have to do that, together. `t` runs a two-second speed test. Under 5 Gbit/s is red. Drops are packets lost in the last 15 seconds. Any drop in that window is red, and older losses clear.
4. **Confirm.** Target only. The screen says the disk will be erased. Type the serial shown above the prompt. A mismatch stays here.
5. **Ready.** `enter` on both laptops starts the copy. Esc aborts. A finished copy shows the BLAKE3 of the bytes. Enter after that does not start again.

`w` opens Wi-Fi from any clone screen except the serial prompt. That screen is not part of the copy. `d` disconnects. `q` quits, except while a copy is running or while a serial, network name, or password is being typed. `ctrl-c` always quits. Alt+F2 is a login if the TUI is not on tty1. Quitting the TUI brings the tty1 login back.

Boot does not join Wi-Fi and does not set a root password. For SSH, add `rootpass=` on the boot command line of that machine. Do not put that password, or a Wi-Fi passphrase, in this tree.

`sysrescue-autorun.service` can show FAILED in red during boot. The TUI takes tty1 and the autorun process receives SIGHUP. The Thunderbolt reload is scheduled before that handoff. The link is ready when `clx` and `e2e` are off and `thunderbolt0` has an address.

## What the copy will and will not do

The copy is the source disk, byte for byte, over `thunderbolt0` only. The target does not open its disk until it has seen the source serial and size. A peer that is not a link-local address is dropped. If the target is larger, the bytes past the source are left as they are. FileVault and other encrypted volumes stay encrypted. The clone boots with the same accounts and keys as the source.

Serial `S2Z5NY0H998813` can be a source. The program still refuses to write it. That is the development machine's internal disk. Change `CONTROLLER_SERIAL` in `tui/src/model.rs` if a different disk must be protected.

Pulling the cable during the copy leaves the target unbootable. Booting the source and the clone on one network duplicates host identity. Rename the clone before it shares a LAN with the original.

These two ports are one Falcon Ridge controller. A second Thunderbolt cable does not add a second full-speed path, and this program uses one TCP stream on `thunderbolt0`.

## Thunderbolt

A stock boot looks connected and then drops every transmitted frame. The DSL5520 needs both drivers loaded with lane low-power and USB4 end-to-end flow control off, on both laptops, at the same time:

```text
modprobe -r thunderbolt_net thunderbolt
modprobe thunderbolt clx=0
modprobe thunderbolt_net e2e=0
```

`autorun/autorun` does this after handing the console to the TUI. Pressing `r` on only one laptop drops the host-to-host handshake. Press it on both, or boot both from this image. After a good reload a single TCP stream on this hardware is about 10 Gbit/s. The SSD and the CPU are not the limit. The Thunderbolt IP path is.

## Wi-Fi

Wi-Fi is for SSH and for looking around the rescue system. The disk stream does not use it. On this Broadcom chip the TUI reloads `brcmfmac` with `roamoff=1` the first time that screen opens, and sets the regulatory domain to `NL` so a 5 GHz DFS channel can be used. Change that domain in `tui/src/wifi.rs` if you are somewhere else. The program stores a NetworkManager profile after the password is typed. `nmcli device wifi connect` and `nmcli --ask` start WPS on some access points, so the TUI does not use those commands.

## Build

The stick binary is a static musl executable. From a Rust toolchain that has the musl target:

```text
rustup target add x86_64-unknown-linux-musl
cd tui
cargo test
cargo build --release --target x86_64-unknown-linux-musl
```

`cargo run -- --print` prints the internal disk this machine would use and exits. It does not need the musl target. Do not point a copy at a machine whose disk you are not willing to erase.

`tui/deploy.sh HOST ASKPASS` builds that binary and installs it on a live SystemRescue root over SSH. The askpass program must be executable and must print the root password. Each live boot has a new SSH host key. The script keeps that key in a temporary file. If a copy is running, leave it alone. The new binary is what the next start of `omaclone` runs.

## Bake an ISO

`bake.sh` builds the musl binary and calls `sysrescue-customize`. You need a SystemRescue 13.02 ISO and, on `PATH`, `sysrescue-customize`, `mksquashfs`, and `xorriso`. The customize tool is the one documented at <https://www.system-rescue.org/manual/customizing_systemrescue/>. The work directory needs a couple of gigabytes. `/var/tmp` is used unless `BAKE_WORK` is set.

```text
./bake.sh /path/to/systemrescue-13.02-amd64.iso
```

The image is written to `out/systemrescue-13.02-amd64-omaclone.iso`. Pass a second path to write it somewhere else. The recipe is five files: `autorun/autorun`, `sysrescue.d/200-omaclone.yaml`, the binary, `omaclone/omaclone-console`, and `omaclone/omaclone-tui.service`. `out/` is gitignored.

## Layout

| Path | Role |
| --- | --- |
| `tui/` | The program. |
| `autorun/autorun` | Boot script. Installs the binary, starts the TUI, reloads Thunderbolt. |
| `omaclone/` | tty1 wrapper and the systemd unit copied onto the live system. |
| `sysrescue.d/200-omaclone.yaml` | Turns the SystemRescue firewall off before autorun, so SSH can answer. |
| `bake.sh` | Builds the stick binary into a SystemRescue ISO. |

There is no license file yet. Ask before redistributing.
