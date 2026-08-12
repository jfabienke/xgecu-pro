# InfoICT76.dll — chip-database binary format

*Reverse-engineered 2026-08-12 from `InfoICT76.dll` (XGPro T76 V13.19) with
radare2. This is everything needed to extract the chip database **statically**,
without executing the DLL (no Wine) and without decompiling it — see the
[`DllDb`](../minipro-rs/crates/minipro-db/src/dll.rs) backend that implements it.*

## The DLL

- **Native (unmanaged) 32-bit x86 PE** — not .NET (CLR data directory empty, no
  `mscoree` import). Built with **Visual Studio 2015, Release** (debug dir
  references `E:\1_VS2015\InfoIC\Release\InfoIC.pdb`, PDB not shipped). Imports
  `KERNEL32`, `ADVAPI32`, and the base `InfoIC.dll`. ImageBase `0x10000000`.
- Sections: `.text` (code), **`.rdata` (~3.8 MB — holds the database)**, `.data`,
  `.rsrc`, `.reloc`.
- Accessor exports: `GetDllInfo`, `GetMfcStru`, `GetIcStru`, `GetIcList`,
  `GetIcMFC`. Disassembling these gave the table addresses directly — the PDB was
  never needed.

## Two-level table structure

```
GetDllInfo   -> manufacturer count = 173   (constant at RVA 0x111e94)
GetMfcStru(i, out): out = *(0x10172790 + i*0x4c)          ; Mfc table, 76-byte entries
GetIcStru(m, k, out): base = *(mfcTable[m] + 0x44)        ; per-mfc IC-array pointer (a VA)
                      out  = *(base + k*0x74), 29 dwords   ; Ic entries, 116 bytes
```

### Manufacturer table (`Mfc`)
- **Location:** RVA `0x172790` (VA `0x10172790`). **173 entries × 0x4c (76) bytes.**

| Offset | Type | Field |
|---|---|---|
| 0x00 | u32 | id |
| 0x04 | u32 | logo id |
| 0x08 | char[20] | short_name (e.g. `WINBOND`) |
| 0x1c | char[40] | long_name (e.g. `Atmel Corporation`) |
| **0x44** | u32 (VA) | **pointer to this manufacturer's Ic array** |
| **0x48** | u32 | **num_ics** (chips for this manufacturer) |

### Chip entry (`Ic`)
- **Location:** per manufacturer, at the VA in `Mfc+0x44`. **`num_ics` × 0x74 (116) bytes.**

| Offset | Type | Field |
|---|---|---|
| 0x00 | u32 | protocol_id (low byte) |
| 0x08 | u32 | type (0=—,1=EEPROM,2=MCU,3=PLD,4=SRAM,5=LOGIC) |
| 0x0c | char[40] | name (e.g. `W25Q64BV`, sometimes `NAME @PKG`) |
| 0x34 | u32 | variant (`>>8` = algorithm number) |
| 0x38 | u32 | code_memory_size |
| 0x3c | u32 | data_memory_size |
| 0x40 | u32 | data_memory2_size |
| 0x48 | u16 | read_buffer_size |
| 0x4a | u16 | write_buffer_size |
| 0x5c | u8[8] | chip_id (JEDEC/electronic id, little-endian bytes) |
| 0x64 | u32 | chip_id_bytes_count |
| 0x70 | u32 | package_details |

## Totals (verified by walking the tables)

- **173 manufacturers**, **35,399 chip descriptors** total — matches Matt Brown's
  independent extraction. (`infoic.xml` carries a filtered subset, ~32,519.)
- Spot check: `WINBOND` → 1013 chips, IC[0] = `W19B320AB @TSOP48` (proto 0x12);
  `W25Q64BV` → proto 0x03, `chip_id ef 40 17` (Winbond JEDEC), 3 bytes — matches
  the value the port's DB tests assert.

## Static extraction recipe

1. Map the PE: read ImageBase + section headers; build `va → file-offset`.
2. **Locate the Mfc table.** Version-robust way: scan for the longest run of
   76-byte structs whose `short_name@0x08` is printable ASCII, `ptr@0x44` is a VA
   inside the image, and `num_ics@0x48` is a sane count. (For V13.19 it's at RVA
   `0x172790`, 173 entries — but derive it, don't hardcode, so other XGPro builds
   work.)
3. For each `Mfc`: follow `ptr@0x44` (VA→file), walk `num_ics` × 116-byte `Ic`
   structs, decode the fields above into a device record.

No DLL execution, no Wine, no decompilation, no PDB.

## Ic field map (verified against `infoic.xml`)

| Ic offset | Device field | Notes |
|---|---|---|
| 0x00 | protocol_id | low byte |
| 0x0c | name | `NAME` or `NAME @PKG` |
| 0x34 | variant (low byte) | **algo number NOT here** — see below |
| 0x38/0x3c/0x40 | code / data / data2 size | |
| 0x44 | chip_info | M27C256B=6, W25Q64BV=0x90 |
| 0x48/0x4a | read / write buffer | |
| 0x4c/0x50 | voltages = `(vpp@0x50<<8)|vcc@0x4c` | M27C256B 0x5070, W25Q64BV 0x0001 |
| 0x54 | page_size | W25Q64BV 0x100 |
| 0x58 | pulse_delay | M27C256B 0x64, W25Q64BV 0x1388 |
| 0x5c / 0x64 | chip_id (big-endian) / chip_id_bytes | |
| 0x68 | pages_per_block | W25Q64BV 2 |
| 0x6c | package_details | (**not 0x70** — that's `flags`) |

## What blocks a full read (not the `opts` transforms)

The `opts`→params decode above is done and verified. Two parameters, however,
are **not in the `Ic` struct at all**, so a DLL-only device can't yet program:

- **Algorithm number** — the *high* byte of `infoic.xml`'s `variant` (e.g. `0x32`
  → `M27C256B` uses `ROM28P32`). The DLL stores only the low byte (`0x11`), so a
  straight read derives `ROM28P00` and loads the wrong bitstream. XGPro assigns
  algorithms through a separate mechanism outside this table.
- **`flags`** (a transform of the value at 0x70) and **`pin_map`** (derived from
  the package layout, not stored).

Recovering the device→algorithm assignment is a distinct RE task — the natural
next step for full DLL-driven programming.

## Other caveats

- Addresses (`0x172790`, count `173`) are specific to this DLL build; the *method*
  (signature-scan for the table) is version-portable.
- `infoic.xml`'s exact names include extra grouping/aliasing; a straight DLL read
  normalizes lightly (`NAME @PKG` → `NAME@PKG`).
- `InfoICT76.dll` imports the base `InfoIC.dll`, but the chip table itself is
  self-contained as mapped above.
