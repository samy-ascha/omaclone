# omaclone

> **Clone one MacBook directly to another over Thunderbolt.**

`omaclone` is a small Rust terminal application and custom SystemRescue image for making a **byte-for-byte clone of one MacBook's internal disk onto another MacBook**.

The two machines boot the same USB stick, you designate one as the source and the other as the target, and the disks are copied directly over a Thunderbolt cable.

There is no third computer involved, and the copy does not use Wi-Fi or your normal network.

---

## At a glance

| | |
| --- | --- |
| **Source → target** | MacBook → MacBook |
| **Connection** | Thunderbolt 2 |
| **Transfer** | Raw disk bytes |
| **Verification** | BLAKE3 |
| **Tested hardware** | MacBook Pro 15" Retina, Mid 2015 |
| **Typical speed** | ~10 Gbit/s |
| **234 GiB disk** | ~3½ minutes |

> ⚠️ **This is destructive software.** The target disk is erased. Make sure you have selected the correct target before starting a copy.

## What it was built for

The project was developed and tested specifically with two:

- **MacBook Pro 15-inch Retina, Mid 2015**
- Model **MacBookPro11,4 / A1398**
- Apple **SM0256G** internal SSD
- Intel **DSL5520 / Thunderbolt 2**
- Broadcom **BCM43602** Wi-Fi

On this hardware, a complete 234 GiB disk clone takes roughly **three and a half minutes**, with the Thunderbolt network reaching around **10 Gbit/s**.

Other hardware may work, but it has not been the target of this project.

## Why?

Sometimes you want an exact copy of a machine rather than a fresh installation and a migration of files.

`omaclone` copies the source disk as a block device, including things such as:

- the partition layout
- operating system
- applications
- user accounts
- configuration
- encrypted volumes
- boot data

The result is intended to be a bootable clone of the source machine.

Because this is a disk-level copy, the clone initially has the same machine identity as the source. **Do not put the source and clone on the same network without changing the clone's hostname and any other identity that needs to be unique.**

---

## How it works

The two machines run the same rescue environment:

```text
       SOURCE                         TARGET
   ┌─────────────┐                ┌─────────────┐
   │  MacBook A  │                │  MacBook B  │
   │             │                │             │
   │ source disk │                │ target disk │
   └──────┬──────┘                └──────▲──────┘
          │                              │
          │       raw disk bytes         │
          └──────── Thunderbolt ─────────┘
                       │
                  thunderbolt0
```

The source reads its disk directly as a block device.

The target writes directly to its disk.

Only the number of bytes occupied by the source disk is copied. If the target is larger, the space beyond the end of the source is left alone.

After the copy, the data is independently verified with **BLAKE3**.

---

## Safety checks

A disk clone is an easy thing to get catastrophically wrong, so `omaclone` deliberately puts several barriers between choosing a target and actually writing to it.

Before the copy starts:

1. Both machines identify their internal disks.
2. You explicitly choose **source** or **target** on each machine.
3. The machines exchange their disk serial numbers and sizes.
4. The target must be at least as large as the source.
5. The target operator must type the **target disk's serial number**.
6. The two machines must have opposite roles.
7. A disk with the configured protected serial number can never be used as a target.
8. The actual copy only uses the Thunderbolt network interface.

The target disk is not opened for writing until the two machines have successfully identified and admitted one another.

After the copy, the source and target independently participate in a **BLAKE3 verification**. The target reports success only when its hash matches the source's hash.

---

## Using the rescue image

The easiest way to use `omaclone` is with the custom SystemRescue ISO produced by this repository.

Write the resulting ISO to **two USB sticks**.

Then:

1. Connect the two MacBooks with the Thunderbolt cable.
2. Boot both machines from the USB sticks.
3. On one machine choose `source`.
4. On the other choose `target`.
5. Check the disks displayed by the TUI.
6. Check the Thunderbolt connection on both machines.
7. On the target, type the displayed disk serial number.
8. Press `Enter` on both machines.
9. Wait for the copy and BLAKE3 verification to finish.

The TUI intentionally makes the process symmetrical: both laptops run exactly the same software and differ only in the role you select.

### The screens

The application walks through a small number of stages:

**Role**

Choose whether this laptop is the source or target.

**Disk**

Shows the detected internal disk, including its model, size, serial number, device name, and partitions.

The boot USB is shown separately so it is clear which disk will actually be cloned.

**Cable**

Shows the state of the Thunderbolt connection, including its address, driver settings, packet drops and an approximate link speed.

**Confirm**

Only shown for the target.

The screen clearly states that the disk will be erased. You must type the disk serial number exactly as displayed.

**Ready**

Both machines wait here until the other side is ready.

Press `Enter` on both machines to start the copy.

**Done**

The final screen shows the completed byte count and the BLAKE3 hash.

---

## Keyboard controls

### During cloning

| Key | Action |
| --- | --- |
| `s` | Select this machine as the source |
| `t` | Select this machine as the target |
| `Enter` | Continue / start the copy |
| `Esc` | Go back / cancel the current stage |
| `q` | Quit |
| `Ctrl-C` | Quit immediately |
| `w` | Open the Wi-Fi screen |
| `r` | Reload the Thunderbolt drivers |
| `t` | Run the Thunderbolt speed test |

`q` and `Esc` cannot accidentally abandon a serial-number entry by being interpreted as commands while you are typing.

While a copy is running, quitting requests that the copy stop instead of silently leaving it running.

---

## Thunderbolt

The most important part of this project is the Thunderbolt networking.

On the supported MacBook hardware, the normal Linux Thunderbolt configuration can appear to work while actually dropping transmitted packets. The Intel DSL5520 controller needs two particular driver parameters disabled:

```sh
modprobe -r thunderbolt_net thunderbolt
modprobe thunderbolt clx=0
modprobe thunderbolt_net e2e=0
```

The rescue image loads the drivers with those settings automatically.

Both machines need to perform this setup. The TUI therefore shows the relevant state rather than simply assuming that an interface called `thunderbolt0` means the connection is usable.

If the link is connected but traffic is not passing, press `r` on **both machines**.

The TUI also runs a short speed test and watches recent packet drops. This makes it possible to distinguish "Thunderbolt is plugged in" from "Thunderbolt is actually usable for a disk clone."

The actual copy uses:

```text
thunderbolt0
```

and a single TCP connection.

Wi-Fi is never used for the disk data.

---

## Wi-Fi

Wi-Fi is optional.

It is provided primarily so that you can SSH into the SystemRescue environment or otherwise inspect the machine while it is running.

Press `w` from the clone screens to open the Wi-Fi interface.

From there you can:

- scan for networks
- connect to a visible network
- connect to a hidden network
- disconnect
- inspect the current connection

The Wi-Fi password is entered interactively and is not stored in this repository.

The Broadcom Wi-Fi driver is also adjusted for the supported hardware when the Wi-Fi screen is opened.

If you use the project outside the Netherlands, note that the current implementation sets the Wi-Fi regulatory domain to `NL`. Change this in `tui/src/wifi.rs` if appropriate for your location.

### SSH

The rescue environment does not set a root password automatically.

If you want SSH access, supply a `rootpass=` boot parameter or set a password yourself after booting.

Do not put passwords or Wi-Fi credentials into this repository.

---

## What gets copied?

The copy is deliberately simple:

```text
source disk
    │
    │  raw bytes
    ▼
Thunderbolt 2
    │
    │  raw bytes
    ▼
target disk
```

The source disk is read directly as a block device.

The target disk is written directly as a block device.

Only the number of bytes occupied by the source disk is copied. If the target is larger, the space beyond the end of the source is left alone.

Encrypted volumes remain encrypted; `omaclone` does not need to understand the filesystem or the encryption scheme.

---

## Important limitations

### Supported hardware

This project is currently a specialized tool, not a general-purpose disk-cloning application.

It was built around the **MacBookPro11,4 / A1398** and its Intel Thunderbolt 2 controller.

The software makes hardware-specific assumptions about:

- the Thunderbolt controller
- its Linux drivers
- the Broadcom Wi-Fi adapter
- the SystemRescue environment
- the internal disk layout

If you have a different Mac, expect to do some work before relying on it.

### Do not interrupt a copy

Disconnecting the Thunderbolt cable or otherwise interrupting a copy can leave the target disk incomplete and unbootable.

If the copy is stopped part-way through, treat the target as invalid and start again.

### Do not immediately network the clone with its source

A byte-for-byte clone has the same operating-system state and machine identity as the source.

Booting both machines onto the same network before changing their identities can cause conflicts.

### The source must fit on the target

The target disk must be at least as large as the source disk.

The tool refuses to start if it is smaller.

### One Thunderbolt path

The two Thunderbolt ports on these machines belong to the same Falcon Ridge controller. Connecting a second Thunderbolt cable does not provide a second independent full-speed path.

`omaclone` currently uses one TCP stream over `thunderbolt0`.

---

## Building the application

The TUI is a small Rust application.

It uses:

- Rust 2024 edition
- `ratatui`
- `blake3`
- `libc`

To build and test it:

```sh
cd tui

cargo test

rustup target add x86_64-unknown-linux-musl

cargo build --release --target x86_64-unknown-linux-musl
```

The resulting binary is:

```text
tui/target/x86_64-unknown-linux-musl/release/omaclone
```

It is a static musl executable intended for the SystemRescue environment.

### Inspect a machine without starting the TUI

Once the `omaclone` binary is built and available in your `PATH`, you can ask it what it thinks the internal disk is:

```sh
omaclone --print
```

This prints information such as:

```text
MacBookPro11,4
APPLE SSD SM0256G
S29CNYDG898371
251000193024 bytes
233.8 GiB
/dev/sda
```

It also reports the detected USB devices and Thunderbolt state.

The `--print` mode does not require the musl target.

> **Warning:** do not point a copy at a machine whose disk you are not willing to erase.

### Deploying a new binary

For development, `tui/deploy.sh` can build the binary and install it into an already-running SystemRescue environment over SSH:

```sh
tui/deploy.sh HOST ASKPASS
```

`ASKPASS` must be an executable program that prints the root password.

Each SystemRescue boot gets a new SSH host key, so the deployment script handles that using a temporary known-hosts file.

Do not use this while a disk copy is running.

---

## Building the SystemRescue ISO

The repository contains everything needed to turn an existing SystemRescue ISO into an `omaclone` rescue image.

You need:

- SystemRescue **13.02**
- `sysrescue-customize`
- `mksquashfs`
- `xorriso`
- `cargo`
- the `x86_64-unknown-linux-musl` Rust target

Then run:

```sh
./bake.sh /path/to/systemrescue-13.02-amd64.iso
```

The resulting image is:

```text
out/systemrescue-13.02-amd64-omaclone.iso
```

You can choose a different destination:

```sh
./bake.sh /path/to/systemrescue-13.02-amd64.iso /path/to/output.iso
```

The build script creates the static Rust binary and adds it, together with the boot scripts and SystemRescue configuration, to the new ISO.

Temporary build files are normally placed under `/var/tmp`. Set `BAKE_WORK` if you want to use another directory.

For example:

```sh
BAKE_WORK=/some/large/directory ./bake.sh /path/to/systemrescue-13.02-amd64.iso
```

The resulting `out/` directory is gitignored.

## Recording the TUI

The repository also contains a small helper for recording the application on the supported MacBook's built-in display.

The HDMI output exposes only the top-left 2560×1440 portion of the 2880×1800 panel. `capture-console` adjusts the console viewport so the entire TUI fits inside the captured area.

Run:

```sh
capture-console
```

and restore the normal console viewport with:

```sh
capture-console restore
```

This is purely a recording aid; it is not required for cloning.

---

## Repository layout

```text
.
├── tui/
│   └──              Rust TUI and cloning implementation
│
├── autorun/
│   └── autorun       SystemRescue boot/initialisation script
│
├── omaclone/
│   ├── omaclone-console
│   ├── capture-console
│   └── omaclone-tui.service
│
├── sysrescue.d/
│   └── 200-omaclone.yaml
│
└── bake.sh            Build the complete SystemRescue image
```

The most interesting parts of the Rust application are:

```text
tui/src/model.rs      TUI state machine and safety gates
tui/src/copy.rs       Thunderbolt protocol and block-device copy
tui/src/disk.rs       Disk and Thunderbolt discovery
tui/src/wifi.rs       Optional Wi-Fi support
tui/src/speed.rs      Thunderbolt throughput test
tui/src/ui.rs         Terminal interface
```

## Project status

This is a **special-purpose tool for a specific piece of hardware**, rather than a polished general-purpose cloning product.

It has successfully been used to clone a complete MacBookPro11,4 internal disk over Thunderbolt and boot the resulting clone.

If you want to adapt it to different hardware, the Thunderbolt setup and hardware-specific assumptions are the first places to look.

## License

`omaclone` is released into the public domain under [The Unlicense](https://unlicense.org/).

Do whatever you want with it. 😄
