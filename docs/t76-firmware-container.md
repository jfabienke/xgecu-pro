# The `updateT76.dat` container, and why the payload stays opaque

Analysed 2026-08-25 against `firmware-T76-00.1.17.dat` (256,420 bytes).

Recorded because the wall here is real and cheap to re-hit: anyone hoping to
disassemble the CH569 firmware to recover the HSPI protocol will spend a session
reaching the same conclusion.

**No firmware bytes are reproduced here, and none belong in this repository.**
This documents the container, not the contents.

## Layout

```
   offset 0    +----------------------------------+
               | 16-byte file header              |
   offset 16   +----------------------------------+
               | block 0                          |  276 bytes
               | block 1                          |  276 bytes
               | ...                              |
               | block 928                        |  276 bytes
               +----------------------------------+   929 x 276 = 256,404
```

File header, little-endian:

| offset | field |
|---|---|
| 0 | version word — low half is the firmware version, high half a format tag |
| 4 | CRC-32/IEEE of the body, init `0xffffffff`, **no final complement** |
| 8 | zero |
| 12 | block count |

`minipro-rs` validates all of this in `UpdateFile::parse` and then streams the
blocks **verbatim**. It never decrypts, and `caps.rs` says so plainly:
"transport only; payload stays opaque". The decryption happens in the device's
bootloader.

## Block structure

```
   [ 6-byte nonce | 10 zero bytes ][ 256-byte ciphertext ][ 4-byte tag ]
        0..5           6..15              16..271            272..275
```

Derived from measurement, not assumption:

- **Autocorrelation peaks at lag 276**, the block length — 4.08% coincidence
  against ~0.4% for random. Block-aligned structure.
- **Offsets 6-15 are byte-identical across all 929 blocks** (100% agreement).
  Plaintext padding: a block cipher would not leave ten zeros standing.
- **Offsets 0-5 differ in every block** — 929 distinct values in 929 blocks.
  A per-block nonce. Byte 5 shows only 128 distinct values, so that field is
  plausibly 47 bits rather than 48.
- **Offsets 16 onward show ~1% modal agreement**, i.e. random.
- **Offsets 272-275 differ in every block** — 929 distinct. A tag or checksum
  over the plaintext.

## The payload is properly encrypted

The decisive test is whether identical plaintext yields identical ciphertext:

```
   14,864 sixteen-byte payload chunks
   14,864 distinct
        0 repeated
```

A 256 KB firmware image contains repeated plaintext with certainty — zero fills,
`0xFF` padding, recurring instruction sequences. **Zero repeated ciphertext
chunks** rules out ECB. With a distinct nonce per block, this is a per-block IV
or a stream cipher, competently applied. Whole-image entropy is 7.90 bits/byte.

**The key is in the CH569's bootloader, inside the chip.** Recovering it means
defeating WCH's code-read protection — a hardware attack, on the only programmer
this project has.

## What this means for the HSPI return path

Disassembling the firmware was one of two routes to the vendor's HSPI protocol,
which is what gates a USB return path for FPGA-generated data (see
`fpga/census/README.md` and commit `ef467fc`: the transport is symmetric and the
MCU acknowledges our packets in hardware, then discards them in firmware).

**That route is closed** short of attacking the silicon. The remaining route is
protocol RE from captured command streams, which needs no key.

Worth noting the firmware route was never the better one. The captures already
show the packet framing, CRC and register addressing; what is missing is which
register makes the MCU treat an inbound packet as read data. That is an
iterative bench question, not a disassembly question.
