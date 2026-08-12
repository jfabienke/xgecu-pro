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

### The macOS USB blocker (root-caused)

**Symptom.** `minipro --version` (which queries the attached programmer) returns:

```
IO error: bulk_transfer: LIBUSB_ERROR_IO
IO error: expected 5 bytes but 0 bytes transferred
```

**Root cause (confirmed at the IOKit level via `LIBUSB_DEBUG=4`).** The device
enumerates and does control transfers perfectly, but every **bulk** transfer to
the command endpoint fails with a kernel status of **`kIOReturnNotResponding`
(`0xe00002ed`, "device not responding")** at USB 3.0 **SuperSpeed**. That error
is emitted by the Mac's **xHCI controller**, i.e. *below* libusb — the controller
sent the token and the device gave no response at the SuperSpeed link layer. The
likely mechanism is a SuperSpeed link-power-management (U1/U2) or burst
disagreement between this Apple Silicon controller and the T76 — **since
confirmed**: see [ch569-usb3-notes.md](ch569-usb3-notes.md) for the full
root-cause research (firmware-serviced U1/U2 on the CH569, worst-case
advertised exit latencies, strict Apple xHCI).

What was verified:

- Enumerates correctly: VID `0xA466` / PID `0x1A86`, config 1, **interface 0**,
  vendor class `0xFF`, 14 bulk endpoints incl. **EP `0x01` OUT / `0x81` IN**,
  1024-byte packets, **no streams** (`MaxStreams=0`). Connected **directly to the
  built-in `AppleT6000USBXHCI` controller at SuperSpeed** — not behind a hub.
- Control endpoint works (full descriptor reads succeed).
- `libusb_open` + `libusb_claim_interface(0)` succeed.
- Bulk OUT to EP `0x01` → `kIOReturnNotResponding`; the *unused* OUT endpoints
  (`0x02`–`0x07`) merely time out — so the firmware knows EP `0x01`, it just
  won't service it at SuperSpeed.

**Everything tried in software (all failed):** `SET_INTERFACE` alt-setting,
`SET_CONFIGURATION` re-set, config de/reconfigure cycle, `reset_device`,
`clear_halt`, fresh reopen, a 115-iteration warming loop with control keepalives,
a first-touch-after-re-enumeration test, and a faithful replay of the *one*
transient success (a config-cycle that armed the endpoints exactly once, never
reproducible cold). Because the error originates in the controller, swapping USB
libraries or using Apple's native IOKit API would hit the same wall, and macOS
exposes no user-space API to force a device's link speed down.

**What's in the tree for it.** `src/usb_nix.c` `usb_open()` now cycles the T76's
configuration (`0 → 1`) and retries the claim before use — this is the arming
sequence that produced the single success, guarded to VID/PID `0xA466:0x1A86` so
it can't affect the TL866/T48/T56. It makes minipro robust for when the device is
reachable, but does **not** by itself overcome the SuperSpeed controller failure.

**The fix is physical:** force the T76 off SuperSpeed by connecting it through a
**USB 2.0 hub** (High-Speed enumeration sidesteps the failing SuperSpeed bulk
path; minipro treats link speed as cosmetic). Or move it to a **Linux host**. A
Linux VM *on this Mac* (OrbStack, UTM host-USB, Parallels, Docker Desktop) does
**not** help: all are built on Apple's Virtualization.framework, which cannot
capture a physical USB device or pass through the xHCI controller, so macOS
keeps ownership of the physical link and the guest's Linux quirks never reach
it. See [ch569-usb3-notes.md §6](ch569-usb3-notes.md#6-a-linux-vm-on-this-mac-orbstack-etc-does-not-bypass-it).
A native Linux host, or the physical USB 2.0 downgrade, remain the only fixes.

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
