# Chip dumps

## AHA-1542CP_U15_MCODE_AT27C256R.bin

Microcode ("MCODE") EPROM from an Adaptec **AHA-1542CP** ISA SCSI host adapter.

| Field | Value |
|---|---|
| Source card | Adaptec AHA-1542CP |
| Socket / label | U15, Adaptec "553801-00 … MCODE" |
| Chip (as read) | **Atmel AT27C256R** (electronic ID `0x1E8C`), 32K×8, DIP-28 EPROM |
| Read with | XGecu T76 + minipro `t76-improvements`, `M27C256B@DIP28` algorithm, USB 2.0 |
| Size | 32,768 bytes (full ROM, no padding) |
| SHA-256 | `7809fa1003b74d4f4f14eb7f27e224425ae92644b8bfe6d821a312382c8429e8` |
| Verified | two independent reads byte-identical |
| Contents | **Z80 microcode** — reset `JP 0x006A`; NMI @0x0066 `DI; JP`; IX/IY/ED opcodes present |

Read is non-destructive; the electronic-ID/pin-detect warnings during capture
were due to oxidized 30-year-old pins (contact spray + reseat resolved the read),
not a data problem — confirmed by the two-read match.

> The chip is an Atmel AT27C256R here; Adaptec also shipped these with ST
> M27C256B. Both are 27C256-class (same pinout/read protocol), so the
> `M27C256B@DIP28` algorithm reads either correctly.
