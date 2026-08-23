# minipro-rs

A Rust redesign and reimplementation of the [minipro](https://gitlab.com/nmatt0/minipro/-/tree/t76-improvements) CLI, built from the design in
[`docs/rust-redesign.md`](../docs/rust-redesign.md),
[`docs/rust-trait-model.md`](../docs/rust-trait-model.md), and the three-mode
output design.

**Status:** working. The T76 driver is fully implemented and **hardware-verified**
across the read/write/erase core: byte-identical reads on four parts, a write
proven bit-exact on an MX27C2000, and full write→verify→erase→blank cycles run
three times each on two electrically-erasable Winbond parts (W27C512 64 KB,
W27C257 32 KB) with distinct data every cycle. The native chip database reads
XGecu's `InfoICT76.dll` directly (no XML needed) and can provision itself from
a mirror. 219 tests pass, clippy clean.

> ⚠️ **The T48, T56, and TL866II+ drivers are untested on hardware.** They are
> reference-only implementations — verified against byte-exact golden
> packets, but **never run against real silicon** (we don't have the
> hardware). Treat them as unproven until someone confirms them on-device. See
> [Contributing](#contributing).

```
cargo test           # 219 hardware-free protocol/golden/db tests
cargo clippy --all-targets
./target/debug/minipro --json info
```

## Driver capability matrix

Four programmer drivers sit behind one `Programmer` trait; `detect()` dispatches
on the system-info device-type byte (`MP_TL866II+=5`, `MP_T56=6`, `MP_T48=7`,
`MP_T76=8`).

Legend: ✅ implemented ・ ⬜ deferred (see below) ・ ➖ N/A for that hardware

| Capability | T76 | T56 | T48 | TL866II+ |
|---|:--:|:--:|:--:|:--:|
| Memory (read / write / erase / blank / identify) | ✅ | ✅ | ✅ | ✅ |
| Fuses (user / config / lock) | ✅ | ✅ | ✅ | ✅ |
| JEDEC rows (PLD/GAL) | ✅ | ✅ | ✅ | ✅ |
| Protect on / off | ✅ | ✅ | ✅ | ✅ |
| Calibration readout | ✅ | ✅ | ➖ | ⬜ |
| Logic-IC test | ✅ | ✅ | ✅ | ✅ |
| SPI 25-series autodetect | ✅ | ✅ | ✅ | ✅ |
| Firmware update | ✅ | ⬜ | ⬜ | ⬜ |
| eMMC / NAND | ✅ | ➖ | ➖ | ➖ |
| Pin-contact test | ❌ | ➖ | ➖ | ➖ |
| Pin-driver / bit-bang | ➖ | ➖ | ⬜ | ⬜ |

The T76 pin-contact test is ❌ **removed**, not deferred: on real hardware the
reply is a constant (mostly stale reply-buffer bytes) and issuing the command
corrupts the read that follows, so the driver no longer advertises it. The
TL866II+ is the family's odd one out on the wire — bulk transfers larger than
64 bytes interlace across *two* endpoint pairs, small writes fold into the
command pipe — and is **reference-only** (never run against real silicon).

The fuse/JEDEC/protect/calibration commands are byte-identical across the lineage
and run on EP01/EP81 with no bitstream, so they are implemented once in
`wire.rs` and every driver delegates. Logic test and autodetect share the same
vector/probe loop in `wire.rs`; on the FPGA drivers they fetch their utility
bitstreams (`TestLgcPull`, `TTL1`, `SPI25F*`) by name from `algoT76/`, per pass.

Every non-✅ cell is principled, not an oversight:

- **➖** is hardware-appropriate: eMMC/NAND/pin-test are T76-only silicon; the
  pin-driver subsystem is a T48/TL866 feature; the T48 has no calibration op.
- **⬜** is deferred with reason: firmware update is per-device obfuscation-table
  transcription, unverifiable without hardware and a real `updateT*.dat` (the
  T76 implementation is the template); the T48 pin-driver/bit-bang/self-test
  cluster is the shared `bitbang.c` subsystem with no standalone caller yet.

**Not started:** the TL866A/CS driver (the older, different protocol).

## Workspace

| Crate | Role |
|---|---|
| `minipro-core` | Device model, `Transport`/`Programmer` + capability traits, `Reporter`, typed `Error`, op orchestration. Object-safe capability upcasts; the must-drain `Pending` guard. |
| `minipro-proto` | Per-programmer drivers (`t76.rs`, `t56.rs`, `t48.rs`, `tl866ii.rs`), the shared `wire.rs` (II+-class header + shared ops), and `detect()`. |
| `minipro-usb` | `UsbTransport` (over `nusb`) + `MockTransport` for hardware-free tests. |
| `minipro-db` | `ChipDb` trait with four backends: `XmlDb` (infoic.xml), `DllDb` (parses `InfoICT76.dll` directly), `HttpDb` (`net` feature — provisions the vendor files from a mirror of extracted files, daily version check), and `vendor` (the zero-setup default: downloads XGecu's installer archive once and unpacks the database with a system RAR tool — `bsdtar`/`unar`/`unrar` — so no RAR decoder is linked in). |
| `minipro-cli` | The `minipro` binary: human / JSON / TUI reporters, mode selection. |

## Design in three lines

- **Transport → Programmer(+capabilities) → Reporter**, all `dyn`-dispatched.
- The must-drain `Pending` guard makes "forgot to read the response" (which wedges
  the T76) a compile-time warning.
- One event stream, three renderers; `#[derive(Serialize)]` on the outcome *is*
  the JSON mode.

## How the drivers are verified without hardware

The T76 was validated against a real device. The T56/T48/TL866II+ have no
hardware here, so correctness rests on a **golden-packet** discipline: the
shared BEGIN header and every driver's packet builders are frozen as byte-exact
fixtures, so any change to a byte the device would see fails a test.
The `wire.rs` extraction that all four drivers share was proven byte-identical
against the hardware-verified T76 goldens.

A caution that discipline earned the hard way: the T76 write path passed every
golden test while programming nothing — the packets were plausible, the cadence
was wrong, and only a chip could tell. Golden packets pin the wire; silicon
proves the operation.

## Remaining work

1. **TL866A/CS driver** — the outlier: 24-bit addressing, alternate opcode space,
   latch-based ZIF model, no digital voltage control.
2. **Firmware update for the T56/T48/TL866II+** (per-device, obfuscation-table
   transcription; the T76's is done, exposed as `minipro update`, and gated on
   verified bootloader entry both ways).
3. **Remaining DB fidelity**: GAL/PLD `config` (fuse-map geometry), the
   host-side `pin_map` package tables, and fuse item widths for 16-bit parts
   (`DATA_BUS_WIDTH` — memory transfers are already byte-correct; the flag
   matters only for fuses and display). A compiled/baked-in DB was considered
   and dropped — direct-to-source via `DllDb`/`HttpDb` is better.

The FPGA logic test and SPI autodetect are **done** on T56/T76 — the driver ops
fetch their utility bitstreams (`TestLgcPull`, `TTL1`, `SPI25F*`) by name from
the same `algoT76/` source as chip bitstreams, and the CLI exposes them as
`minipro logic <chip>` and `minipro autodetect [--wide]`.

## Non-goals

- **The host-side bit-bang / pin-driver subsystem** (`prom.c` + `bitbang.c` +
  the fixed-silicon drivers' `set_zif_*`/`set_pin_drivers`/`set_voltages`
  primitives). Its only real payoff is reading vintage parallel PROMs (the
  `CP_PROM` path, read-only), and the FPGA programmers (T56/T76) already cover
  those natively by uploading a ROM-class algorithm (`ROM24P`/`ROM28P`/…) and
  clocking the parallel bus through the FPGA. On fixed-silicon (TL866/T48) the
  bit-bang path just re-does that in software, pin-by-pin over USB — ~1200
  lines of hardware-unverifiable pin-level code for a fixed-silicon-only niche.
  A parallel PROM on a T56/T76 is simply another ROM-class algorithm upload +
  `read_block` through the existing path.
- **A compiled/baked-in chip database.** Direct-to-source (`DllDb` parses the
  vendor DLL; `HttpDb` provisions + caches it from a mirror) keeps the catalog
  live and is strictly better than a frozen, rebuild-to-update blob.

## Contributing

Help wanted — especially from anyone with the **hardware we don't have**.

- **🔌 Validate the T48 / T56 / TL866II+ on real devices.** These drivers are
  complete but **untested on hardware** — implemented from the wire protocol and
  covered by byte-exact golden packets, yet never run against real silicon. If
  you own one, running `minipro info`, `detect`, and a `read` against a known
  chip (and reporting what you see) is the single most valuable thing you can
  do. It turns "reference-only" into "proven." The TL866II+'s interlaced
  dual-endpoint bulk path and the write-buffer-size `TODO(hw)` in
  `t48.rs`/`t56.rs`/`tl866ii.rs` can only be settled this way.
- **🔌 T76 op coverage on hardware.** Reads, writes, and erase are verified on
  parallel EPROM/EEPROM parts; **flash-class writes (the protect-off
  sequence), NAND, eMMC, and firmware-update** are implemented but not yet
  exercised on a device. One salvaged 25-series SPI flash covers the protect
  path and flash write chunking in a recoverable loop.
- **Implement the TL866A/CS driver.** The outlier of the family: 24-bit
  addressing, an alternate opcode space, a latch-based ZIF model. Well-specified
  in the C fork.
- **The roadmap** ([`docs/rust-roadmap.md`](../docs/rust-roadmap.md))
  lists the rest — CLI verbs for fuses/JEDEC/firmware, GAL/PLD `config`, extra
  memory regions — each scoped with effort and dependencies.

Every driver is checked against a golden-packet harness, so new work has a clear
correctness bar. If you validate on hardware, please include the programmer
model, firmware version (`minipro info`), and the exact command + output. Bug
reports with a `--json` line and the chip name are ideal.

## License

MIT — see [`LICENSE`](../LICENSE). The MIT grant covers this project's own
source only. The protocol facts these drivers implement were reverse-engineered
by the minipro community (nmatt0 / Matt Brown); that credit and the status of
XGecu's proprietary DLL/algorithm data are recorded in the repo-root
[`NOTICE`](../NOTICE).
