# TL866A / TL866CS — the last unimplemented driver, researched

*As of 2026-08-21. Protocol facts extracted from nmatt0's C fork
(`src/tl866a.c`, 1,613 lines, one driver serving both models), per the
project's wire-facts rule: everything below is transcribed observation, and a
Rust driver built from it would use these facts with its own expression.
Companion to [`design-decisions.md`](design-decisions.md) §10 and the
capability matrix in the [`minipro-rs` README](../minipro-rs/README.md).*

## Why it is the outlier

Every other supported programmer (TL866II+, T48, T56, T76) shares one protocol
family: a 64-byte BEGIN carrying the chip context once, bare-opcode commands
afterwards, 32-bit addresses, and bulk data endpoints. The TL866A/CS predates
all of that — different silicon (a Microchip PIC18F87J50-class MCU, hence the
**Microchip VID: `04d8:e11c`**), different message model, different opcode
space. Nothing from `wire.rs` transfers; this driver shares only the
`Transport` trait.

## Transport

Plainer than the family's: **every transfer rides EP01 OUT / EP81 IN.** There
are no bulk-data endpoints, no interlacing, no payload pipes — block data is
inlined into the command messages themselves. Our existing `Transport` trait
covers it with nothing new; the TL866II+'s `recv_interlaced` machinery is
irrelevant here.

Message sizes are per-command and irregular (4, 5, 8, 10, 15, 17, 18, 48, 64,
7+data bytes) rather than the family's fixed shapes.

## The message model: context on every packet

The family establishes chip context once in BEGIN_TRANS. The A repeats it in
**every command header**:

```
[0] opcode      [1] protocol_id      [2] variant (low byte)
```

(A few commands skip the context — calibration, autodetect, TSOP48 unlock —
and zero those bytes instead.)

This is the deepest structural difference for a Rust driver: the family
drivers derive per-op packets from the opcode alone; an A driver threads the
device through every builder.

## Addresses are 24-bit

Code size in BEGIN, block addresses in read/write, fuse addressing, and the
verify-failure address in status replies are all **3-byte little-endian**.
16 MB is the architectural ceiling — consistent with the part classes the A
supports.

## Packet layouts (transcribed facts)

**BEGIN_TRANS (0x03), 48 bytes sent** — its own layout, not the family's:

| Offset | Size | Field |
|---|---|---|
| 3 | 2 | data-memory size, LE16 |
| 5 | 1 | VPP nibble << 4 |
| 6 | 2 | page size, LE16 |
| 8 | 1 | (VDD nibble << 4) \| VCC nibble |
| 9 | 2 | pulse delay, LE16 |
| 11 | 1 | ICSP byte |
| 12 | 3 | code size, **LE24** |

**END_TRANS (0x04)**: 4 bytes. **Status (0xFE)**: send 5, receive 64 —
`[0]` verify-error flag, `[2..4]`/`[4..6]` the compared words, `[6..9]` the
failing address (LE24), `[9]` overcurrent. The A reports **verify-while-write
errors through status**, a feature the family dropped.

**Read block (0x21/0x30/0x15)**: send 18 — length LE16 at 2, address LE24 at
4 — then the data arrives on EP81 itself. **Write block (0x20/0x31/0x14)**:
one message, 7-byte header + data inline.

**Fuses (0x10–0x15, 0x40/0x41)**: send 18 / receive 64, items count at `[2]`,
code size LE24 at `[4]`, data from reply offset 7. The C preserves a firmware
quirk on the write side — the address field is `code_memory_size - 0x38`, its
comment flagging the offset as a suspected firmware bug. A Rust driver must
reproduce the quirk (it is wire behavior), with its own explanation.

**Chip ID (0x05)**: send 8, receive 32, same id-type/endianness decode rule as
the family. **SPI autodetect (0xFC)**: send 10 (type at `[7]`), receive 16, id
= BE24 at `[2]`. **Erase (0x22)**: send 15 (fuse count at `[2]`), drain 64.
**Protect (0x44/0x45)**: send 10, no reply. **JEDEC rows**: reuse the
read/write-code opcodes with row/flags at `[4]`/`[5]` and bit-packed data.

**TSOP48 unlock (0xFD)**: an 8-byte random nonce with a CRC-16 woven into it
byte-shuffled — a little challenge dance unique to this hardware.

## System info

The same 5-zero probe as everyone (our `detect()` would work unchanged once
the transport opens `04d8:e11c`), but its own reply layout: model byte at
`[6]` (**1 = TL866A, 2 = TL866CS**), status at `[1]` (not the fw==0 rule),
device code `[7..15]`, serial `[15..39]`, hardware revision `[39]`, no
voltage. Firmware version is the common `[4..6]` LE16; the C pins its expected
version at `0x0256`.

## The subsystems that dominate the file

- **Pin-driver / latch model.** The A has no digital voltage control: VPP/VCC/
  GND routing is eight hardware latches loaded through `SET_LATCH` (0xD1),
  with per-pin driver tables, plus raw ZIF primitives (`SET_DIR`/`SET_OUT`/
  `READ_ZIF_PINS`). This is not an optional extra on the A: **the logic-IC
  test is host-driven bit-bang over those primitives**, not a firmware vector
  loop like the family's 0x28. Skipping the bit-bang subsystem (as we do on
  every driver) therefore costs the A its logic test too.
- **Firmware update.** Model-specific XOR-scrambled containers (separate
  256+1024-byte tables for A and CS), ~600 lines with the tables. Same
  deferral class as the other non-T76 updates.

## The database dimension (found after the driver research)

The vendor catalog is **three databases, selected by programmer model**: the
generated `infoic.xml` carries `INFOIC` (TL866A/CS), `INFOIC2PLUS`
(TL866II+/T48/T56), and `INFOICT76` (T76) sections, and the C picks the
section — and a per-generation **voltage table** (`tl866a_vcc_voltages` etc.)
— from the attached model. The A's BEGIN carries voltage *nibbles* resolved
through its own tables.

Two consequences. For the A: a driver alone is not enough — it needs the
`INFOIC` catalog section and the A's voltage-nibble mapping, which raises the
real effort from M toward L. And a finding that matters **today**: our own
database layer reads only the T76 catalog, so chips selected for the
T48/T56/TL866II+ drivers are currently served T76-section parameters. The
families are close, but the C deliberately keeps separate sections — per-model
catalog selection is work worth doing for the drivers we already ship,
regardless of whether the A ever lands.

## What a Rust driver would look like

- **Fits the existing traits cleanly**: `Programmer` + Memory/Fuse/Jedec/
  Protect/Autodetect capability impls, EP01/EP81 only. No transport work.
- **Shares nothing with `wire.rs`** — new opcode constants, new builders, the
  3-byte context header threaded through, LE24 helpers. All-new golden tests
  (the C is the only oracle; there is no capture corpus for this hardware).
- **Caps**: MEMORY | FUSES | JEDEC | PROTECT | AUTODETECT. No LOGIC (bit-bang
  only), no CALIBRATION at first (op exists, trivial to add), no firmware
  update.
- **Effort: M** (roughly the TL866II+ effort again, without the interlacing
  but with nothing reusable from the shared wire layer).
- **Reference-only forever-ish**: no TL866A or CS exists on this bench, and
  unlike the II+ the hardware is discontinued — silicon validation depends
  entirely on a contributor with one in a drawer.

## Design, if it is ever built

Every element below is a gathered C fact plus our own expression — the same
recipe as the TL866II+. Facts cited inline; anything not a fact is marked as
our policy.

**Placement and shape.** One driver, `minipro-proto/src/tl866a.rs`, serving
both models (they differ only in the system-info model byte and the
firmware-update tables, which are out of scope). Structured like `tl866ii.rs`:
one struct over `Box<dyn Transport>`, capability impl blocks, a `SharedTx`
golden-test harness. Nothing borrowed from `wire.rs` — the opcode space and
every layout are different, and pretending otherwise is how accidental
divergence starts.

**Transport.** Zero new surface. Everything rides EP01/EP81 through the
existing `send`/`recv`; the interlace machinery is irrelevant. Erase drains
its reply through `command_slow` (our policy: the reply arrives when the erase
finishes, and the C's blanket six-minute timeout is the thing we deliberately
do not copy). New id `(0x04d8, 0xe11c)` appended to `KNOWN_IDS`, and the
config-cycle re-arm stays T76-gated as it already is.

**The context header, as a builder not a convention.** Fact: `msg_init` puts
`opcode, protocol_id, variant-low` in bytes 0..3 — and then most ops
*overwrite* byte 2 (size LE16 in block ops, item count in fuses and erase), so
the variant actually reaches the wire only in BEGIN and the short commands.
Encoding that truthfully means two private builders: `ctx_msg(dev, op)` for
commands that keep the header, and plain `op_msg(op)` for the three that zero
it (calibration, autodetect 0xFC, TSOP48 unlock). The per-op field writes then
land wherever the C lands them, byte-2 clobbers included — pinned by tests so
nobody "fixes" the clobber into a divergence. A local `le24` helper for the
24-bit addresses; it stays in this file.

**Programmer core.**
- `begin`: the 48-byte layout from §Packet layouts, followed by the 0xFE
  status read with overcurrent at byte 9 — same shape our other drivers use
  for their OVC gate. The voltage fields are *nibbles from the INFOIC
  section's tables*; this driver refuses to run from a T76-section catalog
  (preflight-style guard), because feeding T76 nibble encodings to the A's
  latches is exactly the class of wrong our history warns about.
- `end`: the 4-byte END. `identify`: send 8 / receive 32, then the family's
  id-type endianness rule — the decode logic our cleared drivers already
  express.
- `query_info`: the shared 5-zero probe; model from byte 6 (1 = A, 2 = CS),
  status from byte 1 (`2` = bootloader — the A predates the fw==0 rule),
  device code 7..15, serial 15..39, hardware revision 39 as the version's
  leading field, no voltage.

**MemoryOps.** Read: 18-byte command (size LE16 @2, address LE24 @4), data on
EP81 itself — `block_size` = `read_buffer_size` (fact: the C chunks reads by
it). Write: one inline message, 7-byte header + payload. After **each** write
block the C polls status and reports a verify-while-write failure with the
failing address (LE24 @6) — we map that to `Error::Verify { addr }`, which
means the A gets mid-write verification the newer drivers cannot offer.
Blank check reuses `Region::blocks` like everyone.

**Fuses / JEDEC / protect / autodetect.** Straight transcriptions: the fuse
kind→opcode map with data at reply offset 7 and the `code_size − 0x38`
write-side quirk reproduced (it is wire behavior; our comment explains it in
our own words). JEDEC rows reuse the code opcodes with row/flags @4/5 and
bit-packed data — read replies carry the bits at offset **0**, headerless,
which a test pins because it is the kind of asymmetry that gets "corrected"
by accident. Autodetect: no context, type @7, id BE24 @2 from a 16-byte
reply; the idle-bus refusal from `no_device_response` applies unchanged.

**Caps and refusals.** `MEMORY | FUSES | JEDEC | PROTECT | AUTODETECT`, plus
`CALIBRATION` (the op is ten lines and headerless — cheap to carry). No
`LOGIC`: on this hardware the logic test is host-driven bit-bang, and the
driver refuses with exactly that reason rather than advertising a capability
the deferred subsystem owns. No firmware update; a bootloader-stuck A is
still *visible* through the status byte in `info`.

**Tests.** Golden fixtures for: the 48-byte BEGIN (nibble packing), block
read/write (including the byte-2 clobber), the fuse quirk, headerless JEDEC
reads, BE24 autodetect, the 0xFE status decode, and both info layouts (A and
CS bytes). Plus the provenance sweep run against `tl866a.c` before first
commit — the TL866II+ order of operations, kept.

**Prerequisite, restated.** Per-model catalog selection (§The database
dimension) ships first. Without the INFOIC section the driver would be
correct wire grammar speaking the wrong vocabulary.

## Honest recommendation

Implementable any time under the wire-facts rule, but it is the lowest-value
driver in the queue: discontinued hardware, zero silicon access, a logic test
that can't come along, and no shared code to amortize. The research here is
the expensive part and is now done; building it is a well-scoped M whenever a
motivated TL866A owner appears — which is exactly the moment it should be
built, so first hardware contact has someone to report to.
