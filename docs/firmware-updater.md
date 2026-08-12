# T76 firmware updater (`UpdateT76.Dat`)

*Analysis as of 2026-08-12. Sources: local extraction of Xgpro T76 V13.21 +
Matt Brown's minipro `t76-improvements` branch (`src/t76.c`).*

`UpdateT76.Dat` ships inside the Xgpro installer (SFX payload) and carries the
firmware image for the T76's **WCH CH569W** MCU. In V13.21 it is 256,420 bytes
and pairs with device firmware **00.1.17** (`0x111`).

## Key point: the payload is encrypted; only the transport is open

The firmware **image is encrypted/obfuscated** — measured entropy ≈ **7.9
bits/byte**, ~4% zero bytes, no plaintext structure. Matt Brown reverse-engineered
the **container format and the bootloader flashing protocol** (so minipro can
push an update on Linux/macOS), but the firmware bytes are moved through
**opaquely** — never decrypted host-side. The **on-device bootloader** performs
decryption when it writes to CH569 flash. So the plaintext CH569 firmware has
**not** been extracted by any public work, including his.

Contrast: the per-operation FPGA bitstreams (`algoT76/*.alg`) are stored in the
clear (entropy ≈ 3.3, ~55% zeros) — see [bitstream notes](../README.md) / the
`.alg` structure. The updater is the one blob XGecu actually protects.

## File structure (per `src/t76.c`)

```
+--------------------------------------------------------------+
| 16-byte header                                               |
|  offset 0  : file version        (u32 LE)                    |
|  offset 4  : CRC-32 of payload    (u32 LE, over body)        |
|  offset 8  : Unknown              (u32 LE)  <- not RE'd       |
|  offset 12 : block count          (u32 LE)                   |
+--------------------------------------------------------------+
| block[0]   : 284 bytes (0x114)  = 256 B firmware data + meta |
| block[1]   : 284 bytes                                       |
|   ...                                                        |
| block[N-1] : 284 bytes                                       |
+--------------------------------------------------------------+

file_size == 16 + block_count * 0x114
CRC-32 (poly 0xFFFFFFFF init) over bytes [16 .. end] must match header offset 4.
Each block flashes 256 bytes of firmware at an incrementing address (step 256).
```

## Bootloader flashing protocol (host side)

Command bytes on EP1 (`0x01` OUT / `0x81` IN), from `t76.c`:

| Step | Opcode | Notes |
|---|---|---|
| Switch to bootloader | `0x3A` (`T76_SWITCH`) + `0xAA`, magic `T76_BTLDR_MAGIC` @msg[4] | then device reset + re-open |
| Erase | `0x3C` (`T76_BOOTLOADER_ERASE`) + `0xAA` | whole-device erase |
| Begin write | `0x3B` (`T76_BOOTLOADER_WRITE`) + `0xAA` | |
| Write block | `0x3B`, len `0x0100`, address @msg[4], 0x114-byte block @msg[8]; sent as a 0x11c-byte USB message | repeated `block_count` times |

Guards implemented before flashing: file size 16 B–1 MiB, version mask check,
`blocks*0x114 + 16 == file_size`, and the CRC-32 match. Reference constants in
the source: `LAST_BLOCK_ADDR 0x049f00`, `LAST_BLOCK_CRC 0xcdef8668`.

## Open questions

- The header **offset-8 field is unidentified** ("Unknown").
- The **encryption scheme** guarding the firmware body is unknown; decrypting it
  would require dumping/reversing the CH569 bootloader (which holds the key).
- Whether the 284-byte block's 28 bytes of non-data overhead include a per-block
  MAC/CRC vs. addressing metadata is not fully documented upstream.
