# minipro codebase report (nmatt0 `t76-improvements`)

*Reviewed 2026-08-12, against the working tree in `minipro-t76/` (Matt Brown's
fork of DavidGriffith/minipro, `t76-improvements` branch, version 0.7.4).
~19.5 KLOC of C across `src/`.*

## 1. What it is

A command-line driver for XGecu's TL866/T-series chip programmers, GPLv3. Mainline
minipro supports TL866CS/A, TL866II+, T48, T56; **this fork adds the T76**
(device id `MP_T76 = 8`). It is a single C program (`minipro`) plus a set of
data files and helper tools. No GUI, no runtime deps beyond **libusb-1.0** and
**zlib**.

The shallow clone here has one squashed commit (`930e55e`, "add 2028 new T76
chips"), so per-change history isn't available locally — the design intent lives
in the (unusually good) source comments.

## 2. Build & dependencies

- **`Makefile`** — plain make, no autotools/cmake. `pkg-config` resolves
  `libusb-1.0` and `zlib` CFLAGS/LIBS; hard-errors if either is missing. On this
  Mac that required `PKG_CONFIG_PATH` pointing at Homebrew's libusb and the
  versioned zlib stub (see [minipro-setup.md](minipro-setup.md)).
- Object set: `xml jedec ihex srec database` + per-device drivers + `main`.
- Targets of note: `library` (builds `libminipro` static lib — the core is
  reusable), `install-algorithm` (installs the FPGA bitstream DB), `algorithm`
  (unpacks `algorithm.xml`). zlib is used to inflate compressed bitstreams.
- `CFLAGS` default `-g -O0 -Wall -Wextra` — debug build out of the box.

## 3. Architecture

Two data structures carry the whole design (`src/minipro.h`):

- **`device_t`** — a *chip* description loaded from the XML database: name,
  memory sizes (code/data/data2/page), voltages, `flags_t` (can_erase,
  has_chip_id, word_size, …), package details, chip-id, and an embedded
  **`algorithm_t`** (the FPGA `bitstream` + length). This is the "what to do"
  for a given part.
- **`minipro_handle_t`** — the live *programmer* session: model/firmware/serial,
  the libusb handle, `cmdopts`, and a **table of ~35 function pointers**
  (`minipro_begin_transaction`, `read_block`, `write_block`, `get_chip_id`,
  `erase`, `read_fuses`, `firmware_update`, `pin_test`, `logic_ic_test`, …).

**Dispatch is a hand-rolled vtable.** `minipro_open()` in `minipro.c` identifies
the attached programmer, then a `switch` on the model binds those function
pointers to the concrete driver (`tl866a_*`, `tl866iiplus_*`, `t48_*`, `t56_*`,
`t76_*`). Higher-level code in `main.c` only ever calls the generic
`minipro_*()` wrappers, which forward through the pointers. Adding a programmer =
implement the ops + add a `case`. Clean, C-idiomatic polymorphism.

```
main.c  (CLI, orchestration)
  │  calls generic minipro_*()
  ▼
minipro.c  (open/close, dispatch, shared helpers)
  │  function pointers bound per model
  ▼
tl866a.c / tl866iiplus.c / t48.c / t56.c / t76.c   (device drivers)
  │  msg_send / msg_recv (bulk EP1)
  ▼
usb_nix.c (libusb)  |  usb_win.c (WinUSB)
```

### Module tour (`src/`)

| File | Lines | Role |
|---|---|---|
| `main.c` | 3621 | CLI parsing, action dispatch (read/write/erase/verify/blank/logic), file I/O glue, size/ID checks, progress |
| `database.c` | 2156 | Chip DB: parses `infoic.xml`/`logicic.xml`, matches `algorithm.xml` bitstreams, `-L`/`-d` lookups |
| `t76.c` | 2056 | **T76 driver** (the fork's headline) — see §4 |
| `t48.c` / `tl866iiplus.c` / `tl866a.c` / `t56.c` | ~1000–1660 | Other programmer drivers |
| `minipro.c` | 999 | Handle lifecycle, model dispatch, generic wrappers |
| `usb_nix.c` / `usb_win.c` | 472 / 504 | USB transport (bulk EP1 command channel). `usb_nix.c` is the libusb path |
| `prom.c`, `jedec.c`, `srec.c`, `ihex.c` | 428/381/317/283 | File-format + PROM-protocol handlers |
| `xml.c` | 207 | Minimal XML reader for the data files |
| `database.h` | 164 | Protocol/algorithm enums (`IC2_ALG_NAND`, etc.) |
| `dumpic/` | — | Standalone tools that extract the chip DB from XGPro's `InfoIC*.dll` |
| `b64/`, `cdecode.c`, `cencode.c` | — | Base64 (bitstreams are base64 in the XML) |

## 4. The T76 driver (`t76.c`) — the interesting part

The T76 is **FPGA-based**: unlike the TL866 (fixed silicon), every chip family
needs a per-operation **Anlogic EG4 bitstream** uploaded before the session.
`t76.c` is a careful reverse-engineering of XGPro's USB protocol, and the
comments cite the exact vendor function addresses (e.g.
`t76_cmd_24_fpga_register_io @0x455ec0`, `t76_adapter_detect @0x455670`).

Session flow (`t76_begin_transaction`, line 503):
1. **`t76_adapter_init`** — one-time socket-adapter power/init via `0x24` FPGA
   register-I/O commands (power-down → read-adapter-ID → power-up), then
   `0x3e` **pin detection** run twice to configure the socket pin-drivers for the
   package. Skipped and the NAND lines never drive (chip reads as `0xFF`).
2. **`t76_write_bitstream`** (line 116) — `BEGIN_BS`, stream the bitstream in
   504-byte payload chunks (512-byte packets), `END_BS`. A notable fixed bug:
   the vendor sends the real last-block size in `END_BS`; upstream minipro sent
   0, leaving NAND FPGAs mis-finalized — the fork carries the real size for NAND.
3. Per-op **read/write/erase** via `t76_read_block`/`t76_write_block`, and
   `t76_get_chip_id` / `t76_spi_autodetect` for identification.

Coverage implemented: **SPI-NOR, parallel-NOR, NAND (parallel + SPI), and eMMC**.
The eMMC path is substantial — `t76_emmc_bring_up`, `t76_emmc_switch_partition`
(USER/BOOT1/BOOT2/RPMB via the `--partition` CLI option), `t76_emmc_timing`,
and reading real capacity from **EXT_CSD SEC_COUNT** into `handle->emmc_capacity`
(a `uint64_t`, since eMMC exceeds the 32-bit `code_memory_size`). Also present:
fuses, calibration, JEDEC rows, `pin_test`, `logic_ic_test`, and
`t76_firmware_update` (the encrypted-updater transport — see
[firmware-updater.md](firmware-updater.md)).

**RE quality is high and honest.** Comments distinguish confirmed behavior from
bytes "replayed from a capture," flag hardcoded sequences, and leave explicit
TODOs (e.g. derive adapter detection from `t76_adapter_detect` instead of
hardcoding). This is experimental code that knows exactly where its edges are —
consistent with the `--version` warning "T76 support is experimental."

## 5. Data files — all derived from XGecu's own XGPro software

None of the XML content is independently authored. Every file traces back to
XGecu's proprietary XGPro Windows software; the fork ships the *tools to
regenerate the data from the vendor binaries* rather than only shipping the data.

- **`algorithm.xml`** (48 MB, 649 T76 FPGA bitstreams, base64 + zlib) — built by
  `dump-alg-minipro.bash`, which:
  1. downloads the official XGPro installer RARs from the community
     **[Kreeblah/XGecu_Software](https://github.com/Kreeblah/XGecu_Software)**
     GitHub mirror, **SHA-256-pinned** to specific versions (T76 → **V13.19**);
  2. extracts XGecu's own `algoT76/*.alg` files (one per chip algorithm);
  3. for each, slices the FPGA bitstream out at a fixed offset (`tail -c +N`),
     gzip-compresses it, base64-encodes it, and wraps it as
     `<algorithm name= description= bitstream= />`.
  So the bitstreams are **XGecu's actual `.alg` files, recompressed into XML** —
  not reverse-engineered. Hence they are **firmware-version specific** (V13.19
  pairs with device fw 00.1.17); regenerating from the XGPro build that matches
  your device firmware is the clean fix for any bitstream mismatch.
- **`infoic.xml`** (34,607-chip database; the one local commit added 2,028
  V13.19 chips) — extracted from XGecu's `InfoIC*.dll` by the `dumpic/` tools:
  `dump-infoic-dll.c` / `dump-infoic2plus-dll.c` are mingw-compiled, **run under
  Wine**, `LoadLibrary()` the DLL and call its *own exports* (`GetDllInfo`,
  `GetMfcStru`) to walk the chip-descriptor structs and emit JSON (method credited
  to the nullsecurity.org infoic RE writeup); `json_to_devices.py` then transforms
  that JSON into minipro's `infoic.xml`.
- **`logicic.xml`** — logic-IC test vectors, extracted similarly by
  `dumpic/dump-lgc.pl`. `firmware-*.dat` — vendor firmware images for `--update`.

**Licensing note:** this content is XGecu's proprietary data. Shipping the
extraction tooling (you run it against the official installer) rather than
redistributing the chip DB/bitstreams wholesale is a deliberate posture.

## 6. Local modifications in this tree

`src/usb_nix.c` carries our macOS-arming patch (config-cycle + claim retry,
guarded to the T76 VID/PID `0xA466:0x1A86`). It does **not** overcome the
SuperSpeed controller failure — that needs the USB-2.0 physical downgrade
(see [ch569-usb3-notes.md](ch569-usb3-notes.md)). Everything else is stock fork.

## 7. Observations & risks

- **Strengths:** clean vtable architecture; small, dependency-light; reusable
  `libminipro`; the T76 RE is well-documented and defensively coded (drains
  every EP81/EP82 response, warns that an undrained `0xf0` power-down wedges the
  device until replug).
- **Experimental surface:** T76 support is explicitly beta. Some sequences are
  replayed captures, not derived; adapter-ID isn't validated yet; the `END_BS`
  finalization fix is NAND-only pending generalization. Treat writes/erases on
  unusual parts with care; reads are safe.
- **Firmware coupling:** `algorithm.xml` must match device firmware. This unit is
  on 00.1.07 while the DB targets 00.1.17 — reads work, but bitstream-backed ops
  could mismatch (minipro only *warns*).
- **Portability wart:** `pkg-config` data-file paths are Linux-centric; macOS
  needs `PKG_CONFIG_PATH` help. Debug `-O0` build by default.
- **Not mainline:** lives only on this feature branch; upstream minipro has no
  T76. Contributions would need upstreaming.
```
