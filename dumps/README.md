# Chip dumps — Adaptec AHA-1542CP (ISA Fast SCSI-2 HBA)

Both ROMs off an Adaptec **AHA-1542CP**, read on an **XGecu T76** (macOS, USB 2.0)
with minipro (`t76-improvements`), `M27C256B@DIP28` algorithm. The card is
driven by a **Zilog Z80 (Z84C0010, 10 MHz)** — the MCODE is that CPU's firmware.

Both silicon parts read as **Atmel AT27C256R** (electronic ID `0x1E8C`), 32K×8
DIP-28 EPROMs. Reads are non-destructive.

## Verification (manufacturer ground truth)

The 4-hex code on each Adaptec label is a **16-bit sum of the full 32 KB image**.
Both dumps reproduce their printed label checksum exactly — independent proof the
reads are byte-perfect:

| File | Adaptec label | Computed sum16 | SHA-256 | CRC-32 |
|---|---|---|---|---|
| MCODE | `908301-00 G` · MCODE **144C** · ©1996 | `0x144C` ✅ | `7809fa1003b74d4f4f14eb7f27e224425ae92644b8bfe6d821a312382c8429e8` | `59318A17` |
| BIOS  | `908501-00 E` · BIOS **C7AA** · ©1997 · v1.03 | `0xC7AA` ✅ | `e137a4542dec07610566b8dab5f4831c1beb2eef1732a0fbfe94c641b2e547d9` | `CFC15E6C` |

Each was also read multiple times to byte-identical results. Pin-detect and the
electronic-ID check false-alarmed on the oxidized 30-year-old pins (contact spray
+ reseat fixed the read); the label-checksum match is the authoritative proof.

## Contents

- **MCODE** — Z80 program ROM: reset `JP 0x006A`, NMI @0x0066 `DI; JP`,
  IX/IY/ED opcodes throughout. Full 32 KB used.
- **BIOS** — x86 option ROM: `55 AA` header, strings
  "Adaptec AHA-1540CP/1542CP BIOS v1.03", "Copyright Adaptec, Inc. 1997",
  "Press <Ctrl><A> for SCSISelect(TM) Utility!". Note the raw EPROM does **not**
  zero-sum over the 16 KB option-ROM length (it's 254, not 0) — expected, because
  the 1542CP serves its PC BIOS via Z80/banking logic rather than a flat map.

## Preservation note

These revisions appear **newer than MAME's** archived 1542CP set (MAME has BIOS
rev D "A91E" / MCODE rev F "17C9"; these are BIOS rev **E** "C7AA" v1.03 / MCODE
rev **G** "144C"). Candidates to contribute to MAME (`src/devices/bus/isa/aha1542c.cpp`).

## Socket designators

Per the MAME 1542CP layout: **BIOS = U7**, **MCODE = U12**. (The 1542C**F** uses
MCODE U15 / BIOS U16 — a different board.)
