# minipro-rs

A Rust redesign of the [minipro](https://gitlab.com/nmatt0/minipro/-/tree/t76-improvements) CLI, built from the design in
[`docs/rust-redesign.md`](../docs/rust-redesign.md),
[`docs/rust-trait-model.md`](../docs/rust-trait-model.md), and the three-mode
output design.

**Status:** working. The T76 driver is fully implemented and **hardware-verified**
(byte-identical reads from a real T76). The native chip database reads XGecu's
`InfoICT76.dll` directly (no XML needed) and can provision itself from a mirror.
164 tests pass, clippy clean.

> ⚠️ **The T48 and T56 drivers are untested on hardware.** They are faithful
> reference-only ports — verified against the C source and byte-exact golden
> packets, but **never run against a real T48 or T56** (we don't have the
> hardware). Treat them as unproven until someone confirms them on-device. See
> [Contributing](#contributing).

```
cargo test           # 164 hardware-free protocol/golden/db tests
cargo clippy --all-targets
./target/debug/minipro --json info
```

## Driver capability matrix

Three programmer drivers sit behind one `Programmer` trait; `detect()` dispatches
on the system-info device-type byte (`MP_T56=6`, `MP_T48=7`, `MP_T76=8`).

Legend: ✅ implemented ・ ⬜ deferred (see below) ・ ➖ N/A for that hardware

| Capability | T76 | T56 | T48 |
|---|:--:|:--:|:--:|
| Memory (read / write / erase / blank / identify) | ✅ | ✅ | ✅ |
| Fuses (user / config / lock) | ✅ | ✅ | ✅ |
| JEDEC rows (PLD/GAL) | ✅ | ✅ | ✅ |
| Protect on / off | ✅ | ✅ | ✅ |
| Calibration readout | ✅ | ✅ | ➖ |
| Logic-IC test | ✅ | ✅ | ✅ |
| SPI 25-series autodetect | ✅ | ✅ | ✅ |
| Firmware update | ✅ | ⬜ | ⬜ |
| eMMC / NAND | ✅ | ➖ | ➖ |
| Pin-contact test | ✅ | ➖ | ➖ |
| Pin-driver / bit-bang | ➖ | ➖ | ⬜ |

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
  T76 port is the template); the T48 pin-driver/bit-bang/self-test cluster is
  the shared `bitbang.c` subsystem with no standalone caller in the port yet.

**Not started:** the TL866II+ and TL866A/CS drivers.

## Workspace

| Crate | Role |
|---|---|
| `minipro-core` | Device model, `Transport`/`Programmer` + capability traits, `Reporter`, typed `Error`, op orchestration. Object-safe capability upcasts; the must-drain `Pending` guard. |
| `minipro-proto` | Per-programmer drivers (`t76.rs`, `t56.rs`, `t48.rs`), the shared `wire.rs` (II+-class header + shared ops), and `detect()`. |
| `minipro-usb` | `UsbTransport` (over `nusb`) + `MockTransport` for hardware-free tests. |
| `minipro-db` | `ChipDb` trait with three backends: `XmlDb` (infoic.xml), `DllDb` (parses `InfoICT76.dll` directly), and `HttpDb` (`net` feature — provisions the DLL from a mirror into RAM, persists only the derived catalog + `.alg` bitstreams, daily version check). |
| `minipro-cli` | The `minipro` binary: human / JSON / TUI reporters, mode selection. |

## Design in three lines

- **Transport → Programmer(+capabilities) → Reporter**, all `dyn`-dispatched.
- The must-drain `Pending` guard makes "forgot to read the response" (which wedges
  the T76) a compile-time warning.
- One event stream, three renderers; `#[derive(Serialize)]` on the outcome *is*
  the JSON mode.

## How the drivers are verified without hardware

The T76 was validated against a real device. The T56/T48 have no hardware here,
so correctness rests on a **golden-packet** discipline: the shared BEGIN header
and every driver's packet builders are frozen as byte-exact fixtures derived
from the C source, so any change to a byte the device would see fails a test.
The `wire.rs` extraction that all three drivers share was proven byte-identical
against the hardware-verified T76 goldens.

## Remaining work

1. **TL866II+ driver** — II+-class, close to the T48 shape (EP02 bulk,
   fixed-silicon).
2. **TL866A/CS driver** — the outlier: 24-bit addressing, alternate opcode space,
   latch-based ZIF model, no digital voltage control.
3. **Firmware update** (per-device, obfuscation-table transcription).
4. **Remaining DB fidelity**: GAL/PLD `config` (fuse-map geometry) and the
   host-side `pin_map` package tables. (The `chip_type`/`blank_value` fields,
   the `logicic.xml` vector parser, and the `catalog.postcard` schema-version
   header are done. A compiled/baked-in DB was considered and dropped —
   direct-to-source via `DllDb`/`HttpDb` is better.)

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

- **🔌 Validate the T48 / T56 on real devices.** These drivers are complete but
  **untested on hardware** — ported faithfully from the C source and covered by
  byte-exact golden packets, yet never run against an actual T48 or T56. If you
  own one, running `minipro info`, `detect`, and a `read` against a known chip
  (and reporting what you see) is the single most valuable thing you can do. It
  turns "reference-only" into "proven." The write-buffer-size `TODO(hw)` in
  `t48.rs`/`t56.rs` can only be settled this way too.
- **🔌 T76 op coverage on hardware.** T76 *reads* are byte-verified; **write,
  erase, NAND, eMMC, and firmware-update** are ported but not yet exercised on a
  device. Confirmations (or bug reports) welcome.
- **Port the TL866II+ and TL866A/CS drivers.** The TL866II+ is II+-class and
  close to the existing T48 (EP02 bulk, fixed-silicon), so it reuses most of the
  shared `wire` layer. The TL866A/CS is the outlier (24-bit addressing, an
  alternate opcode space, a latch-based ZIF model). Both are well-specified in
  the C fork.
- **The roadmap** ([`docs/rust-port-roadmap.md`](../docs/rust-port-roadmap.md))
  lists the rest — CLI verbs for fuses/JEDEC/firmware, GAL/PLD `config`, extra
  memory regions — each scoped with effort and dependencies.

Every driver is checked against the C reference and a golden-packet harness, so
new work has a clear correctness bar. If you validate on hardware, please include
the programmer model, firmware version (`minipro info`), and the exact command +
output. Bug reports with a `--json` line and the chip name are ideal.
