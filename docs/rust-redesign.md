# minipro in Rust — a redesign sketch

*A design proposal, 2026-08-12. Grounded in the [C codebase
report](minipro-codebase-report.md) and the real failure modes we hit this
session (macOS SuperSpeed transport, device-wedging on undrained responses,
firmware/bitstream coupling, dump verification).*

## What to keep vs. change

**Keep** (the C design is sound where it counts):
- The **per-programmer dispatch** (C's function-pointer vtable) — becomes a Rust
  trait. It's the right abstraction.
- The **lib/bin split** (`libminipro` + `minipro`) — becomes a Cargo workspace.
- **Data-file compatibility** with `algorithm.xml` / `infoic.xml` / `logicic.xml`
  so existing DB-generation tooling still applies.

**Change** (where C hurts):
- Manual `msg[512]` buffers + `format_int`, manual `free(bitstream)` on every
  error path, 0/1 int returns, ~zero tests, and a transport that can't express
  "this response must be drained or the device wedges."

## Workspace layout

```
minipro/                         (cargo workspace)
├─ minipro-core     (lib)  device model, Programmer + Transport traits, errors
├─ minipro-proto    (lib)  packet types, per-programmer drivers (tl866, t48/56, t76)
├─ minipro-db       (lib)  chip DB + algorithm/bitstream loading (serde)
├─ minipro-usb      (lib)  Transport impl over nusb (pure-Rust USB)
├─ minipro-cli      (bin)  clap CLI, progress, file formats
└─ minipro-ffi      (cdylib, optional)  C ABI shim for existing libminipro users
```

**Dependency graph is 100% pure Rust** — no vendored/compiled C, and crucially
none of the current tool's C libs: `nusb` replaces libusb, and `flate2`'s
`miniz_oxide` backend replaces zlib. This **directly fixes the
pkg-config/libusb/zlib build pain** we hit on macOS.

The only C left is the OS's own interface (unavoidable in any language, since the
syscall/driver boundary is C): Linux reaches usbfs via raw syscalls (→ a truly
static **musl** binary, zero dynamic deps); macOS links `IOKit.framework` and
Windows the WinUSB system DLLs — so those targets produce a *self-contained*
binary that still dynamically links system frameworks (normal and unavoidable),
not a fully static one.

## Core abstractions

The C `minipro_handle_t` vtable becomes a trait; drivers implement it and are
held as `Box<dyn Programmer>` (dispatch mirrors the C `switch` on model).

```rust
pub trait Programmer {
    fn begin(&mut self, dev: &Device) -> Result<Session>;
    fn read_block (&mut self, s: &Session, req: &BlockReq) -> Result<Vec<u8>>;
    fn write_block(&mut self, s: &Session, req: &BlockReq, data: &[u8]) -> Result<()>;
    fn chip_id(&mut self, s: &Session) -> Result<ChipId>;
    fn erase(&mut self, s: &Session, kind: EraseKind) -> Result<()>;
    // fuses, calibration, jedec rows, pin_test, logic_test, firmware_update ...
    fn capabilities(&self) -> Caps;   // replaces the scattered *_only flags
}
```

`Device` (the chip descriptor, C's `device_t`) is a plain `#[derive(Deserialize)]`
struct; `Session` is an RAII guard (see below). Ops return `Result<_, Error>`,
not `int`.

## The transport trait — and the two bugs it should make unrepresentable

The C driver has a load-bearing comment: *an undrained `0xf0` response wedges the
device until a USB replug.* Encode that invariant in the type system.

```rust
pub trait Transport {
    fn send(&mut self, ep: Endpoint, buf: &[u8]) -> Result<()>;
    fn recv(&mut self, ep: Endpoint, len: usize) -> Result<Vec<u8>>;
    fn link_speed(&self) -> LinkSpeed;   // learned the hard way this matters
}

/// A command that declares its response length. You cannot get the ack
/// without consuming the response, so "forgot to drain EP81" can't compile.
#[must_use]
pub struct Command<'a> { op: u8, args: [u8; 7], resp_len: u16, tx: &'a mut dyn Transport }
impl Command<'_> { pub fn exec(self) -> Result<Response> { /* send, then recv resp_len */ } }
```

- **USB backend: `nusb`** (pure-Rust, async, no libusb C dependency) rather than
  `rusb`. Drops a C dep, improves cross-platform behavior, and gives real control
  over transfers.
- **Centralize the macOS lesson.** `link_speed()` surfaces SuperSpeed vs High
  Speed. We proved the SuperSpeed/U1-U2 failure is *unfixable in userspace*
  ([ch569-usb3-notes.md](ch569-usb3-notes.md)), so the tool shouldn't pretend —
  it should **detect and diagnose**: on macOS + SuperSpeed + T76, emit the
  "use a USB 2.0 cable" guidance instead of a bare `LIBUSB_ERROR_IO`.
- **Testability:** a `MockTransport` replays capture fixtures. The C T76 driver
  is *already* built from byte-captures ("replayed from the read5 capture") —
  formalize those as `#[test]` golden fixtures, turning today's tribal knowledge
  into a regression suite.

## The FPGA bitstream / firmware coupling

The T76 uploads a per-op Anlogic bitstream, and those bitstreams are
firmware-version-specific (our whole 00.1.07-vs-00.1.17 saga). Make it typed:

```rust
struct AlgorithmSet { firmware: FwVersion, algos: HashMap<AlgoName, Lazy<Bitstream>> }
```

- **Lazy + decompressed on demand** (`flate2`), not the C model of holding/freeing
  a raw buffer per op.
- **Refuse or loudly warn on firmware mismatch** at `begin()` — a typed
  `Error::FirmwareMismatch { device, algorithms }`, not C's soft `printf` warning
  that's easy to miss.

## Chip database

- Parse `infoic.xml` with `quick-xml` + `serde` for drop-in compatibility.
- **Add a compiled DB path:** a `build.rs` (or `minipro-db build`) that bakes the
  48 MB XML into a `postcard`/`bincode` blob `include_bytes!`'d into the binary —
  startup goes from "parse 48 MB XML" to "mmap + zero-copy." Keep XML as the
  editable source of truth.

## Error handling, CLI, ergonomics

- **Errors:** `thiserror` in the libs (typed: `Usb`, `Protocol`, `ChipIdMismatch`,
  `Verify`, `FirmwareMismatch`, `BadContact`), `anyhow` in the CLI.
- **CLI:** `clap` derive. Offer subcommands (`minipro read`, `write`, `erase`,
  `info`) with a thin flag-compat shim for the old `-p/-r/-w` muscle memory.
  `indicatif` for progress, `tracing` for `LIBUSB_DEBUG`-style diagnostics.
- **Bake in what we learned about trustworthy dumps:** make verification
  first-class — `read` can auto re-read and compare for stability, and a
  `--expect-checksum`/known-checksum hook (the Adaptec label-sum trick) turns
  "did the dump come out clean?" from manual forensics into a built-in check.
- **Pin-detect is advisory, not fatal** — return a typed `BadContact { pins }`
  warning; on old/oxidized chips the read may still be fine (exactly our
  AHA-1542CP case).

## Safety/correctness wins that fall out for free

| C hazard | Rust outcome |
|---|---|
| `free(bitstream)` on every error path | `Vec`/`Drop` — impossible to leak or double-free |
| `uint8_t msg[512]` + `format_int` offsets | typed packet structs + `byteorder`/`zerocopy` |
| undrained EP response wedges device | `#[must_use] Command` — can't skip the drain |
| 0/1 int returns, silent mismatches | `Result` + typed errors, `?` propagation |
| no tests | `MockTransport` + capture fixtures + `insta` snapshots |
| libusb/zlib pkg-config build pain | pure-Rust `nusb` + `flate2`, static binary |

## Migration path

Incremental, not a rewrite-in-one-shot:
1. `minipro-core` + `minipro-usb` + **T76 driver only** (the device we care about),
   validated against the capture fixtures and a real read.
2. Add TL866/T48/T56 drivers behind the same trait.
3. `minipro-ffi` `cdylib` to keep any `libminipro` consumers working.
4. DB tooling (`dumpic` equivalents) can stay as-is — same XML in/out.

## What I would *not* do

- Not async-first in the public API — a programmer CLI is inherently sequential;
  keep a blocking surface (`nusb` async under the hood via `block_on`).
- Not a new data-file format — XML compatibility keeps the whole DB ecosystem.
- Not a ground-up protocol rewrite — the C T76 RE is hard-won; port it
  faithfully, with the captures as tests, before improving it.
