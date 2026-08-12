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

## Where does the decryption key come from?

Two different key systems exist on this chip; only the second one protects
`UpdateT76.Dat`.

**Layer 1 — WCH factory ISP (not the one that matters here).** The CH569's
built-in ROM bootloader, which [`wch-ch56x-isp`](https://github.com/hydrausb3/wch-ch56x-isp)
drives, "encrypts" its transport with a trivial XOR keyed to the chip's 64-bit
**Unique ID**. From `wch-ch56x-isp.c`:

```c
for (sum = 0, i = 0; i < dev_uid_len; i++) sum += dev_uid[i]; // sum of UID bytes
memset(xor_key, sum, sizeof(xor_key));                        // all 8 key bytes = that sum
xor_key[7] = xor_key[0] + dev_id;                             // last byte tweaked by device type
unk[5 + i] = data[i] ^ key[i % 8];                            // each byte XORed on the wire
```

The host reads the UID over ISP and derives the key itself, so this is
obfuscation/pairing, not confidentiality — the host already holds the plaintext.
**This is not what guards the XGecu firmware.**

**Layer 2 — XGecu's application-layer encryption (the real one).** The
`UpdateT76.Dat` body is genuinely encrypted (entropy ≈ 7.9) and is moved through
the flashing protocol above **opaquely** — Xgpro, minipro, the `.Dat` file, and
the USB traffic never contain the plaintext or the key. So the key is not
host-side at all; it lives in the **resident XGecu bootloader already running on
the CH569**, which decrypts each block just before writing flash.

It is almost certainly a **single global key, not per-device.** `UpdateT76.Dat`
is one file shipped in the Xgpro installer to every T76 owner; a UID-derived key
(Layer-1 style) could not decrypt on every unit. So the decryption key must be a
constant baked into every T76's firmware (compiled into XGecu's bootloader, or
burned into a data-flash/config region at the factory). The CH569 provides the
means: a hardware **AES/SM4 engine (ECDC module)** plus the read-only Unique ID
(datasheet §2.2.3, Chapter 15). XGecu plausibly runs each block through that AES
engine with the embedded key; whether they additionally mix the UID in for
per-unit *authentication* (while keeping a global *decryption* key) is unknown.

**Why it can't just be dumped.** The obvious attack — read the CH569 flash and
lift the key out of XGecu's bootloader — is blocked by the chip's flash
**read-protection**. The datasheet's `RB_ROM_EXT_RE` bit
("External programmer read Flash ROM enable — `1: Read enabled; 0: Read
protection`") is cleared on a shipped unit, so an external programmer can erase
and reflash but **cannot read code flash back out**. Recovering the key would
require defeating that protection (voltage glitching, decap/microprobing, or an
ISP/ROM-bootloader vulnerability), not a software crack. This is why no public
work — including Matt Brown's — has the plaintext CH569 firmware or the key.

## Open questions

- The header **offset-8 field is unidentified** ("Unknown").
- The **exact cipher and mode** (AES vs. SM4, key length, IV/chaining) inside
  the Layer-2 scheme are unconfirmed — only the container and transport are RE'd.
- Whether the 284-byte block's 28 bytes of non-data overhead include a per-block
  MAC/CRC vs. addressing metadata is not fully documented upstream.
