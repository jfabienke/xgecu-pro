# Open-source support status for the T76

*Last checked: 2026-08-12*

**Summary:** There is now a substantial open-source effort to *use* the T76 for
chip programming (Matt Brown's minipro fork), plus separate FPGA-bitstream
tooling (radiomanV). The T76's per-operation **FPGA bitstreams are open**, the
**USB host protocol is documented**, and the **firmware-update transport is
reverse-engineered** — but the **encrypted CH569 firmware image itself has not
been decrypted** by anyone publicly. The on-device bootloader holds the key.

## Projects

### Matt Brown (nmatt0) — minipro fork — headline effort

- Repo/branch: <https://gitlab.com/nmatt0/minipro/-/tree/t76-improvements>
- Talk (June 2026): "Open Source Firmware Extraction – Reversing the XGecu T76" — <https://www.youtube.com/watch?v=rq9d00cgoek>

A genuine open-source path to drive the T76 without Xgpro (still a feature
branch, not mainline minipro). As of the May–June 2026 commits:

- **USB protocol documented** — see [t76-protocol.md](t76-protocol.md) (vendored
  from his `t76.md`): endpoints, command bytes, the 128-byte BEGIN_TRANS with
  the FPGA-setup extension.
- **Working read / erase / program** for SPI-NOR, parallel-NOR, NAND (parallel +
  SPI), and eMMC (incl. user/boot1/boot2/rpmb partition selection).
- **Static RE of the Windows binary** — notes cite `Xgpro_T76.exe` /
  `InfoICT76.dll` function addresses (e.g. `t76_load_chip_to_state @0x4eed10`).
- **Chip DB extraction** — a tool parses 35,399 chip descriptors out of
  `InfoICT76.dll` and reverse-engineers the field/variant transforms to
  regenerate minipro's `infoic.xml` (2,028 new V13.19 chips added).
- **FPGA bitstreams** pulled from the XGPro install (the `algoT76/*.alg` files)
  into `algorithm.xml` via `dump-alg-minipro.bash`. Bitstreams are
  firmware-version specific (V13.19 pairs with device fw 00.1.17).
- **Firmware-update transport** reverse-engineered — see
  [firmware-updater.md](firmware-updater.md). He can flash a vendor update from
  minipro, but the firmware payload is transported opaquely; it is **not**
  decrypted host-side.

### radiomanV/Xgecu_T76 — FPGA-side tooling

- Repo: <https://github.com/radiomanV/Xgecu_T76> (The Unlicense)

Confirms the internal FPGA is an **Anlogic Eagle EG4X20** (BG256). Contains a
converter for Anlogic TD IDE bitstreams (`gen_bit.py`), a libusb uploader
(`t76_uploader.py`, Linux/macOS), and a `clock_sniffer` test design. It lets you
*load your own* bitstreams onto the T76's FPGA; it is not a chip-programming
replacement and does **not** touch the firmware updater. radiomanV also maintains
the long-running open-source `TL866` firmware project, but that targets the
Microchip-PIC TL866 family, not the T76's WCH CH569.

### minipro (mainline) — <https://gitlab.com/DavidGriffith/minipro>

Mainline supports TL866CS/A, TL866II+, T48, T56 — **not the T76**. T76 support
lives only in Matt Brown's branch above for now.

## What is still closed

- The **CH569W MCU firmware image** inside `UpdateT76.Dat` is encrypted/obfuscated
  (measured entropy ≈ 7.9 bits/byte) and has **not** been decrypted or extracted
  publicly. The device bootloader decrypts on-flash; the host only ships blocks.

## Re-checking

- Matt Brown's branch: `git ls-remote https://gitlab.com/nmatt0/minipro t76-improvements`
- Mainline minipro for T76: `curl -s https://gitlab.com/DavidGriffith/minipro/-/raw/master/README.md | grep -i t76`
- radiomanV repo: watch <https://github.com/radiomanV/Xgecu_T76>
