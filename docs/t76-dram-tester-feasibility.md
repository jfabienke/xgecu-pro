# Turning the T76 into a DRAM tester — feasibility, and the reverse engineering it would take

*Analysis 2026-08-23, prompted by comparing the XGecu programmers with the
[Retro Chip Tester Professional](https://8bit-museum.de/hardware-projekte-chip-tester-english/).
Every claim below is marked **measured** (taken from this machine or the
attached T76), **documented** (from a vendor datasheet or repo), or
**inferred**. The distinction matters: the plan's biggest risk turns on a
question that is currently only inferred.*

## Summary

The T76 could plausibly test **single-rail +5 V asynchronous DRAM** — the 64 Kbit
generation onward — but nothing before it, and no DDR-era part at any effort.
Two milestones separate "interesting" from "useful", and they have very
different risk profiles:

- **MS1 — a DRAM controller in the FPGA.** Bounded work. Everything it needs is
  already documented and the hardest bring-up problem (pin constraints) is
  solved by an existing open example.
- **MS2 — getting results back to the host.** The link is a *documented* vendor
  peripheral, not a bespoke bus, which makes this far more tractable than it
  first appears. One unresolved question could still stop it dead.

## What the T76 already does with RAM

**Measured.** The catalog ships three RAM bitstreams — `RAM3250`, `RAM3251`,
`RAM3665` — covering **68 device entries / 45 distinct parts**, all 32-pin or
36-pin:

| Bitstream | Pins | Size | Parts |
|---|---|---|---|
| `RAM3250` | 32 | 128K–512K | `628256`, `628512`, `W24020/40`, and NVRAM: `DS1249*`, `DS1250*`, `BQ4014/15*`, `M48T128/129/512/513*`, `M48Z128/129/512*` |
| `RAM3251` | 32 | 64K–128K | `61512`, `62512`, `628128`, `W24010`, `W24512`, `DS1245*`, `BQ4013*` |
| `RAM3665` | 36 | 1M–2M | `DS1265*`, `DS1270*`, `M48Z2M1*`, `KM-1M` |

Two things follow. First, **the FPGA already runs memory tests** — 15 entries are
explicit `(TEST)` variants paired with 19 `(RW)` ones, and the mode is selected
by `protocol_id` (`0x0e` = RW, `0x29` = TEST) while both load the *same*
bitstream. So a RAM-test personality is an existing, supported operation.

Second, **it is entirely static memory.** Searching the full 28,483-device
catalog for `4164`, `41256`, `4116`, `44256`, `514400`, `TMS4164` returns **zero
matches**. There is no DRAM entry and no DRAM bitstream.

## What the socket can electrically reach

**Documented.** The protocol's voltage field is a *VPP/VCC code* — both rails
positive. There is no negative-rail encoding anywhere. That single fact draws
the line at 1980, when the `4164` generation moved DRAM to a single +5 V supply.

**Reachable** (≤40 pins, single +5 V, asynchronous):

| Family | Organisation | Pins |
|---|---|---:|
| `4164` | 64K × 1 | 16 |
| `41256` | 256K × 1 | 16 |
| `4416` | 16K × 4 | 18 |
| `4464` / `41464` | 64K × 4 | 18 |
| `511000` / `421000` | 1M × 1 | 18 |
| `44256` / `514256` | 256K × 4 | 20 |
| `514400` | 1M × 4 | 20 |

FPM and EDO variants share these packages and rails. ZIP parts work through a
passive ZIP→DIP adapter. A **30-pin SIMM** (30 contacts, +5 V) also fits inside
the 40-pin socket with a passive edge adapter.

**Out of reach, permanently:**

- `4116` (+12 V / +5 V / **−5 V**) — and with it Apple II, TRS-80, early PET,
  Atari 800. The most-wanted vintage DRAM, blocked by a rail the hardware cannot
  generate. No bitstream changes this.
- `2107`, `MK4006/4008`, `4096`, `1103` — all multi-rail.
- SDRAM TSOP-54 (54 pins), 72-pin SIMM, every DIMM — pin count.
- DDR anything — pins, 1.2–1.5 V rails, and timing domain all out of range.

## What already exists for custom bitstreams

**Documented**, from [radiomanV/Xgecu_T76](https://github.com/radiomanV/Xgecu_T76)
(The Unlicense) — this is a complete worked example, not just a converter:

| Piece | What it gives |
|---|---|
| `gen_bit.py` | Anlogic TD `.bit` → T76 format |
| `t76_uploader.py` | libusb uploader |
| `clock_sniffer/t76.v` | working Verilog top level |
| `clock_sniffer/t76.adc` | **every pin constrained** (123 lines: location, IO standard, drive strength, pull) |
| `clock_sniffer/timing.sdc` | `clk_20` @ 50 ns, `clk_80` @ 12.5 ns |

That `.adc` removes the usual bring-up risk on an undocumented board.

The Verilog exposes the whole I/O surface:

- **All 48 ZIF positions are `inout`** — full bidirectional per-pin control,
  which is what multiplexed DRAM address/data needs.
- **Rail assignment is a shift register**: clock a pattern out on
  `ser_data`/`ser_clk`, latch with `vpp_le`/`vcc_le`/`gnd_le`, enable with the
  matching `_oe` lines. Per-pin VCC/GND placement, solved.
- **An 80 MHz clock** gives 12.5 ns granularity against async DRAM timings of
  60–150 ns — roughly 5–10× the resolution required.

**Measured:** this project's own bitstream upload path (`BEGIN_BS` → `BLOCK` →
`END_BS`) is hardware-verified against the attached T76.

## MS1 — the DRAM controller

Write the RAS/CAS/refresh state machine and march/checkerboard patterns, run the
test entirely inside the FPGA, and signal pass/fail on an ISP pin (the example
already drives `j_19` as an output). Observe with a logic analyser.

This validates the electrical work — pin mapping, rail switching, DRAM timing —
**with zero host-protocol RE**, and it is useful on its own even if MS2 never
completes. Async DRAM is a modest design: a state machine, a refresh counter,
an address multiplexer.

## MS2 — getting results back to the host

### The link is HSPI, and it is documented

**Measured.** Cross-referencing the T76's FPGA pinout against the CH569 pin
table identifies all 33 CPU-facing FPGA pins:

| Signal | Role |
|---|---|
| `HD0`–`HD22`, `HD31` | HSPI data bus |
| `HTCLK` / `HRCLK` | transmit / receive clock |
| `HTREQ` / `HTACK` | transmit request / acknowledge |
| `HTVLD` / `HRVLD` | transmit / receive valid |
| `HTRDY` / `HRACT` | ready / receive active |

This is the CH569's **High-Speed Parallel Interface**, specified in **Chapter 10**
of `docs/hardware/CH569DS1.PDF`: configurable 8/16/32-bit data, built-in FIFO,
DMA, dual-buffer transceiver, up to 3.8 Gbps (32-bit @ 120 MHz).

So MS2 is **not** blind RE of 33 unknown wires. The link layer is specified; only
the payload is unknown. The wiring shows at least a 16-bit bus; the exact mode
and clock rate are **inferred** and should be confirmed empirically.

### Shape of the effort

| Phase | Work | Risk |
|---|---|---|
| **A** | Implement the FPGA side of HSPI; stream captured traffic out an ISP pin to a logic analyser | Low — spec-driven |
| **B** | Known-plaintext capture and diffing | **The open one** |
| **C** | Impersonate a memory device: answer a normal `0x0d` read with a synthetic counter | Moderate |
| **D** | Present DRAM results as a readable image | Low |

Phase B is unusually favourable, because this project already owns the host
side. Every experiment is a one-variable diff against a known answer:

- read 64 B at `0x0000` vs `0x1000` → isolates **address** encoding
- read 64 B vs 1024 B → isolates **length** encoding
- `READID` (`0x05`) vs `READ_CODE` (`0x0d`) → isolates **opcode**
- read a chip of known contents → identifies the **data** path

**Phase C is the success criterion, and it is binary**: if `minipro read` returns
the counter pattern, results can reach the host through the standard path. No
partial credit. That matters here — this project's write path once passed every
golden test while programming nothing, and only a chip could tell.

### The question that could kill it: is HSPI traffic encrypted?

The CH569 has an **ECDC** module (Chapter 15): AES and SM4, ECB and CTR, with
"hardware automatic encryption/decryption modes for data transfer between some
peripheral interfaces and SRAM" — HSPI among them. If XGecu enabled it, Phase B
captures ciphertext and MS2 stops.

It is plausible on its face: the MCU firmware image in `updateT76.dat` is already
encrypted (~7.9 bits/byte entropy, never decrypted).

**Current evidence points to plaintext, but this is not confirmed:**

1. **Measured.** Decompressing three vendor bitstreams (`T7_ROM28P32`,
   `T7_RAM3250`, `T7_SPI25F11`, 700–775 KB each) and searching for the AES
   S-box, the SM4 S-box and the AES round constants — in normal and bit-reversed
   byte order — finds **none of them**. If the FPGA had to decrypt HSPI it would
   need one of those tables. *Weak evidence only*: bitstreams pack LUT and BRAM
   contents in device-specific bit orders, so a table need not appear
   byte-aligned. Presence would have been strong evidence; absence is not.
2. **Inferred.** Encrypting this bus would protect nothing. The USB protocol is
   already fully plaintext and reverse-engineered — this project reimplements it
   — so the same commands, addresses and chip data are visible at a layer that is
   far easier to observe than a BGA-to-BGA link.
3. **Inferred.** The key would have to live in bitstreams that ship on disk and
   decompress freely, which is not a defensible place for one.
4. **Measured.** ECDC is optional and must be configured; it is not implicit in
   HSPI operation.

**How to confirm it, cheaply and first.** In Phase A, before building anything
elaborate, capture traffic during a plain `minipro info` — a 5-byte request with
a known 64-byte reply containing known ASCII (the manufacture date). If those
bytes appear, it is plaintext. If the payload is high-entropy noise that does
not correlate, ECDC is on and MS2 ends at out-of-band results.

That check costs a fraction of the total effort and decides whether the rest is
worth starting. **Do not defer it.**

## Verdict

MS1 is worth doing on its own terms and de-risks the electrical unknowns. MS2 is
better-shaped than it first looked — a documented link layer plus a
known-plaintext payload problem — with one unresolved gate that can be settled
early and cheaply.

Even complete, the result covers the 64 Kbit-and-later single-rail DIP DRAMs and
30-pin SIMMs: most 1980s home computers from the C64 era onward, but not the
three-rail 1970s parts. An off-the-shelf Retro Chip Tester covers all of
that today, including the `4116`, plus SRAM, PROMs and cartridge dumping. So this is a
project worth doing to learn what the FPGA can be made to do — not a shortcut to
a DRAM tester.
