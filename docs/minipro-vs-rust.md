# minipro (C) vs. minipro-rs (Rust) — comparison

*As of 2026-08-18. C = nmatt0's `t76-improvements` fork; Rust = the `minipro-rs/`
redesign and reimplementation in this repo. It now covers **T48 / T56 / T76**
(the TL866II+ and TL866A/CS drivers are not yet done).*

## At a glance

| Dimension | C minipro (`t76-improvements`) | Rust `minipro-rs` |
|---|---|---|
| **Programmers** | TL866CS/A, TL866II+, T48, T56, **T76** | **TL866II+ / T48 / T56 / T76** (TL866A/CS not yet; TL866II+ reference-only) |
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
| **Tests** | ~none | **203** (golden-packet + MockTransport wire tests) + 5 hardware tests; clippy-clean |
| **Chip detect** | `-a` SPI autodetect + id check | `detect` (id → DB match) + `autodetect` (SPI JEDEC id) |
| **Identity query** | fw / serial / mfg-date / device-code / voltage | fw / serial / mfg-date / device-code / voltage |
| **LOC** | ~19,500 (all programmers) | ~14,500 (T48 / T56 / T76) |
| **License** | GPL-3.0 | **MIT** (independent reimplementation of the wire facts) |

## Hardware validation (this repo, live T76 at USB 2.0)

| Operation | C | Rust |
|---|---|---|
| Enumerate / identify programmer | ✅ | ✅ (`info` → fw 00.1.07, High Speed, 5.1 V) |
| Detect chip by electronic id | ✅ | ✅ (`0x1E8C` → AT27C256R@DIP28) |
| **Read** (EPROM) | ✅ (produced the reference dumps) | ✅ **byte-identical**, stable over 10× |
| SPI autodetect (`autodetect`) | ✅ | ✅ path exercised live (uploads SPI25F, probes; matches `detect`) |
| Overcurrent detection / abort | ✅ | ✅ (read OVC status, aborts with a typed error) |
| Write (parallel EPROM) | ✅ | ✅ **bit-exact on an MX27C2000** — intended bits changed, nothing else |
| Write (other classes) / erase | ✅ | implemented, not hardware-run |
| NAND / eMMC | ✅ | implemented (byte-tested vs captures), not hardware-run |
| Logic test / firmware update | ✅ | implemented (logic wired end-to-end; both un-hardware-run) |

Only a T76 is available in this repo; the T48/T56 drivers are reference-only
(verified against byte-exact golden packets, not live silicon).

## Where each wins

**C is ahead on:** breadth (5 programmer families vs 3), maturity (years of use,
broad chip coverage exercised), and completeness (write/erase/NAND/eMMC all
field-proven). It remains the field-proven reference for the protocol's behavior.

**Rust is ahead on:** memory safety (no manual frees, no fixed-buffer overruns),
build/deploy (no libusb/zlib, single static binary — fixes the macOS pkg-config
pain), machine-usability (JSON/TUI, stable error codes), correctness guarantees
baked into types (the must-drain `Pending`, typed firmware mismatch), first-class
dump verification, and an actual test suite. Its T76 read path is proven
byte-exact on hardware.

## Honest bottom line

minipro-rs is **not a replacement** for the C tool today — it covers three of
the five programmer families (T48/T56/T76), and only T76 reads are
hardware-proven. It is a **safer, more ergonomic, better-tested reimplementation**
whose value is the
architecture (traits + typed transport), the agent/human/TUI output story, and
built-in verified reads — with T76 reads *and* parallel-EPROM writes now proven
bit-exact on hardware. Reaching parity means erase/NAND/eMMC on silicon and the
remaining programmer families behind the same traits. For the decision-level
retrospective, see [`design-decisions.md`](design-decisions.md).
