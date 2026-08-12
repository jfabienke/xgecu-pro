# minipro (T76) setup — build, algorithm.xml, and the macOS comms blocker

*Set up 2026-08-12 on this Mac (Apple Silicon, macOS 15).*

Matt Brown's `t76-improvements` minipro fork, built and staged for T76 chip
programming. **Software side is complete and working; live USB comms to the T76
currently fail on macOS** (see [Status](#status)).

The working tree lives in `minipro-t76/` (a clone of the fork) and is
**git-ignored** — it's a 160 MB build with its own `.git`, a 48 MB generated
`algorithm.xml`, and downloaded installer RARs. Reproduce it with the steps below.

## Build

```sh
git clone --depth 1 -b t76-improvements https://gitlab.com/nmatt0/minipro.git minipro-t76
cd minipro-t76

# macOS pkg-config gotcha: libusb + zlib .pc files aren't on the default path.
# libusb via Homebrew; zlib uses Homebrew's versioned macOS stub (pick your OS ver, here 15).
export PKG_CONFIG_PATH="/opt/homebrew/lib/pkgconfig:/opt/homebrew/Library/Homebrew/os/mac/pkgconfig/15:$PKG_CONFIG_PATH"
pkg-config --modversion libusb-1.0 zlib   # sanity: should print two versions

make CC=cc
```

Dependencies present on this machine: `libusb-1.0` (1.0.29), `zlib` (1.2.12),
`bsdtar`, `curl`, `sha256sum` (`/sbin`), `base64`. No `wget` (script falls back to curl).

## Generate `algorithm.xml` (FPGA bitstreams)

The T76 is FPGA-based: most chip families need a per-chip bitstream uploaded
before the session, sourced from `algorithm.xml`. It is **firmware-version
specific** — the bundled script pins **T76 V13.19**, which pairs with device
firmware **00.1.17**.

```sh
bash dump-alg-minipro.bash        # downloads version-pinned RARs, SHA-verifies, builds algorithm.xml
```

Produces (in `minipro-t76/`):

- `algorithm.xml` — 48 MB, 649 T76 algorithms (+ T56). Well-formed.
- `firmware-T76-00.1.17.dat` and the T48/T56/TL866II+ firmware images.

The bundled `infoic.xml` already carries the T76 chip database (**34,607 devices**),
so chip lookups work offline:

```sh
./minipro -q t76 -L W25Q64        # lists W25Q64BV/CV/... with package + adapter variants
```

## Status

| Piece | State |
|---|---|
| Build | ✅ compiles and runs (`minipro 0.7.4`, branch `t76-improvements`) |
| T76 chip database | ✅ 34,607 devices; offline search works |
| `algorithm.xml` bitstreams | ✅ generated, 649 T76 algorithms, fw-matched to 00.1.17 |
| **Live USB comms to the T76** | ❌ **fails on macOS** — see below |

### The macOS USB blocker

`minipro --version` (which queries the attached programmer) returns:

```
IO error: bulk_transfer: LIBUSB_ERROR_TIMEOUT
IO error: expected 5 bytes but 0 bytes transferred
```

Direct libusb probing (bypassing minipro) confirms the device is **fine**, but
macOS/libusb **cannot move data on its bulk endpoints**:

- Enumerates correctly: VID `0xA466` / PID `0x1A86`, config 1, **interface 0**,
  vendor class `0xFF`, 14 bulk endpoints incl. **EP `0x01` OUT / `0x81` IN**,
  1024-byte packets (USB 3.0 SuperSpeed) — exactly what minipro targets.
- `libusb_open` + `libusb_claim_interface(0)` both succeed.
- Yet an 8-byte OUT transfer to EP `0x01` times out with **0 bytes sent**
  (`LIBUSB_ERROR_TIMEOUT`, then `LIBUSB_ERROR_IO` after `libusb_reset_device` +
  `libusb_clear_halt`).

So it is not wrong addressing, a busy interface, or a stale process — it's the
macOS IOUSBHost + libusb layer failing to service this device's bulk pipes.
Matt Brown's T76 work is **Linux-developed and marked experimental**; macOS is
untested territory.

### Recommended path to actually program a chip

1. **Run minipro on Linux** — native, or a Linux VM with the T76 passed through
   (filter VID `0xA466` / PID `0x1A86`). This is the environment the fork was
   built and tested on and is the most likely to just work.
2. Copy `algorithm.xml` and the `infoic.xml` alongside the Linux build (or run
   `sudo make install install-algorithm`), add the udev rule shipped in
   `udev/`, and verify with `minipro --version` — it should print the T76
   firmware (expected **00.1.17**; a mismatch means regenerating `algorithm.xml`
   for that firmware).
3. First hardware step should be **non-destructive**: insert a known chip
   (e.g. a W25Q64 SPI flash), seat it per the Xgpro adapter image, and do a
   **read** (`minipro -p W25Q64@SOIC8 -r dump.bin`) before ever erasing/writing.
4. If you must stay on macOS, this needs upstream libusb/IOUSBHost debugging
   (or a different USB backend) — treat it as unsupported for now.

## Firmware note

`firmware-T76-00.1.17.dat` extracted here is the same encrypted updater analyzed
in [firmware-updater.md](firmware-updater.md) (renamed by the dump script). minipro
can flash it via `minipro --update firmware-T76-00.1.17.dat` **once comms work** —
the payload is still transported opaquely (device bootloader decrypts).
