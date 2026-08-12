# TheRetroWeb submission — Adaptec AHA-1542CP ROMs

**Target page:** Adaptec AHA-1542CP — https://theretroweb.com/expansioncards/s/adaptec-aha-1542cp (ID 697)
**Provenance:** original card, dumped on an XGecu T76 with minipro; both reads
verified against the manufacturer's on-label 16-bit checksum.

## What this adds / fixes

1. **NEW: BIOS v1.03** (rev E). The page currently has only v1.02 (which is
   actually the older rev D / label "A91E"). This is a genuinely newer revision.
2. **CORRECTION: MCODE "144C"** (U12). The page's current MCODE file
   (`adaptec-2.bin`) is a **bad dump with data bit D3 stuck high**: every one of
   the 12,996 differing bytes is off by exactly +0x08, its reset vector reads
   `CB 6A 08` (invalid) instead of the correct `C3 6A 00` (`JP 0x006A`), and it
   sums to `AA6C` — not the `144C` printed on the chip. The attached file matches
   the on-label checksum and disassembles correctly; it should replace the bad one.

## Files

### AHA-1542CP_BIOS_v1.03_908501-00-E_C7AA.bin
- Version: **1.03**
- Note/label: **AHA-1540CP/1542CP BIOS (U7)** — Adaptec `908501-00 E`, "BIOS C7AA", (c) 1997
- Size: 32768 bytes | sum16 (on-label): **C7AA** (matches label "C7AA")
- CRC32: `CFC15E6C`
- SHA-1: `d281478dc7bee3110bfcb8254958eb55b1f0d5b2`
- SHA-256: `e137a4542dec07610566b8dab5f4831c1beb2eef1732a0fbfe94c641b2e547d9`

### AHA-1542CP_MCODE_908301-00-G_144C.bin  (replaces corrupted adaptec-2.bin)
- Note/label: **MCODE (U12)** — Adaptec `908301-00 G`, "MCODE 144C", (c) 1996
- Size: 32768 bytes | sum16 (on-label): **144C** (matches label "144C")
- CRC32: `59318A17`
- SHA-1: `555369cd1bc74f1a3477fbe77b621786b876765b`
- SHA-256: `7809fa1003b74d4f4f14eb7f27e224425ae92644b8bfe6d821a312382c8429e8`

Both silicon parts are Atmel AT27C256R (electronic ID 0x1E8C), 32Kx8 DIP-28.
The card's CPU is a Zilog Z80 (Z84C0010); the MCODE is that CPU's firmware.

## Description text (paste into the submission)

> Adds AHA-1542CP **BIOS v1.03** (Adaptec 908501-00 rev E, label "C7AA", 1997) —
> the page currently lists only v1.02 (the older rev D / "A91E").
>
> Also **corrects the existing MCODE file** (U12, "144C"): the current upload is
> a bad dump with data line **D3 stuck high** — it sums to AA6C instead of the
> "144C" on the chip, and its Z80 reset vector is corrupted (CB 6A 08 vs the
> correct C3 6A 00). The attached MCODE matches the on-label checksum (0x144C)
> and disassembles correctly. Both dumped from an original card on an XGecu T76
> and verified against the manufacturer's on-label 16-bit sum.

## How to submit
1. Create / log in to a TheRetroWeb account (power-button menu -> register).
2. Open the AHA-1542CP page (ID 697) and submit an improvement / edit.
3. Title: `Adaptec AHA-1542CP`.
4. Paste the description above; attach both .bin files with the versions/notes listed.
5. Optionally flag the corrupted MCODE on their Discord/forum so a curator can
   retire `adaptec-2.bin`.
