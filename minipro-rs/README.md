# minipro-rs

A Rust redesign of the [minipro](../minipro-t76) CLI, built from the design in
[`docs/rust-redesign.md`](../docs/rust-redesign.md),
[`docs/rust-trait-model.md`](../docs/rust-trait-model.md), and the three-mode
output design.

**Status:** working. The T76 driver is fully implemented and **hardware-verified**
(byte-identical reads from a real T76). The T56 and T48 drivers are reference-only
ports, verified against golden packets derived from the C source rather than
hardware. The native chip database reads XGecu's `InfoICT76.dll` directly (no XML
needed) and can provision itself from a mirror. 141 tests pass, clippy clean.

```
cargo test           # 141 hardware-free protocol/golden/db tests
cargo clippy --all-targets
./target/debug/minipro --json info
```

## Driver capability matrix

Three programmer drivers sit behind one `Programmer` trait; `detect()` dispatches
on the system-info device-type byte (`MP_T56=6`, `MP_T48=7`, `MP_T76=8`).

Legend: ✅ implemented ・ 🔷 blocked on FPGA utility-bitstream plumbing ・
⬜ deferred (see below) ・ ➖ N/A for that hardware

| Capability | T76 | T56 | T48 |
|---|:--:|:--:|:--:|
| Memory (read / write / erase / blank / identify) | ✅ | ✅ | ✅ |
| Fuses (user / config / lock) | ✅ | ✅ | ✅ |
| JEDEC rows (PLD/GAL) | ✅ | ✅ | ✅ |
| Protect on / off | ✅ | ✅ | ✅ |
| Calibration readout | ✅ | ✅ | ➖ |
| Logic-IC test | 🔷 | 🔷 | ✅ |
| SPI 25-series autodetect | 🔷 | 🔷 | ✅ |
| Firmware update | ✅ | ⬜ | ⬜ |
| eMMC / NAND | ✅ | ➖ | ➖ |
| Pin-contact test | ✅ | ➖ | ➖ |
| Pin-driver / bit-bang | ➖ | ➖ | ⬜ |

The fuse/JEDEC/protect/calibration commands are byte-identical across the lineage
and run on EP01/EP81 with no bitstream, so they are implemented once in
`wire.rs` and every driver delegates.

Every non-✅ cell is principled, not an oversight:

- **➖** is hardware-appropriate: eMMC/NAND/pin-test are T76-only silicon; the
  pin-driver subsystem is a T48/TL866 feature; the T48 has no calibration op.
- **🔷** is one well-defined unit: logic test and SPI autodetect on the FPGA
  drivers (T56/T76) need a utility-algorithm bitstream uploaded first
  (`TTL1/2`, `TestLgcPull/Down`, `SPI25F*`). The vector/probe wire protocol is
  already shared; what's missing is DB plumbing to fetch utility algorithms by
  name. That single task unblocks both ops on both drivers.
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

1. **FPGA utility-bitstream plumbing** — unblocks logic test + SPI autodetect on
   T56/T76 (the 🔷 cells).
2. **TL866II+ driver** — II+-class, close to the T48 shape (EP02 bulk,
   fixed-silicon).
3. **TL866A/CS driver** — the outlier: 24-bit addressing, alternate opcode space,
   latch-based ZIF model, no digital voltage control.
4. **T48 pin-driver / bit-bang subsystem** + firmware update (per-device).
5. A magic + schema-version header on the persisted `catalog.postcard` so cache
   invalidation is robust across `Device` struct changes.
