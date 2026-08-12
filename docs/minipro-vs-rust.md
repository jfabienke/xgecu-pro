# minipro (C) vs. minipro-rs (Rust) — comparison

*As of 2026-08-12. C = nmatt0's `t76-improvements` fork; Rust = the `minipro-rs/`
port in this repo. The Rust port is deliberately scoped to the **T76 only**.*

## At a glance

| Dimension | C minipro (`t76-improvements`) | Rust `minipro-rs` |
|---|---|---|
| **Programmers** | TL866CS/A, TL866II+, T48, T56, **T76** | **T76 only** (by design, for now) |
| **Language / safety** | C; manual `malloc`/`free`, fixed `uint8_t` buffers | Rust; `#![forbid(unsafe_code)]`, RAII, `Vec` |
| **USB backend** | libusb-1.0 (C library) | **nusb** (pure Rust — no libusb) |
| **Compression** | zlib (C library) | flate2 / miniz_oxide (pure Rust) |
| **External C deps** | libusb + zlib via pkg-config | **none** (pure-Rust crate graph) |
| **Build** | `make` + pkg-config (needs `PKG_CONFIG_PATH` help on macOS) | `cargo`; static musl binary on Linux |
| **Output modes** | one human console | **human / JSON (NDJSON) / TUI** (ratatui) |
| **Machine-readable** | text only (must be scraped) | stable JSON `code`s; agent-ergonomic |
| **Errors** | `int` 0/1 returns; some silent | typed `Result` + `thiserror`, `code()` + `hint()` |
| **Verification** | manual (single read) | **`read_verified`**: two-read stability + crc32/sha256 in the outcome |
| **Wedge safety** | "drain the response" is a code comment | **type-enforced** `#[must_use] Pending` |
| **Tests** | ~none | **107** (incl. MockTransport wire tests); clippy-clean |
| **Chip detect** | `-a` SPI autodetect + id check | `detect` cmd: id → DB match + JEDEC vendor |
| **Identity query** | fw / serial / mfg-date / device-code / voltage | fw / serial / voltage (date & code not surfaced yet) |
| **LOC** | ~19,500 (all programmers) | ~6,000 (T76 slice) |
| **License** | GPL-3.0 | GPL-3.0 (same) |

## Hardware validation (this repo, live T76 at USB 2.0)

| Operation | C | Rust |
|---|---|---|
| Enumerate / identify programmer | ✅ | ✅ (`info` → fw 00.1.07, 5.1 V) |
| Detect chip by electronic id | ✅ | ✅ (`0x1E8C` → AT27C256R@DIP28) |
| **Read** (EPROM) | ✅ (produced the reference dumps) | ✅ **byte-identical**, stable over 10× |
| Write / erase | ✅ | **not yet exercised on hardware** (ported, untested) |
| NAND / eMMC | ✅ | ported (byte-tested vs captures), not hardware-run |

## Where each wins

**C is ahead on:** breadth (5 programmer families vs 1), maturity (years of use,
broad chip coverage exercised), and completeness (write/erase/NAND/eMMC all
field-proven). It is the reference the Rust port is checked against.

**Rust is ahead on:** memory safety (no manual frees, no fixed-buffer overruns),
build/deploy (no libusb/zlib, single static binary — fixes the macOS pkg-config
pain), machine-usability (JSON/TUI, stable error codes), correctness guarantees
baked into types (the must-drain `Pending`, typed firmware mismatch), first-class
dump verification, and an actual test suite. Its T76 read path is proven
byte-exact on hardware.

## Honest bottom line

The Rust port is **not a replacement** for the C tool today — it does one
device, and only reads are hardware-proven. It is a **safer, more ergonomic,
better-tested reimplementation of the T76 slice** whose value is the
architecture (traits + typed transport), the agent/human/TUI output story, and
built-in verified reads. Reaching parity means porting write/erase to hardware
validation and the other programmer families behind the same traits.
